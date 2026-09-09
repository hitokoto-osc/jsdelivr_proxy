use serde::{Deserialize, Serialize};
use std::fmt;

/// 进程内缓存配置。
#[derive(Deserialize, Debug)]
pub struct Cache {
    /// 缓存存活时间（秒），默认 7200（2 小时），与迁移前 Redis 版本一致。
    #[serde(default = "Cache::default_ttl_secs")]
    pub ttl_secs: u64,
    /// 缓存总字节预算（MB）。默认 256MB：jsDelivr 上单个 npm/gh 资源通常在
    /// 数 KB ~ 数 MB 量级，256MB 足以覆盖数千个热点文件，同时对容器常见的
    /// 512MB / 1GB 内存限制仍留有余量。
    #[serde(default = "Cache::default_max_capacity_mb")]
    pub max_capacity_mb: u64,
    /// 单个条目的字节上限（MB），默认 16MB。超过此大小的资源仍会正常返回，
    /// 只是不进入缓存，避免一个超大文件挤占整个缓存预算。
    #[serde(default = "Cache::default_max_entry_size_mb")]
    pub max_entry_size_mb: u64,
    /// Both budgets above are measured after compression, so raising the
    /// compression ratio directly raises how much fits in the cache.
    #[serde(default)]
    pub compression: Compression,
}

/// Codec used to store cached response bodies in memory.
#[derive(Deserialize, Serialize, Debug, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    /// Keep bodies verbatim, which makes a cache hit a refcount bump and
    /// nothing else — the right choice when CPU is scarcer than memory.
    None,
    #[default]
    Zstd,
    Brotli,
}

impl fmt::Display for Compression {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Compression::None => write!(f, "none"),
            Compression::Zstd => write!(f, "zstd"),
            Compression::Brotli => write!(f, "brotli"),
        }
    }
}

const MB: u64 = 1024 * 1024;

impl Cache {
    fn default_ttl_secs() -> u64 {
        60 * 60 * 2
    }

    fn default_max_capacity_mb() -> u64 {
        256
    }

    fn default_max_entry_size_mb() -> u64 {
        16
    }

    pub fn max_capacity_bytes(&self) -> u64 {
        self.max_capacity_mb.saturating_mul(MB)
    }

    pub fn max_entry_size_bytes(&self) -> usize {
        self.max_entry_size_mb
            .saturating_mul(MB)
            .try_into()
            .unwrap_or(usize::MAX)
    }
}

impl Default for Cache {
    fn default() -> Self {
        Cache {
            ttl_secs: Cache::default_ttl_secs(),
            max_capacity_mb: Cache::default_max_capacity_mb(),
            max_entry_size_mb: Cache::default_max_entry_size_mb(),
            compression: Compression::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ttl_matches_the_redis_era_two_hours() {
        let cache = Cache::default();
        assert_eq!(cache.ttl_secs, 7200);
        assert_eq!(cache.max_capacity_bytes(), 256 * MB);
        assert_eq!(cache.max_entry_size_bytes(), (16 * MB) as usize);
        assert_eq!(cache.compression, Compression::Zstd);
    }

    #[test]
    fn compression_deserializes_from_lowercase_names() {
        for (input, expected) in [
            ("none", Compression::None),
            ("zstd", Compression::Zstd),
            ("brotli", Compression::Brotli),
        ] {
            let parsed: Compression =
                serde_json::from_str(&format!("\"{input}\"")).expect("known codec name");
            assert_eq!(parsed, expected);
        }
    }
}
