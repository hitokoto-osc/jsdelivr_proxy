//! Audit trail for the admin API and the cache-purge webhook.
//!
//! The backend is picked once at startup: a remote Turso database when one is
//! configured, a bounded in-memory buffer otherwise. The in-memory buffer is
//! not a degraded fallback to apologise for — it keeps the panel useful on the
//! zero-dependency single-binary deployment this proxy is built for. It simply
//! does not survive a restart, which is why Turso exists as an option.

mod memory;
mod turso;

use std::fmt;
use std::sync::OnceLock;

use serde::Serialize;
use tracing::{info, warn};

use crate::utils::time::must_get_timestamp;
use crate::CONFIG;

/// Entries kept by the in-memory backend. Records are tiny (a few hundred
/// bytes), so this is well under a megabyte and covers far more history than
/// an operator will scroll through in the panel.
const MEMORY_CAPACITY: usize = 500;

/// Who performed the operation. Not a user identity: the admin key and the
/// webhook secret are both shared secrets, so this only says which door the
/// request came through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    Admin,
    Webhook,
}

impl Actor {
    pub fn as_str(&self) -> &'static str {
        match self {
            Actor::Admin => "admin",
            Actor::Webhook => "webhook",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "webhook" => Actor::Webhook,
            _ => Actor::Admin,
        }
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Record {
    /// Unix milliseconds.
    pub ts: u128,
    pub actor: Actor,
    /// Peer address of the TCP connection. Behind a reverse proxy this is the
    /// proxy, not the original client; no forwarding header is trusted here.
    pub remote_addr: Option<String>,
    /// Dotted operation name, e.g. `cache.purge`.
    pub action: String,
    /// What the operation was aimed at: a cache key, a prefix, or `*`.
    pub target: Option<String>,
    pub success: bool,
    pub detail: Option<String>,
}

impl Record {
    pub fn new(actor: Actor, action: impl Into<String>) -> Self {
        Record {
            ts: must_get_timestamp(),
            actor,
            remote_addr: None,
            action: action.into(),
            target: None,
            success: true,
            detail: None,
        }
    }

    pub fn remote_addr(mut self, addr: Option<String>) -> Self {
        self.remote_addr = addr;
        self
    }

    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn failed(mut self, reason: impl Into<String>) -> Self {
        self.success = false;
        self.detail = Some(reason.into());
        self
    }
}

enum Backend {
    Memory(memory::MemoryLog),
    Turso(Box<turso::TursoLog>),
}

impl Backend {
    async fn append(&self, record: Record) -> anyhow::Result<()> {
        match self {
            Backend::Memory(log) => {
                log.append(record);
                Ok(())
            }
            Backend::Turso(log) => log.append(record).await,
        }
    }

    async fn list(&self, limit: usize, offset: usize) -> anyhow::Result<Vec<Record>> {
        match self {
            Backend::Memory(log) => Ok(log.list(limit, offset)),
            Backend::Turso(log) => log.list(limit, offset).await,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Backend::Memory(_) => "memory",
            Backend::Turso(_) => "turso",
        }
    }
}

static BACKEND: OnceLock<Backend> = OnceLock::new();

/// Resolves the backend. A Turso database that cannot be reached at startup
/// degrades to the in-memory buffer rather than aborting: losing the ability to
/// persist an audit trail is not a reason to take the proxy itself down.
pub async fn init() {
    let backend = match turso::TursoLog::connect(&CONFIG.admin.turso).await {
        Ok(Some(log)) => {
            info!("Audit log: Turso at {}", log.endpoint());
            Backend::Turso(Box::new(log))
        }
        Ok(None) => {
            info!(
                "Audit log: in-memory, last {} entries (set [admin.turso] to persist across restarts)",
                MEMORY_CAPACITY
            );
            Backend::Memory(memory::MemoryLog::new(MEMORY_CAPACITY))
        }
        Err(e) => {
            warn!("Audit log: Turso is unreachable ({e:#}); falling back to the in-memory buffer");
            Backend::Memory(memory::MemoryLog::new(MEMORY_CAPACITY))
        }
    };
    let _ = BACKEND.set(backend);
}

/// Appends a record, reporting failures through the log rather than to the
/// caller: the operation being audited has already happened by this point, so
/// there is nothing useful for a handler to do with the error.
pub async fn record(record: Record) {
    let Some(backend) = BACKEND.get() else {
        warn!(
            action = %record.action,
            "audit log used before initialisation; the record was dropped"
        );
        return;
    };
    let action = record.action.clone();
    if let Err(e) = backend.append(record).await {
        warn!("failed to append {action} to the audit log: {e:#}");
    }
}

/// Most recent first.
pub async fn list(limit: usize, offset: usize) -> anyhow::Result<Vec<Record>> {
    match BACKEND.get() {
        Some(backend) => backend.list(limit, offset).await,
        None => Ok(Vec::new()),
    }
}

pub fn backend_name() -> &'static str {
    BACKEND.get().map(Backend::name).unwrap_or("uninitialised")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_names_round_trip() {
        for actor in [Actor::Admin, Actor::Webhook] {
            assert_eq!(Actor::parse(actor.as_str()), actor);
        }
    }

    #[test]
    fn builders_compose_a_failed_record() {
        let record = Record::new(Actor::Webhook, "cache.purge")
            .remote_addr(Some("10.0.0.1:5000".into()))
            .target("gh/owner/repo@HEAD")
            .failed("signature mismatch");
        assert!(!record.success);
        assert_eq!(record.detail.as_deref(), Some("signature mismatch"));
        assert_eq!(record.target.as_deref(), Some("gh/owner/repo@HEAD"));
    }
}
