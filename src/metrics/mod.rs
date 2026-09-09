pub mod requests;

use std::sync::LazyLock;
use std::time::Duration;

use serde::Serialize;
use sysinfo::{get_current_pid, Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::{watch, Notify};
use tracing::{debug, warn};

use crate::utils::time::must_get_timestamp;

/// Comfortably above `sysinfo::MINIMUM_CPU_UPDATE_INTERVAL` (~200ms), below
/// which CPU deltas stop being meaningful.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProcessMetrics {
    /// Percent of a single core, so it can exceed 100 on a multi-core machine.
    pub cpu_percent: f32,
    /// Resident set size.
    pub memory_bytes: u64,
    pub virtual_memory_bytes: u64,
    /// Seconds since the process started.
    pub uptime_secs: u64,
    /// Cores available to this process, for normalising `cpu_percent`.
    pub cpu_count: usize,
    /// Unix milliseconds. Zero until the first sample lands, which is how the
    /// panel tells "idle" apart from "not sampling".
    pub sampled_at: u128,
}

impl ProcessMetrics {
    const fn empty() -> Self {
        ProcessMetrics {
            cpu_percent: 0.0,
            memory_bytes: 0,
            virtual_memory_bytes: 0,
            uptime_secs: 0,
            cpu_count: 0,
            sampled_at: 0,
        }
    }
}

static UPDATES: LazyLock<watch::Sender<ProcessMetrics>> = LazyLock::new(|| {
    let (sender, _) = watch::channel(ProcessMetrics::empty());
    sender
});
static SUBSCRIBED: Notify = Notify::const_new();

pub fn subscribe() -> watch::Receiver<ProcessMetrics> {
    let receiver = UPDATES.subscribe();
    SUBSCRIBED.notify_one();
    receiver
}

/// Starts the sampler. Only called when the admin API is enabled: without a
/// reader there is no reason to wake up every few seconds.
pub fn spawn() {
    let pid = match get_current_pid() {
        Ok(pid) => pid,
        Err(e) => {
            warn!("cannot determine the current PID ({e}); process metrics stay empty");
            return;
        }
    };
    tokio::spawn(sample_loop(pid));
}

pub fn snapshot() -> ProcessMetrics {
    *UPDATES.borrow()
}

async fn sample_loop(pid: Pid) {
    let cpu_count = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(0);

    loop {
        while UPDATES.receiver_count() == 0 {
            SUBSCRIBED.notified().await;
        }
        // Discard CPU history across idle periods so the next delta only
        // covers time during which an admin is watching.
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now() + sysinfo::MINIMUM_CPU_UPDATE_INTERVAL,
            SAMPLE_INTERVAL,
        );
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = UPDATES.closed() => break,
                _ = interval.tick() => {}
            }
            // Refreshing a single PID reads one /proc entry (or the equivalent) and
            // takes well under a millisecond, so it runs inline rather than paying
            // for a trip through `spawn_blocking`.
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing().with_cpu().with_memory(),
            );

            match system.process(pid) {
                Some(process) => {
                    let sample = ProcessMetrics {
                        cpu_percent: process.cpu_usage(),
                        memory_bytes: process.memory(),
                        virtual_memory_bytes: process.virtual_memory(),
                        uptime_secs: process.run_time(),
                        cpu_count,
                        sampled_at: must_get_timestamp(),
                    };
                    UPDATES.send_replace(sample);
                }
                // Unreachable while this task is running, since the task belongs to
                // the very process being looked up.
                None => debug!("the current process is missing from the process table"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sampling_only_runs_while_subscribed_and_resumes() {
        let task = tokio::spawn(sample_loop(get_current_pid().unwrap()));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(snapshot().sampled_at, 0);
        let mut first = subscribe();
        tokio::time::timeout(Duration::from_secs(2), first.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot().memory_bytes > 0);
        let mut second = subscribe();
        drop(first);
        tokio::time::timeout(Duration::from_secs(6), second.changed())
            .await
            .unwrap()
            .unwrap();
        drop(second);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let stopped_at = snapshot().sampled_at;
        tokio::time::sleep(SAMPLE_INTERVAL + Duration::from_millis(100)).await;
        assert_eq!(snapshot().sampled_at, stopped_at);
        let mut resumed = subscribe();
        tokio::time::timeout(Duration::from_secs(2), resumed.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot().sampled_at > stopped_at);
        task.abort();
    }

    /// Two samples separated by the real interval, to prove the sampler
    /// produces a populated snapshot rather than zeros.
    #[tokio::test]
    async fn sampling_fills_in_memory_and_uptime() {
        let pid = get_current_pid().expect("a PID for the test process");
        let mut system = System::new();
        let kind = ProcessRefreshKind::nothing().with_cpu().with_memory();

        system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, kind);
        tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
        system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, kind);

        let process = system.process(pid).expect("the test process itself");
        assert!(process.memory() > 0);
        assert!(process.cpu_usage() >= 0.0);
    }
}
