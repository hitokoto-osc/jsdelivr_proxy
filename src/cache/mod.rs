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

use crate::conf::cache::{Cache as CacheConfig, Compression};
use crate::CONFIG;
use brotli::enc::BrotliEncoderParams;
use bytes::Bytes;
use moka::future::Cache as MokaCache;
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
    mime: String,
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
    fn encode(resource: CachedResource, codec: Compression) -> Self {
        let CachedResource { mime, data } = resource;
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
                mime,
                body,
                compression: codec,
                raw_len,
            },
            None => CacheEntry {
                mime,
                body: data,
                compression: Compression::None,
                raw_len,
            },
        }
    }

    fn decode(&self) -> io::Result<CachedResource> {
        let data = match self.compression {
            Compression::None => self.body.clone(),
            Compression::Zstd => Bytes::from(zstd::bulk::decompress(&self.body, self.raw_len)?),
            Compression::Brotli => {
                let mut out = Vec::with_capacity(self.raw_len);
                brotli::BrotliDecompress(&mut self.body.as_ref(), &mut out)?;
                Bytes::from(out)
            }
        };
        Ok(CachedResource {
            mime: self.mime.clone(),
            data,
        })
    }

    /// Bytes this entry occupies: key + Content-Type + the body as stored.
    fn weight(&self, key: &str) -> usize {
        key.len()
            .saturating_add(self.mime.len())
            .saturating_add(self.body.len())
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

pub struct ResourceCache {
    inner: MokaCache<String, CacheEntry>,
    /// 单条目字节上限：超过则不缓存（但仍然正常返回给客户端）。
    /// Measured on the stored body, i.e. after compression, so the limit caps
    /// what the entry costs rather than how big the file was upstream.
    max_entry_size: usize,
    compression: Compression,
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
            .weigher(|key: &String, value: &CacheEntry| -> u32 {
                value.weight(key).try_into().unwrap_or(u32::MAX)
            })
            .build();
        ResourceCache {
            inner,
            max_entry_size,
            compression,
        }
    }

    /// 取缓存；未命中时调用 `init` 回源，并把结果写入缓存后返回。
    ///
    /// 使用 moka 的 entry API（与 `try_get_with` 同一套语义，额外提供
    /// `is_fresh()`）：同一个键上的并发未命中会被合并成一次回源，其余请求
    /// 等待同一个 future。失败不会被缓存，且错误以 `Arc` 形式返回
    /// （多个等待者共享同一个错误对象）。
    ///
    /// 超过 `max_entry_size` 的条目会在写入后立即失效，等价于「不缓存」：
    /// 单个超大文件因此无法挤占整个缓存预算。
    ///
    /// A miss compresses the fetched body and immediately decompresses it again
    /// to build the return value. That is one extra pass over bytes that just
    /// crossed the network, and it buys a single decode path shared by hits and
    /// misses alike.
    pub async fn get_or_fetch<F, E>(
        &self,
        key: String,
        init: F,
    ) -> Result<CachedResource, CacheError<E>>
    where
        F: Future<Output = Result<CachedResource, E>>,
        E: Send + Sync + 'static,
    {
        let codec = self.compression;
        let entry = self
            .inner
            .entry(key.clone())
            .or_try_insert_with(async move { init.await.map(|r| CacheEntry::encode(r, codec)) })
            .await
            .map_err(CacheError::Fetch)?;
        let is_fresh = entry.is_fresh();
        let value = entry.into_value();

        if is_fresh && value.weight(&key) > self.max_entry_size {
            debug!(
                key = %key,
                size = value.weight(&key),
                limit = self.max_entry_size,
                "resource exceeds per-entry cache limit, not cached"
            );
            self.inner.invalidate(&key).await;
        }

        value.decode().map_err(CacheError::Decompress)
    }

    /// Unconditional write: replaces any existing value and restarts the TTL.
    ///
    /// [`Self::get_or_fetch`] neither refetches nor renews on a hit, so the
    /// periodic preload refresh has to come through here.
    ///
    /// Entries above `max_entry_size` are not written and return `false`.
    pub async fn insert(&self, key: String, value: CachedResource) -> bool {
        let entry = CacheEntry::encode(value, self.compression);
        let size = entry.weight(&key);
        if size > self.max_entry_size {
            debug!(
                key = %key,
                size,
                limit = self.max_entry_size,
                "resource exceeds per-entry cache limit, not cached"
            );
            return false;
        }
        self.inner.insert(key, entry).await;
        true
    }

    /// Restarts the TTL of an entry that is already cached, reporting whether
    /// there was one. Preload uses this for files whose upstream hash has not
    /// changed; going through [`Self::insert`] instead would decompress and
    /// recompress a body that is already in exactly the form the cache wants.
    pub async fn renew(&self, key: &str) -> bool {
        match self.inner.get(key).await {
            Some(entry) => {
                self.inner.insert(key.to_string(), entry).await;
                true
            }
            None => false,
        }
    }

    #[cfg(test)]
    async fn get(&self, key: &str) -> Option<CacheEntry> {
        self.inner.get(key).await
    }

    /// Per-entry byte limit, which preload uses to skip hopeless files
    /// before downloading them.
    pub fn max_entry_size(&self) -> usize {
        self.max_entry_size
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
        let limit = key.len() + mime.len() + 32;
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
        assert!(stored.weight(&key) <= limit);
        assert!(
            stored.raw_len > limit,
            "the raw body would have been rejected"
        );
    }
}
