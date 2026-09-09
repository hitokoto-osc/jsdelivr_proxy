//! 进程内资源缓存。
//!
//! 早期版本把 jsDelivr 资源缓存在 Redis 里（`mime` / `data` 两个键，TTL 2 小时），
//! 这让一个纯粹的只读反代必须额外维护一个有状态组件。现在改为进程内缓存
//! （[`moka::future::Cache`]），TTL 语义保持不变，服务本身不再有任何外部依赖。
//!
//! 需要注意的两点权衡：
//!
//! * moka 的淘汰策略是 **W-TinyLFU**（带准入过滤的近似 LRU 算法），
//!   不是严格的 LRU；对反代这种热点集中的场景命中率通常优于 LRU。
//! * 缓存位于进程内：重启即丢失，多副本部署时各副本各自持有一份缓存，
//!   不再共享。回源到 jsDelivr 是幂等的，所以这只影响回源次数，不影响正确性。
//!
//! Bodies are stored compressed, with the codec picked at startup from
//! `[cache] compression` ([`Compression`], zstd by default). That trades CPU on
//! every cache *hit* — a hit now decompresses into a fresh allocation instead of
//! handing back a refcounted slice — for several times more entries within the
//! same byte budget. Both run inline on the async task rather than on a blocking
//! pool: at these codec settings a typical jsDelivr asset stays well under a
//! millisecond, which is cheaper than a trip through `spawn_blocking`.

pub mod purge;

use crate::conf::cache::{Cache as CacheConfig, Compression};
use crate::CONFIG;
use brotli::enc::BrotliEncoderParams;
use bytes::Bytes;
use moka::future::Cache as MokaCache;
use serde::Serialize;
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, warn};

lazy_static! {
    static ref CACHE: ResourceCache = ResourceCache::new(&CONFIG.cache);
}

// TODO: make the compression level configurable, along with zstd's dictionary
// support (a trained dictionary pays off on a corpus of many small similar
// files, which is exactly what a package CDN serves).
const ZSTD_LEVEL: i32 = zstd::DEFAULT_COMPRESSION_LEVEL;

/// brotli defaults to quality 11, whose throughput is single-digit MB/s and far
/// too slow to sit on the fetch path. Quality 5 keeps a gzip-class ratio at
/// roughly two orders of magnitude more throughput.
// TODO: make quality and window size configurable.
const BROTLI_QUALITY: i32 = 5;
const BROTLI_LGWIN: i32 = 22;

/// 一条缓存的上游资源：Content-Type 与响应体。
///
/// `Bytes` 的克隆是引用计数级别的浅拷贝，因此从缓存取值不会复制文件内容。
#[derive(Clone, Debug)]
pub struct CachedResource {
    pub mime: String,
    pub data: Bytes,
}

#[derive(Debug, Error)]
pub enum CacheError<E> {
    /// moka hands the same `Arc` to every request coalesced onto one fetch.
    #[error("{0}")]
    Fetch(Arc<E>),
    /// Only reachable if a stored body is corrupt: this process compressed it
    /// itself moments earlier.
    #[error("failed to decompress a cached resource: {0}")]
    Decompress(#[source] io::Error),
}

/// What the cache actually holds: `body` is the response body encoded with
/// `compression`, which is [`Compression::None`] whenever the bytes are stored
/// verbatim — see [`CacheEntry::encode`].
#[derive(Clone, Debug)]
struct CacheEntry {
    body: Bytes,
    compression: Compression,
    /// Decompressed length, so decoding can size its buffer in one allocation.
    raw_len: usize,
}

impl CacheEntry {
    /// Stores the body verbatim when compression fails or fails to shrink it.
    /// jsDelivr serves plenty of already-compressed assets (woff2, png, wasm);
    /// without this fallback, enabling a codec could make the cache hold *less*
    /// than it did before.
    fn encode(data: Bytes, codec: Compression) -> Self {
        let raw_len = data.len();
        let compressed = match compress(&data, codec) {
            Ok(Some(body)) if body.len() < raw_len => Some(Bytes::from(body)),
            Ok(_) => None,
            Err(e) => {
                warn!(error = %e, "compression failed, caching the resource verbatim");
                None
            }
        };
        match compressed {
            Some(body) => CacheEntry {
                body,
                compression: codec,
                raw_len,
            },
            None => CacheEntry {
                body: data,
                compression: Compression::None,
                raw_len,
            },
        }
    }

    fn decode(&self, mime: String) -> io::Result<CachedResource> {
        let data = match self.compression {
            Compression::None => self.body.clone(),
            Compression::Zstd => Bytes::from(zstd::bulk::decompress(&self.body, self.raw_len)?),
            Compression::Brotli => {
                let mut out = Vec::with_capacity(self.raw_len);
                brotli::BrotliDecompress(&mut self.body.as_ref(), &mut out)?;
                Bytes::from(out)
            }
        };
        Ok(CachedResource { mime, data })
    }
}

fn compress(data: &[u8], codec: Compression) -> io::Result<Option<Vec<u8>>> {
    match codec {
        Compression::None => Ok(None),
        Compression::Zstd => zstd::bulk::compress(data, ZSTD_LEVEL).map(Some),
        Compression::Brotli => {
            let params = BrotliEncoderParams {
                quality: BROTLI_QUALITY,
                lgwin: BROTLI_LGWIN,
                size_hint: data.len(),
                ..Default::default()
            };
            let mut out = Vec::new();
            brotli::BrotliCompress(&mut &data[..], &mut out, &params)?;
            Ok(Some(out))
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum CacheKey {
    Path(String),
    Body(blake3::Hash),
}

#[derive(Clone, Debug)]
enum CacheValue {
    Path {
        mime: String,
        checksum: blake3::Hash,
    },
    Body(CacheEntry),
}

impl CacheValue {
    fn weight(&self, key: &CacheKey) -> usize {
        match (key, self) {
            (CacheKey::Path(path), Self::Path { mime, .. }) => {
                path.len().saturating_add(mime.len()).saturating_add(32)
            }
            (CacheKey::Body(_), Self::Body(body)) => 32usize.saturating_add(body.body.len()),
            _ => unreachable!("cache key and value types must match"),
        }
    }
}

pub struct ResourceCache {
    inner: MokaCache<CacheKey, CacheValue>,
    /// 单条目字节上限：超过则不缓存（但仍然正常返回给客户端）。
    /// Measured on the stored body, i.e. after compression, so the limit caps
    /// what the entry costs rather than how big the file was upstream.
    max_entry_size: usize,
    compression: Compression,
    /// Kept only so that [`ResourceCache::stats`] can report the limits this
    /// cache was actually built with, which is not necessarily what `CONFIG`
    /// says: tests build caches with their own parameters.
    ttl_secs: u64,
    max_capacity_bytes: u64,
}

/// Aggregate figures for the admin panel. Byte counts unless noted.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CacheStats {
    pub entry_count: u64,
    pub body_count: u64,
    pub body_stored_bytes: u64,
    pub orphan_body_count: u64,
    pub orphan_stored_bytes: u64,
    pub deduplicated_bytes: u64,
    /// Total as stored, i.e. after compression. This is what counts against
    /// `max_capacity_bytes`.
    pub stored_bytes: u64,
    /// Counts each retained body once, including bodies whose paths were purged,
    /// so deduplication does not inflate the reported compression ratio.
    pub raw_bytes: u64,
    pub ttl_secs: u64,
    pub max_capacity_bytes: u64,
    pub max_entry_size_bytes: usize,
    pub compression: Compression,
}

/// One row of the cache listing. Carries sizes only; no body is decompressed
/// to produce it.
#[derive(Debug, Clone, Serialize)]
pub struct CacheEntryInfo {
    pub key: String,
    pub checksum: String,
    pub shared_paths: u64,
    pub mime: String,
    pub stored_bytes: usize,
    pub raw_bytes: usize,
    pub compression: Compression,
}

impl ResourceCache {
    pub fn new(config: &CacheConfig) -> Self {
        Self::with_params(
            config.ttl_secs,
            config.max_capacity_bytes(),
            config.max_entry_size_bytes(),
            config.compression,
        )
    }

    /// `max_capacity_bytes` 是字节预算：moka 的 `max_capacity` 单位是 weigher
    /// 返回的「权重」，这里 weigher 返回条目的字节数，因此容量即字节数。
    /// 若不设置 weigher，`max_capacity` 的单位会退化成「条目数」。
    pub fn with_params(
        ttl_secs: u64,
        max_capacity_bytes: u64,
        max_entry_size: usize,
        compression: Compression,
    ) -> Self {
        let inner = MokaCache::builder()
            .time_to_live(Duration::from_secs(ttl_secs))
            .max_capacity(max_capacity_bytes)
            .weigher(|key: &CacheKey, value: &CacheValue| -> u32 {
                value.weight(key).try_into().unwrap_or(u32::MAX)
            })
            .build();
        ResourceCache {
            inner,
            max_entry_size,
            compression,
            ttl_secs,
            max_capacity_bytes,
        }
    }

    // Path and body entries share one byte budget, but each write starts only
    // that entry's TTL. Sharing content must never renew another path.
    async fn prepare(&self, key: &str, resource: CachedResource) -> (CacheValue, CacheEntry, bool) {
        let checksum = blake3::hash(&resource.data);
        let path = CacheValue::Path {
            mime: resource.mime,
            checksum,
        };
        let body_key = CacheKey::Body(checksum);
        let value = match self.inner.get(&body_key).await {
            Some(value) => value,
            None => CacheValue::Body(CacheEntry::encode(resource.data, self.compression)),
        };
        let size = path.weight(&CacheKey::Path(key.to_string())) + value.weight(&body_key);
        let cacheable = size <= self.max_entry_size;
        if cacheable {
            // A new alias must not outlive the content it has just validated.
            let value = self
                .inner
                .get_with(body_key.clone(), async { value.clone() })
                .await;
            self.inner.insert(body_key, value.clone()).await;
            let CacheValue::Body(body) = value else {
                unreachable!()
            };
            return (path, body, true);
        }
        debug!(
            key,
            size,
            limit = self.max_entry_size,
            "resource exceeds per-entry cache limit, not cached"
        );
        let CacheValue::Body(body) = value else {
            unreachable!()
        };
        (path, body, false)
    }

    pub async fn get_or_fetch<F, E>(
        &self,
        key: String,
        init: F,
    ) -> Result<CachedResource, CacheError<E>>
    where
        F: Future<Output = Result<CachedResource, E>>,
        E: Send + Sync + 'static,
    {
        let path_key = CacheKey::Path(key.clone());
        let mut init = Some(init);
        loop {
            let mut fetched = None;
            let entry = self
                .inner
                .entry(path_key.clone())
                .or_try_insert_with(async {
                    let resource = init.take().expect("a request fetches at most once").await?;
                    let (path, body, cacheable) = self.prepare(&key, resource).await;
                    fetched = Some((body, cacheable));
                    Ok::<_, E>(path)
                })
                .await
                .map_err(CacheError::Fetch)?;
            let CacheValue::Path { mime, checksum } = entry.into_value() else {
                unreachable!()
            };
            if let Some((body, cacheable)) = fetched {
                if !cacheable {
                    self.inner.invalidate(&path_key).await;
                }
                return body.decode(mime).map_err(CacheError::Decompress);
            }
            if let Some(CacheValue::Body(body)) = self.inner.get(&CacheKey::Body(checksum)).await {
                return body.decode(mime).map_err(CacheError::Decompress);
            }
            // Capacity eviction can remove content before its path expires.
            self.inner.invalidate(&path_key).await;
        }
    }

    /// Preload refresh must replace the mapping and restart its TTL even on a
    /// hit; ordinary requests through `get_or_fetch` must not do either.
    pub async fn insert(&self, key: String, value: CachedResource) -> bool {
        let (path, _, cacheable) = self.prepare(&key, value).await;
        if cacheable {
            self.inner.insert(CacheKey::Path(key), path).await;
        }
        cacheable
    }

    /// Preload can validate an unchanged resource without decoding or hashing
    /// its cached body. A mapping alone is insufficient after body eviction.
    pub async fn renew(&self, key: &str) -> bool {
        let path_key = CacheKey::Path(key.to_string());
        if let Some(path @ CacheValue::Path { checksum, .. }) = self.inner.get(&path_key).await {
            let body_key = CacheKey::Body(checksum);
            if let Some(body) = self.inner.get(&body_key).await {
                self.inner.insert(body_key, body).await;
                self.inner.insert(path_key, path).await;
                return true;
            }
        }
        false
    }

    #[cfg(test)]
    async fn get(&self, key: &str) -> Option<CacheEntry> {
        let CacheValue::Path { checksum, .. } =
            self.inner.get(&CacheKey::Path(key.to_string())).await?
        else {
            unreachable!()
        };
        match self.inner.get(&CacheKey::Body(checksum)).await? {
            CacheValue::Body(body) => Some(body),
            _ => unreachable!(),
        }
    }

    /// Per-entry byte limit, which preload uses to skip hopeless files
    /// before downloading them.
    pub fn max_entry_size(&self) -> usize {
        self.max_entry_size
    }

    // Drain eviction work before sampling so the panel's occupancy reflects
    // capacity maintenance. Concurrent writes can still change this sample.
    pub async fn stats(&self) -> CacheStats {
        self.inner.run_pending_tasks().await;
        let snapshot: Vec<_> = self.inner.iter().collect();
        let mut references = std::collections::HashMap::<_, u64>::new();
        for (_, value) in &snapshot {
            if let CacheValue::Path { checksum, .. } = value {
                *references.entry(*checksum).or_default() += 1;
            }
        }
        let mut entry_count = 0;
        let mut body_count = 0;
        let mut body_stored_bytes = 0;
        let mut orphan_body_count = 0;
        let mut orphan_stored_bytes = 0;
        let mut deduplicated_bytes = 0;
        let mut stored_bytes = 0;
        let mut raw_bytes = 0;
        for (key, value) in &snapshot {
            stored_bytes += value.weight(key) as u64;
            if let (CacheKey::Body(checksum), CacheValue::Body(body)) = (key.as_ref(), value) {
                let paths = references.get(checksum).copied().unwrap_or(0);
                let size = body.body.len() as u64;
                entry_count += paths;
                body_count += 1;
                body_stored_bytes += size;
                raw_bytes += body.raw_len as u64;
                deduplicated_bytes += paths.saturating_sub(1) * size;
                if paths == 0 {
                    orphan_body_count += 1;
                    orphan_stored_bytes += value.weight(key) as u64;
                }
            }
        }
        CacheStats {
            entry_count,
            body_count,
            body_stored_bytes,
            orphan_body_count,
            orphan_stored_bytes,
            deduplicated_bytes,
            stored_bytes,
            raw_bytes,
            ttl_secs: self.ttl_secs,
            max_capacity_bytes: self.max_capacity_bytes,
            max_entry_size_bytes: self.max_entry_size,
            compression: self.compression,
        }
    }

    /// Every cached entry matching `prefix`, largest first.
    pub fn entries(&self, prefix: Option<&str>) -> Vec<CacheEntryInfo> {
        let snapshot: Vec<_> = self.inner.iter().collect();
        let mut references = std::collections::HashMap::<_, u64>::new();
        for (_, value) in &snapshot {
            if let CacheValue::Path { checksum, .. } = value {
                *references.entry(*checksum).or_default() += 1;
            }
        }
        let bodies: std::collections::HashMap<_, _> = snapshot
            .iter()
            .filter_map(|(key, value)| match (key.as_ref(), value) {
                (CacheKey::Body(checksum), CacheValue::Body(body)) => Some((*checksum, body)),
                _ => None,
            })
            .collect();
        let mut entries: Vec<CacheEntryInfo> = snapshot
            .iter()
            .filter_map(|(key, value)| {
                let (CacheKey::Path(path), CacheValue::Path { mime, checksum }) =
                    (key.as_ref(), value)
                else {
                    return None;
                };
                if prefix.is_some_and(|prefix| !path.starts_with(prefix)) {
                    return None;
                }
                let body = bodies.get(checksum)?;
                Some(CacheEntryInfo {
                    key: path.clone(),
                    checksum: checksum.to_hex().to_string(),
                    shared_paths: references[checksum],
                    stored_bytes: path.len() + mime.len() + 64 + body.body.len(),
                    raw_bytes: body.raw_len,
                    mime: mime.clone(),
                    compression: body.compression,
                })
            })
            .collect();
        entries.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.stored_bytes));
        entries
    }

    /// Removes one entry, reporting whether it was there to begin with.
    pub async fn invalidate_key(&self, key: &str) -> bool {
        let key = CacheKey::Path(key.to_string());
        let existed = self.inner.contains_key(&key);
        self.inner.invalidate(&key).await;
        existed
    }

    /// Removes every entry whose key starts with `prefix`, returning how many
    /// were removed.
    ///
    /// This collects the matching keys and invalidates them individually
    /// instead of using moka's `invalidate_entries_if`, which would require
    /// `support_invalidation_closures()` on the builder — extra bookkeeping on
    /// every insert — and only takes effect during later maintenance. One pass
    /// over the keys at this cache's scale is the cheaper trade, and it purges
    /// immediately.
    pub async fn invalidate_prefix(&self, prefix: &str) -> usize {
        let keys: Vec<String> = self
            .inner
            .iter()
            .filter_map(|(key, _)| match key.as_ref() {
                CacheKey::Path(path) => Some(path.clone()),
                _ => None,
            })
            .filter(|key| key.starts_with(prefix))
            .collect();
        for key in &keys {
            self.inner.invalidate(&CacheKey::Path(key.clone())).await;
        }
        keys.len()
    }

    /// Empties the cache, returning how many entries were dropped.
    pub async fn clear(&self) -> u64 {
        self.inner.run_pending_tasks().await;
        let removed = self
            .inner
            .iter()
            .filter(|(key, _)| matches!(key.as_ref(), CacheKey::Path(_)))
            .count() as u64;
        // `invalidate_all` is lazy; draining again makes the count reported to
        // the caller match what a listing will show straight afterwards.
        self.inner.invalidate_all();
        self.inner.run_pending_tasks().await;
        removed
    }

    #[cfg(test)]
    async fn run_pending_tasks(&self) {
        self.inner.run_pending_tasks().await;
    }
}

/// 全局缓存实例上的 [`ResourceCache::get_or_fetch`]。
pub async fn get_or_fetch<F, E>(key: String, init: F) -> Result<CachedResource, CacheError<E>>
where
    F: Future<Output = Result<CachedResource, E>>,
    E: Send + Sync + 'static,
{
    CACHE.get_or_fetch(key, init).await
}

pub async fn insert(key: String, value: CachedResource) -> bool {
    CACHE.insert(key, value).await
}

pub async fn renew(key: &str) -> bool {
    CACHE.renew(key).await
}

pub fn max_entry_size() -> usize {
    CACHE.max_entry_size()
}

pub async fn stats() -> CacheStats {
    CACHE.stats().await
}

pub fn entries(prefix: Option<&str>) -> Vec<CacheEntryInfo> {
    CACHE.entries(prefix)
}

pub async fn invalidate_key(key: &str) -> bool {
    CACHE.invalidate_key(key).await
}

pub async fn invalidate_prefix(prefix: &str) -> usize {
    CACHE.invalidate_prefix(prefix).await
}

pub async fn clear() -> u64 {
    CACHE.clear().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn resource(mime: &str, len: usize) -> CachedResource {
        CachedResource {
            mime: mime.to_string(),
            data: Bytes::from(vec![b'x'; len]),
        }
    }

    /// Deterministic pseudorandom bytes; no codec can shrink these, which is
    /// what the verbatim-storage fallback needs in order to be exercised.
    fn incompressible(len: usize) -> Bytes {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push((state >> 33) as u8);
        }
        Bytes::from(out)
    }

    #[tokio::test]
    async fn aliases_share_one_body_and_keep_their_own_mime() {
        for codec in [Compression::None, Compression::Zstd, Compression::Brotli] {
            let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024, codec);
            let tag = "gh/o/r@v1/file";
            let head = "gh/o/r@HEAD/file";
            cache.insert(tag.into(), resource("text/plain", 4096)).await;
            cache
                .insert(head.into(), resource("application/javascript", 4096))
                .await;
            let first = cache.get(tag).await.unwrap();
            let second = cache.get(head).await.unwrap();
            assert_eq!(first.body.as_ptr(), second.body.as_ptr());
            let stats = cache.stats().await;
            assert_eq!(stats.entry_count, 2);
            assert_eq!(stats.raw_bytes, 4096);
            assert_eq!(
                stats.stored_bytes as usize,
                tag.len()
                    + head.len()
                    + "text/plain".len()
                    + "application/javascript".len()
                    + 96
                    + first.body.len()
            );
            for (key, mime) in [(tag, "text/plain"), (head, "application/javascript")] {
                let result = cache
                    .get_or_fetch::<_, Infallible>(key.into(), async {
                        panic!("alias should hit the shared body")
                    })
                    .await
                    .unwrap();
                assert_eq!(result.mime, mime);
                assert_eq!(result.data, resource(mime, 4096).data);
            }
        }
    }

    #[tokio::test]
    async fn observability_distinguishes_shared_and_unreferenced_bodies() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024, Compression::None);
        let tag = "gh/o/r@v1/file";
        let head = "gh/o/r@HEAD/file";
        cache.insert(tag.into(), resource("text/plain", 1024)).await;
        cache
            .insert(head.into(), resource("text/plain", 1024))
            .await;
        let stats = cache.stats().await;
        assert_eq!(stats.entry_count, 2);
        assert_eq!(stats.body_count, 1);
        assert_eq!(stats.body_stored_bytes, 1024);
        assert_eq!(stats.deduplicated_bytes, 1024);
        assert_eq!(stats.orphan_body_count, 0);
        let filtered = cache.entries(Some("gh/o/r@HEAD"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].shared_paths, 2);
        assert_eq!(
            filtered[0].checksum,
            blake3::hash(&resource("text/plain", 1024).data)
                .to_hex()
                .to_string()
        );

        cache.invalidate_key(tag).await;
        let stats = cache.stats().await;
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.deduplicated_bytes, 0);
        assert_eq!(stats.orphan_body_count, 0);
        assert_eq!(cache.entries(None)[0].shared_paths, 1);

        cache.invalidate_key(head).await;
        let stats = cache.stats().await;
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.body_count, 1);
        assert_eq!(stats.orphan_body_count, 1);
        assert_eq!(stats.orphan_stored_bytes, 1056);
        assert_eq!(stats.stored_bytes, stats.orphan_stored_bytes);
        cache.clear().await;
        let stats = cache.stats().await;
        assert_eq!(stats.body_count, 0);
        assert_eq!(stats.body_stored_bytes, 0);
        assert_eq!(stats.orphan_body_count, 0);
        assert_eq!(stats.orphan_stored_bytes, 0);
    }

    #[tokio::test]
    async fn renewing_one_alias_does_not_renew_another() {
        let cache = ResourceCache::with_params(2, 1024 * 1024, 1024 * 1024, Compression::None);
        cache
            .insert("tag".into(), resource("text/plain", 128))
            .await;
        cache
            .insert("HEAD".into(), resource("text/plain", 128))
            .await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(cache.renew("HEAD").await);
        tokio::time::sleep(Duration::from_millis(1000)).await;
        assert!(cache.get("tag").await.is_none());
        assert!(cache.get("HEAD").await.is_some());
        let result = cache
            .get_or_fetch::<_, Infallible>("tag".into(), async { Ok(resource("text/plain", 256)) })
            .await
            .unwrap();
        assert_eq!(result.data.len(), 256);
        assert_eq!(cache.get("HEAD").await.unwrap().raw_len, 128);
    }

    #[tokio::test]
    async fn replacing_and_purging_aliases_preserves_other_paths() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024, Compression::None);
        for key in ["tag", "HEAD", "branch/main"] {
            cache.insert(key.into(), resource("text/plain", 128)).await;
        }
        cache
            .insert("HEAD".into(), resource("text/plain", 256))
            .await;
        assert_eq!(cache.get("tag").await.unwrap().raw_len, 128);
        assert!(cache.invalidate_key("tag").await);
        assert_eq!(cache.get("branch/main").await.unwrap().raw_len, 128);
        assert_eq!(cache.invalidate_prefix("branch/").await, 1);
        assert_eq!(cache.get("HEAD").await.unwrap().raw_len, 256);
        assert_eq!(cache.clear().await, 1);
        assert_eq!(cache.stats().await.stored_bytes, 0);
    }

    #[tokio::test]
    async fn missing_shared_body_refetches_and_cannot_be_renewed() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024, Compression::None);
        let original = resource("text/plain", 128);
        let checksum = blake3::hash(&original.data);
        cache.insert("HEAD".into(), original).await;
        cache.inner.invalidate(&CacheKey::Body(checksum)).await;
        assert!(!cache.renew("HEAD").await);
        assert!(cache.entries(None).is_empty());
        let result = cache
            .get_or_fetch::<_, Infallible>("HEAD".into(), async { Ok(resource("text/plain", 256)) })
            .await
            .unwrap();
        assert_eq!(result.data.len(), 256);
    }

    #[tokio::test]
    async fn concurrent_misses_still_share_one_fetch() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024, Compression::Zstd);
        let calls = AtomicUsize::new(0);
        let fetch = || {
            cache.get_or_fetch::<_, Infallible>("HEAD".into(), async {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok(resource("text/plain", 4096))
            })
        };
        let (a, b, c) = tokio::join!(fetch(), fetch(), fetch());
        assert_eq!(a.unwrap().data, b.unwrap().data);
        assert_eq!(c.unwrap().data.len(), 4096);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_aliases_converge_on_one_stored_body() {
        let cache = Arc::new(ResourceCache::with_params(
            60,
            1024 * 1024,
            1024 * 1024,
            Compression::Zstd,
        ));
        let barrier = Arc::new(tokio::sync::Barrier::new(16));
        let mut tasks = Vec::new();
        for i in 0..16 {
            let cache = cache.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                cache
                    .get_or_fetch::<_, Infallible>(format!("alias/{i}"), async {
                        Ok(resource("text/plain", 4096))
                    })
                    .await
                    .unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let body = cache.get("alias/0").await.unwrap();
        for i in 1..16 {
            assert_eq!(
                cache
                    .get(&format!("alias/{i}"))
                    .await
                    .unwrap()
                    .body
                    .as_ptr(),
                body.body.as_ptr()
            );
        }
        assert_eq!(cache.stats().await.raw_bytes, 4096);
    }

    #[tokio::test]
    async fn metadata_and_shared_bodies_obey_one_capacity_limit() {
        let cache = ResourceCache::with_params(60, 1024, 1024, Compression::None);
        for i in 0..100 {
            cache
                .insert(format!("path/{i}"), resource("text/plain", 512))
                .await;
        }
        let stats = cache.stats().await;
        assert!(stats.stored_bytes <= 1024);
    }

    #[tokio::test]
    async fn unreferenced_bodies_expire() {
        let cache = ResourceCache::with_params(1, 1024 * 1024, 1024 * 1024, Compression::None);
        cache
            .insert("tag".into(), resource("text/plain", 128))
            .await;
        cache.invalidate_key("tag").await;
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(cache.stats().await.stored_bytes, 0);
    }

    /// 命中缓存时不应再次调用 loader。
    #[tokio::test]
    async fn cache_hit_does_not_reinvoke_loader() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024, Compression::None);
        let calls = AtomicUsize::new(0);

        for _ in 0..3 {
            let value = cache
                .get_or_fetch::<_, Infallible>("npm/vue@3/dist/vue.global.js".to_string(), async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(resource("application/javascript", 16))
                })
                .await
                .unwrap();
            assert_eq!(value.mime, "application/javascript");
            assert_eq!(value.data.len(), 16);
        }

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// TTL 到期后条目应被淘汰，loader 会被重新调用。
    #[tokio::test]
    async fn entry_expires_after_ttl() {
        let cache = ResourceCache::with_params(1, 1024 * 1024, 1024 * 1024, Compression::None);
        let calls = AtomicUsize::new(0);
        let load = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(resource("text/plain", 8))
        };

        cache
            .get_or_fetch("npm/lodash".to_string(), load())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        tokio::time::sleep(Duration::from_millis(1_200)).await;
        cache.run_pending_tasks().await;
        assert!(cache.get("npm/lodash").await.is_none());

        cache
            .get_or_fetch("npm/lodash".to_string(), load())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// 超过单条目上限的资源仍然正常返回，但不会留在缓存里。
    #[tokio::test]
    async fn oversized_entry_is_not_cached() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 64, Compression::None);
        let calls = AtomicUsize::new(0);
        let load = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(resource("application/octet-stream", 128))
        };

        let value = cache
            .get_or_fetch("npm/big@1/big.bin".to_string(), load())
            .await
            .unwrap();
        assert_eq!(value.data.len(), 128, "超限资源仍应正常返回给调用方");

        cache.run_pending_tasks().await;
        assert!(cache.get("npm/big@1/big.bin").await.is_none());
        assert_eq!(cache.stats().await.stored_bytes, 0);

        // 未被缓存 => 下一次请求必须重新回源
        cache
            .get_or_fetch("npm/big@1/big.bin".to_string(), load())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// 恰好等于上限的资源应当被缓存（边界条件）。
    #[tokio::test]
    async fn entry_at_size_limit_is_cached() {
        let key = "npm/edge@1/edge.js".to_string();
        let mime = "application/javascript";
        let limit = key.len() + mime.len() + 64 + 32;
        let cache = ResourceCache::with_params(60, 1024 * 1024, limit, Compression::None);

        cache
            .get_or_fetch::<_, Infallible>(key.clone(), async { Ok(resource(mime, 32)) })
            .await
            .unwrap();

        cache.run_pending_tasks().await;
        assert!(cache.get(&key).await.is_some());
    }

    #[tokio::test]
    async fn insert_overwrites_the_existing_entry() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024, Compression::None);
        let key = "gh/o/r@HEAD/x.json".to_string();

        cache
            .get_or_fetch::<_, Infallible>(key.clone(), async {
                Ok(resource("application/json", 8))
            })
            .await
            .unwrap();
        assert!(
            cache
                .insert(key.clone(), resource("application/json", 32))
                .await
        );

        let value = cache.get(&key).await.expect("entry should still be there");
        assert_eq!(value.body.len(), 32);
    }

    /// Without a TTL reset the periodic preload refresh would be a no-op for
    /// entries that are still cached.
    #[tokio::test]
    async fn insert_resets_the_ttl() {
        let cache = ResourceCache::with_params(2, 1024 * 1024, 1024 * 1024, Compression::None);
        let key = "gh/o/r@HEAD/x.json".to_string();

        cache
            .insert(key.clone(), resource("application/json", 8))
            .await;
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        cache
            .insert(key.clone(), resource("application/json", 8))
            .await;

        tokio::time::sleep(Duration::from_millis(1_000)).await;
        cache.run_pending_tasks().await;
        assert!(
            cache.get(&key).await.is_some(),
            "only 1s since the last write, so the renewed entry must survive"
        );
    }

    #[tokio::test]
    async fn insert_rejects_oversized_entries() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 64, Compression::None);
        let key = "npm/big@1/big.bin".to_string();

        assert!(
            !cache
                .insert(key.clone(), resource("application/octet-stream", 128))
                .await
        );
        cache.run_pending_tasks().await;
        assert!(cache.get(&key).await.is_none());
    }

    /// Renewing must reset the TTL while leaving the stored body untouched, so
    /// that preload's no-refetch path costs no compression work.
    #[tokio::test]
    async fn renew_resets_the_ttl_without_re_encoding() {
        let cache = ResourceCache::with_params(2, 1024 * 1024, 1024 * 1024, Compression::Zstd);
        let key = "gh/o/r@HEAD/x.json".to_string();

        assert!(!cache.renew(&key).await, "nothing cached yet");
        cache
            .insert(key.clone(), resource("application/json", 4096))
            .await;
        let stored = cache.get(&key).await.expect("entry should be there");

        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert!(cache.renew(&key).await);

        tokio::time::sleep(Duration::from_millis(1_000)).await;
        cache.run_pending_tasks().await;
        let renewed = cache
            .get(&key)
            .await
            .expect("only 1s since the renewal, so the entry must survive");
        assert_eq!(renewed.compression, Compression::Zstd);
        assert_eq!(renewed.body, stored.body);
    }

    /// 回源失败不应被缓存。
    #[tokio::test]
    async fn errors_are_not_cached() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024, Compression::None);
        let calls = AtomicUsize::new(0);

        for _ in 0..2 {
            let result = cache
                .get_or_fetch::<_, &'static str>("npm/missing".to_string(), async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err("upstream 404")
                })
                .await;
            assert!(result.is_err());
        }

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Both codecs must shrink the stored body and hand callers back the exact
    /// bytes they were given, on the miss path and on the hit path alike.
    #[tokio::test]
    async fn compressed_entry_round_trips() {
        let body = Bytes::from("console.log('hello');".repeat(512));
        for codec in [Compression::Zstd, Compression::Brotli] {
            let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024, codec);
            let key = "npm/app@1/app.js".to_string();
            let calls = AtomicUsize::new(0);

            for _ in 0..2 {
                let value = cache
                    .get_or_fetch::<_, Infallible>(key.clone(), async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(CachedResource {
                            mime: "application/javascript".to_string(),
                            data: body.clone(),
                        })
                    })
                    .await
                    .unwrap();
                assert_eq!(value.mime, "application/javascript");
                assert_eq!(value.data, body, "{codec} must return the original bytes");
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);

            cache.run_pending_tasks().await;
            let stored = cache.get(&key).await.expect("entry stays cached");
            assert_eq!(stored.compression, codec);
            assert!(
                stored.body.len() < body.len(),
                "{codec} must shrink repetitive JS"
            );
        }
    }

    /// Enabling a codec must never make an entry cost more than it used to.
    #[tokio::test]
    async fn incompressible_body_is_stored_verbatim() {
        let body = incompressible(8 * 1024);
        for codec in [Compression::Zstd, Compression::Brotli] {
            let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024, codec);
            let key = "npm/pkg@1/font.woff2".to_string();

            let value = cache
                .get_or_fetch::<_, Infallible>(key.clone(), async {
                    Ok(CachedResource {
                        mime: "font/woff2".to_string(),
                        data: body.clone(),
                    })
                })
                .await
                .unwrap();
            assert_eq!(value.data, body, "{codec} must return the original bytes");

            cache.run_pending_tasks().await;
            let stored = cache.get(&key).await.expect("entry stays cached");
            assert_eq!(stored.compression, Compression::None);
            assert_eq!(stored.body.len(), body.len());
        }
    }

    /// The per-entry limit caps what an entry costs, so a resource that only
    /// fits once compressed is still cached.
    #[tokio::test]
    async fn entry_limit_is_measured_after_compression() {
        let key = "npm/big@1/big.js".to_string();
        let body = Bytes::from(vec![b'x'; 64 * 1024]);
        let limit = 4 * 1024;
        let cache = ResourceCache::with_params(60, 1024 * 1024, limit, Compression::Zstd);

        cache
            .get_or_fetch::<_, Infallible>(key.clone(), async {
                Ok(CachedResource {
                    mime: "application/javascript".to_string(),
                    data: body.clone(),
                })
            })
            .await
            .unwrap();

        cache.run_pending_tasks().await;
        let stored = cache
            .get(&key)
            .await
            .expect("compressed body fits the limit");
        assert!(key.len() + "application/javascript".len() + 64 + stored.body.len() <= limit);
        assert!(
            stored.raw_len > limit,
            "the raw body would have been rejected"
        );
    }

    /// Populates a cache with three keys under two different prefixes.
    async fn populated() -> ResourceCache {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024, Compression::None);
        for (key, len) in [
            ("npm/vue@3/dist/vue.js", 300),
            ("npm/vue@3/dist/vue.css", 100),
            ("gh/o/r@HEAD/data.json", 200),
        ] {
            cache
                .insert(key.to_string(), resource("text/plain", len))
                .await;
        }
        cache.run_pending_tasks().await;
        cache
    }

    #[tokio::test]
    async fn stats_report_the_limits_the_cache_was_built_with() {
        let cache = populated().await;
        let stats = cache.stats().await;

        assert_eq!(stats.entry_count, 3);
        assert_eq!(stats.raw_bytes, 600);
        assert_eq!(stats.ttl_secs, 60);
        assert_eq!(stats.max_capacity_bytes, 1024 * 1024);
        assert_eq!(stats.compression, Compression::None);
        // Uncompressed bodies plus the key and MIME bytes of each entry.
        assert!(stats.stored_bytes > stats.raw_bytes);
    }

    #[tokio::test]
    async fn the_listing_is_ordered_by_size_and_filtered_by_prefix() {
        let cache = populated().await;

        let all = cache.entries(None);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].key, "npm/vue@3/dist/vue.js");
        assert!(all[0].stored_bytes >= all[1].stored_bytes);
        assert_eq!(all[0].raw_bytes, 300);

        let filtered = cache.entries(Some("npm/vue@3/"));
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|e| e.key.starts_with("npm/vue@3/")));
        assert!(cache.entries(Some("wp/")).is_empty());
    }

    #[tokio::test]
    async fn invalidating_a_key_reports_whether_it_was_cached() {
        let cache = populated().await;

        assert!(cache.invalidate_key("gh/o/r@HEAD/data.json").await);
        assert!(!cache.invalidate_key("gh/o/r@HEAD/data.json").await);
        assert!(!cache.invalidate_key("npm/never-cached").await);

        cache.run_pending_tasks().await;
        assert_eq!(cache.entries(None).len(), 2);
    }

    /// A prefix must match whole keys from the left, and must not take the
    /// siblings of the packages it names with it.
    #[tokio::test]
    async fn a_prefix_purge_removes_exactly_its_subtree() {
        let cache = populated().await;

        assert_eq!(cache.invalidate_prefix("npm/vue@3/").await, 2);
        cache.run_pending_tasks().await;

        let remaining = cache.entries(None);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].key, "gh/o/r@HEAD/data.json");
        assert_eq!(cache.invalidate_prefix("npm/").await, 0);
    }

    #[tokio::test]
    async fn clearing_reports_how_many_entries_went_away() {
        let cache = populated().await;

        assert_eq!(cache.clear().await, 3);
        assert!(cache.entries(None).is_empty());
        assert_eq!(cache.stats().await.entry_count, 0);
        assert_eq!(cache.clear().await, 0);
    }
}
