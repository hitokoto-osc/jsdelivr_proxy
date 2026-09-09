//! Process-level runtime metrics for the admin panel.
//!
//! CPU usage is a difference between two samples, so it cannot be produced on
//! demand inside a request handler without either sleeping for
//! `MINIMUM_CPU_UPDATE_INTERVAL` or reporting a meaningless zero. A background
//! sampler keeps the latest snapshot instead and handlers just read it, which
//! also means the panel polling faster than the sampler costs nothing.

use std::sync::{PoisonError, RwLock};
use std::time::Duration;

use serde::Serialize;
use sysinfo::{get_current_pid, Pid, ProcessRefreshKind, ProcessesToUpdate, System};
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

static SNAPSHOT: RwLock<ProcessMetrics> = RwLock::new(ProcessMetrics::empty());

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
    *SNAPSHOT.read().unwrap_or_else(PoisonError::into_inner)
}

async fn sample_loop(pid: Pid) {
    let mut system = System::new();
    let cpu_count = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(0);

    loop {
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
                *SNAPSHOT.write().unwrap_or_else(PoisonError::into_inner) = sample;
            }
            // Unreachable while this task is running, since the task belongs to
            // the very process being looked up.
            None => debug!("the current process is missing from the process table"),
        }

        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_snapshot_reads_as_empty_before_the_first_sample() {
        let metrics = snapshot();
        assert_eq!(metrics.sampled_at, 0);
        assert_eq!(metrics.memory_bytes, 0);
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
