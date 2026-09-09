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

use crate::conf::cache::Cache as CacheConfig;
use crate::CONFIG;
use bytes::Bytes;
use moka::future::Cache as MokaCache;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

lazy_static! {
    static ref CACHE: ResourceCache = ResourceCache::new(&CONFIG.cache);
}

/// 一条缓存的上游资源：Content-Type 与响应体。
///
/// `Bytes` 的克隆是引用计数级别的浅拷贝，因此从缓存取值不会复制文件内容。
#[derive(Clone, Debug)]
pub struct CachedResource {
    pub mime: String,
    pub data: Bytes,
}

impl CachedResource {
    /// 该条目占用的字节数（键 + Content-Type + 响应体）。
    fn weight(&self, key: &str) -> usize {
        key.len()
            .saturating_add(self.mime.len())
            .saturating_add(self.data.len())
    }
}

pub struct ResourceCache {
    inner: MokaCache<String, CachedResource>,
    /// 单条目字节上限：超过则不缓存（但仍然正常返回给客户端）。
    max_entry_size: usize,
}

impl ResourceCache {
    pub fn new(config: &CacheConfig) -> Self {
        Self::with_params(
            config.ttl_secs,
            config.max_capacity_bytes(),
            config.max_entry_size_bytes(),
        )
    }

    /// `max_capacity_bytes` 是字节预算：moka 的 `max_capacity` 单位是 weigher
    /// 返回的「权重」，这里 weigher 返回条目的字节数，因此容量即字节数。
    /// 若不设置 weigher，`max_capacity` 的单位会退化成「条目数」。
    pub fn with_params(ttl_secs: u64, max_capacity_bytes: u64, max_entry_size: usize) -> Self {
        let inner = MokaCache::builder()
            .time_to_live(Duration::from_secs(ttl_secs))
            .max_capacity(max_capacity_bytes)
            .weigher(|key: &String, value: &CachedResource| -> u32 {
                value.weight(key).try_into().unwrap_or(u32::MAX)
            })
            .build();
        ResourceCache {
            inner,
            max_entry_size,
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
    pub async fn get_or_fetch<F, E>(&self, key: String, init: F) -> Result<CachedResource, Arc<E>>
    where
        F: Future<Output = Result<CachedResource, E>>,
        E: Send + Sync + 'static,
    {
        let entry = self
            .inner
            .entry(key.clone())
            .or_try_insert_with(init)
            .await?;
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

        Ok(value)
    }

    /// Unconditional write: replaces any existing value and restarts the TTL.
    ///
    /// [`Self::get_or_fetch`] neither refetches nor renews on a hit, so the
    /// periodic preload refresh has to come through here.
    ///
    /// Entries above `max_entry_size` are not written and return `false`.
    pub async fn insert(&self, key: String, value: CachedResource) -> bool {
        let size = value.weight(&key);
        if size > self.max_entry_size {
            debug!(
                key = %key,
                size,
                limit = self.max_entry_size,
                "resource exceeds per-entry cache limit, not cached"
            );
            return false;
        }
        self.inner.insert(key, value).await;
        true
    }

    pub async fn get(&self, key: &str) -> Option<CachedResource> {
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
pub async fn get_or_fetch<F, E>(key: String, init: F) -> Result<CachedResource, Arc<E>>
where
    F: Future<Output = Result<CachedResource, E>>,
    E: Send + Sync + 'static,
{
    CACHE.get_or_fetch(key, init).await
}

pub async fn insert(key: String, value: CachedResource) -> bool {
    CACHE.insert(key, value).await
}

pub async fn get(key: &str) -> Option<CachedResource> {
    CACHE.get(key).await
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

    /// 命中缓存时不应再次调用 loader。
    #[tokio::test]
    async fn cache_hit_does_not_reinvoke_loader() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024);
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
        let cache = ResourceCache::with_params(1, 1024 * 1024, 1024 * 1024);
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
        let cache = ResourceCache::with_params(60, 1024 * 1024, 64);
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
        let cache = ResourceCache::with_params(60, 1024 * 1024, limit);

        cache
            .get_or_fetch::<_, Infallible>(key.clone(), async { Ok(resource(mime, 32)) })
            .await
            .unwrap();

        cache.run_pending_tasks().await;
        assert!(cache.get(&key).await.is_some());
    }

    #[tokio::test]
    async fn insert_overwrites_the_existing_entry() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024);
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
        assert_eq!(value.data.len(), 32);
    }

    /// Without a TTL reset the periodic preload refresh would be a no-op for
    /// entries that are still cached.
    #[tokio::test]
    async fn insert_resets_the_ttl() {
        let cache = ResourceCache::with_params(2, 1024 * 1024, 1024 * 1024);
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
        let cache = ResourceCache::with_params(60, 1024 * 1024, 64);
        let key = "npm/big@1/big.bin".to_string();

        assert!(
            !cache
                .insert(key.clone(), resource("application/octet-stream", 128))
                .await
        );
        cache.run_pending_tasks().await;
        assert!(cache.get(&key).await.is_none());
    }

    /// 回源失败不应被缓存。
    #[tokio::test]
    async fn errors_are_not_cached() {
        let cache = ResourceCache::with_params(60, 1024 * 1024, 1024 * 1024);
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
}
