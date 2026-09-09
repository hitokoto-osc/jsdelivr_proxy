//! Cache warming for configured repositories and packages.
//!
//! Three things that are not obvious from the code:
//!
//! * A warmed entry is keyed by the exact request path, so the configured
//!   `version` must match what clients actually request: warming `@HEAD` does
//!   not serve a request for `@master`.
//! * Warming has to repeat. Preloaded entries share the normal TTL and
//!   [`crate::cache::get_or_fetch`] does not renew on a hit, so refreshes go
//!   through [`crate::cache::insert`], which resets the TTL.
//! * A refresh is not a re-download. The listing carries a SHA-256 per file;
//!   when it is unchanged the cached value is simply written back.

pub mod filter;
pub mod listing;

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{sleep, Duration, Instant};
use tracing::{debug, info, warn};

use crate::backend::controller::index::jsdelivr::allowlist;
use crate::cache;
use crate::conf::preload::{Preload, Target};
use crate::upstream;
use listing::RemoteFile;

/// Filename appended when probing the allowlist, which only inspects the
/// provider and package/repository segments.
const ALLOWLIST_PROBE_FILE: &str = "preload-probe";

pub fn spawn() {
    let preload: &'static Preload = &crate::CONFIG.preload;
    if !preload.is_enabled() {
        debug!("preload is disabled (no targets configured)");
        return;
    }

    let interval = preload.refresh_interval_secs(crate::CONFIG.cache.ttl_secs);
    info!(
        "Preload: {} target(s), refresh every {}s, concurrency {}",
        preload.targets.len(),
        interval,
        preload.concurrency()
    );
    // A wrong `version` is the easiest mistake to make here and its only
    // symptom is a permanently cold cache, so make the keys visible up front.
    for target in &preload.targets {
        info!("Preload target: {}/*", target.prefix());
    }
    tokio::spawn(run(preload, Duration::from_secs(interval)));
}

async fn run(preload: &'static Preload, interval: Duration) {
    // Digest of each key as of the previous round; unchanged files only need
    // their TTL reset.
    let mut digests: HashMap<String, String> = HashMap::new();

    loop {
        let started = Instant::now();
        let mut total = Stats::default();

        for target in &preload.targets {
            match warm_target(preload, target, &mut digests).await {
                Ok(stats) => {
                    info!(
                        "Preload {}: warmed {}, renewed {}, skipped {}, failed {} ({} KiB)",
                        target.prefix(),
                        stats.warmed,
                        stats.renewed,
                        stats.skipped,
                        stats.failed,
                        stats.bytes / 1024
                    );
                    total.merge(stats);
                }
                Err(e) => {
                    warn!("Preload {} failed: {:#}", target.prefix(), e);
                    total.failed += 1;
                }
            }
        }

        info!(
            "Preload round finished in {:?}: warmed {}, renewed {}, skipped {}, failed {}; next round in {:?}",
            started.elapsed(),
            total.warmed,
            total.renewed,
            total.skipped,
            total.failed,
            interval
        );
        sleep(interval).await;
    }
}

#[derive(Debug, Default)]
struct Stats {
    warmed: usize,
    renewed: usize,
    skipped: usize,
    failed: usize,
    bytes: u64,
}

impl Stats {
    fn merge(&mut self, other: Stats) {
        self.warmed += other.warmed;
        self.renewed += other.renewed;
        self.skipped += other.skipped;
        self.failed += other.failed;
        self.bytes += other.bytes;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Planned {
    /// The cache key, which is the path clients will request.
    key: String,
    hash: Option<String>,
    size: u64,
}

#[derive(Debug, Default)]
struct Plan {
    files: Vec<Planned>,
    skipped: usize,
    planned_bytes: u64,
}

/// Turns a remote listing into the files to warm.
///
/// The `max_entry_size` check compares the body size alone, ignoring the key
/// and Content-Type that [`crate::cache::insert`] also weighs. It exists only
/// to drop hopeless files before downloading them; `insert` remains the
/// authority.
fn plan(target: &Target, files: &[RemoteFile], max_files: usize, max_entry_size: u64) -> Plan {
    let prefix = target.prefix();
    let mut result = Plan::default();

    for file in files {
        if !filter::should_preload(target, &file.name) {
            result.skipped += 1;
            continue;
        }
        if file.size > max_entry_size {
            debug!(
                file = %file.name,
                size = file.size,
                limit = max_entry_size,
                "preload skips a file larger than the per-entry cache limit"
            );
            result.skipped += 1;
            continue;
        }
        if result.files.len() >= max_files {
            result.skipped += 1;
            continue;
        }
        result.planned_bytes += file.size;
        result.files.push(Planned {
            // Listing names already carry a leading `/`.
            key: format!("{}{}", prefix, file.name),
            hash: file.hash.clone(),
            size: file.size,
        });
    }

    result
}

async fn warm_target(
    preload: &Preload,
    target: &Target,
    digests: &mut HashMap<String, String>,
) -> anyhow::Result<Stats> {
    if !target.is_valid() {
        return Err(anyhow::anyhow!(
            "invalid target: provider must be npm or gh and name must be set (provider={:?}, name={:?})",
            target.provider,
            target.name
        ));
    }

    // The allowlist rejects these requests before the cache is even read, so
    // warming them would only waste the budget.
    let probe = format!("{}/{}", target.prefix(), ALLOWLIST_PROBE_FILE);
    if let Err(e) = allowlist::check(&crate::CONFIG.jsdelivr.allowlist, &probe) {
        warn!(
            "preload target {} is rejected by the resource allowlist ({}); skipping it, \
             warming resources that can never be served would only waste the cache budget",
            target.prefix(),
            e
        );
        return Ok(Stats::default());
    }

    let files = listing::list(
        preload.data_api(),
        &target.provider(),
        target.name(),
        &target.version_spec(),
    )
    .await?;

    let plan = plan(
        target,
        &files,
        preload.max_files(target),
        cache::max_entry_size() as u64,
    );

    let budget = crate::CONFIG.cache.max_capacity_bytes();
    if plan.planned_bytes > budget {
        warn!(
            "preload target {} plans {} MiB, which exceeds the whole cache budget ({} MiB); \
             entries will evict each other; narrow it down with include/extensions/max_files",
            target.prefix(),
            plan.planned_bytes / (1024 * 1024),
            budget / (1024 * 1024)
        );
    }

    let mut stats = Stats {
        skipped: plan.skipped,
        ..Default::default()
    };

    let semaphore = Arc::new(Semaphore::new(preload.concurrency()));
    let mut tasks: JoinSet<(String, Option<String>, Outcome)> = JoinSet::new();

    for file in plan.files {
        // Acquiring before spawning bounds both the concurrent fetches and the
        // number of response bodies held in memory at once.
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("preload semaphore is never closed");
        let previous = digests.get(&file.key).cloned();
        tasks.spawn(async move {
            let _permit = permit;
            let outcome = warm_file(&file, previous.as_deref()).await;
            (file.key, file.hash, outcome)
        });
    }

    while let Some(joined) = tasks.join_next().await {
        let (key, hash, outcome) = match joined {
            Ok(v) => v,
            Err(e) => {
                warn!("preload task panicked: {}", e);
                stats.failed += 1;
                continue;
            }
        };
        match outcome {
            Outcome::Warmed(bytes) => {
                stats.warmed += 1;
                stats.bytes += bytes;
                remember(digests, key, hash);
            }
            Outcome::Renewed => {
                stats.renewed += 1;
                remember(digests, key, hash);
            }
            Outcome::Skipped => {
                stats.skipped += 1;
                // Recording a digest for something not in the cache would make
                // the next round mistake it for an unchanged, cached entry.
                digests.remove(&key);
            }
            Outcome::Failed => {
                stats.failed += 1;
                digests.remove(&key);
            }
        }
    }

    Ok(stats)
}

fn remember(digests: &mut HashMap<String, String>, key: String, hash: Option<String>) {
    match hash {
        Some(hash) => {
            digests.insert(key, hash);
        }
        None => {
            digests.remove(&key);
        }
    }
}

#[derive(Debug)]
enum Outcome {
    /// Fetched and cached, carrying the number of bytes downloaded.
    Warmed(u64),
    /// TTL reset from the cached value, without fetching.
    Renewed,
    /// Fetched but over the per-entry limit, so not cached.
    Skipped,
    Failed,
}

async fn warm_file(file: &Planned, previous_hash: Option<&str>) -> Outcome {
    // Unchanged and still cached: resetting the TTL is all it takes.
    if let (Some(hash), Some(previous)) = (file.hash.as_deref(), previous_hash) {
        if hash == previous && cache::renew(&file.key).await {
            debug!(key = %file.key, "preload renewed an unchanged entry without refetching");
            return Outcome::Renewed;
        }
    }

    match upstream::fetch(&file.key).await {
        Ok(resource) => {
            let bytes = resource.data.len() as u64;
            if cache::insert(file.key.clone(), resource).await {
                Outcome::Warmed(bytes)
            } else {
                Outcome::Skipped
            }
        }
        Err(e) => {
            warn!(key = %file.key, "preload failed to fetch: {}", e);
            Outcome::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(name: &str, size: u64) -> RemoteFile {
        RemoteFile {
            name: name.to_string(),
            hash: Some(format!("hash-of-{}", name)),
            size,
        }
    }

    fn target() -> Target {
        Target {
            provider: "gh".into(),
            name: "hitokoto-osc/sentences-bundle".into(),
            ..Default::default()
        }
    }

    #[test]
    fn plan_builds_request_shaped_cache_keys() {
        let files = [
            remote("/sentences/a.json", 128),
            remote("/img/logo.png", 64),
        ];
        let plan = plan(&target(), &files, 100, 1024);

        assert_eq!(
            plan.files
                .iter()
                .map(|f| f.key.as_str())
                .collect::<Vec<_>>(),
            [
                "gh/hitokoto-osc/sentences-bundle@HEAD/sentences/a.json",
                "gh/hitokoto-osc/sentences-bundle@HEAD/img/logo.png",
            ]
        );
        assert_eq!(plan.planned_bytes, 192);
        assert_eq!(plan.skipped, 0);
    }

    #[test]
    fn plan_drops_filtered_files_and_counts_them() {
        let files = [
            remote("/sentences/a.json", 128),
            remote("/README.md", 10),
            remote("/.github/workflows/ci.yml", 10),
        ];
        let plan = plan(&target(), &files, 100, 1024);

        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.skipped, 2);
    }

    /// Oversized files must be dropped before they are downloaded.
    #[test]
    fn plan_skips_files_larger_than_the_entry_limit() {
        let files = [remote("/big.json", 4096), remote("/small.json", 16)];
        let plan = plan(&target(), &files, 100, 1024);

        assert_eq!(plan.files.len(), 1);
        assert_eq!(
            plan.files[0].key,
            "gh/hitokoto-osc/sentences-bundle@HEAD/small.json"
        );
        assert_eq!(plan.skipped, 1);
        assert_eq!(plan.planned_bytes, 16);
    }

    #[test]
    fn plan_truncates_at_max_files() {
        let files: Vec<RemoteFile> = (0..10)
            .map(|i| remote(&format!("/sentences/{}.json", i), 16))
            .collect();
        let plan = plan(&target(), &files, 3, 1024);

        assert_eq!(plan.files.len(), 3);
        assert_eq!(plan.skipped, 7);
        assert_eq!(plan.planned_bytes, 48);
    }

    #[test]
    fn plan_honours_the_targets_own_filters() {
        let target = Target {
            include: vec!["/dist".into()],
            extensions: vec!["js".into()],
            ..target()
        };
        let files = [
            remote("/dist/vue.js", 16),
            remote("/dist/vue.css", 16),
            remote("/src/vue.js", 16),
        ];
        let plan = plan(&target, &files, 100, 1024);

        assert_eq!(plan.files.len(), 1);
        assert_eq!(
            plan.files[0].key,
            "gh/hitokoto-osc/sentences-bundle@HEAD/dist/vue.js"
        );
    }

    #[test]
    fn digest_bookkeeping_forgets_entries_without_a_hash() {
        let mut digests = HashMap::new();
        remember(&mut digests, "k".into(), Some("h".into()));
        assert_eq!(digests.get("k").map(String::as_str), Some("h"));

        remember(&mut digests, "k".into(), None);
        assert!(digests.is_empty());
    }
}
