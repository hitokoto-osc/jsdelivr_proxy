//! Admin panel and admin API.
//!
//! Handlers here only do HTTP work: authenticate, validate, call into the
//! cache / metrics / audit modules, serialise. None of them reach into moka or
//! sysinfo directly.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Query};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::audit::{self, Actor, Record};
use crate::backend::auth::{AdminAuth, AuthError};
use crate::backend::controller::purge::PurgeRequest;
use crate::cache::{self, purge};
use crate::metrics;
use crate::utils::response::{fail_with_message, success, APIResponse};
use crate::CONFIG;

/// Rows returned when the caller does not ask for a specific number.
const DEFAULT_LIMIT: usize = 100;
/// Upper bound on a caller-supplied limit. The cache listing is built by
/// walking every entry, so an unbounded limit would let one request serialise
/// the whole keyspace.
const MAX_LIMIT: usize = 1000;

/// The panel itself. Served without the admin key because the page holds no
/// data: it prompts for the key and then calls the JSON API below with it.
pub async fn panel() -> Response {
    if !CONFIG.admin.is_enabled() {
        return AuthError::Disabled.into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../../../../assets/admin/index.html"),
    )
        .into_response()
}

pub async fn stats(_: AdminAuth) -> APIResponse<Value> {
    success(json!({
        "process": metrics::snapshot(),
        "cache": cache::stats().await,
        "audit": { "backend": audit::backend_name() },
        "webhook": { "enabled": CONFIG.admin.is_webhook_enabled() },
    }))
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    prefix: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

impl ListQuery {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }

    fn offset(&self) -> usize {
        self.offset.unwrap_or(0)
    }

    fn prefix(&self) -> Option<&str> {
        self.prefix
            .as_deref()
            .map(str::trim)
            .filter(|prefix| !prefix.is_empty())
    }
}

/// `total` counts everything matching `prefix`, not just the returned page, so
/// the panel can show how much it is not displaying.
pub async fn cache_list(_: AdminAuth, Query(query): Query<ListQuery>) -> APIResponse<Value> {
    let entries = cache::entries(query.prefix());
    let total = entries.len();
    let page: Vec<_> = entries
        .into_iter()
        .skip(query.offset())
        .take(query.limit())
        .collect();
    success(json!({
        "total": total,
        "offset": query.offset(),
        "limit": query.limit(),
        "entries": page,
    }))
}

pub async fn cache_purge(
    _: AdminAuth,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<PurgeRequest>,
) -> APIResponse<Value> {
    let remote = Some(peer.to_string());

    let scope = match request.into_scope() {
        Ok(scope) => scope,
        Err(reason) => {
            audit::record(
                Record::new(Actor::Admin, "cache.purge")
                    .remote_addr(remote)
                    .failed(reason),
            )
            .await;
            return fail_with_message(400, None, reason.to_string());
        }
    };

    let outcome = purge::run(&scope).await;
    audit::record(
        Record::new(Actor::Admin, "cache.purge")
            .remote_addr(remote)
            .target(scope.label())
            .detail(format!(
                "removed {}, not cached {}",
                outcome.removed, outcome.missed
            )),
    )
    .await;

    success(json!({
        "scope": scope.label(),
        "removed": outcome.removed,
        "missed": outcome.missed,
    }))
}

pub async fn audit_list(_: AdminAuth, Query(query): Query<ListQuery>) -> APIResponse<Value> {
    match audit::list(query.limit(), query.offset()).await {
        Ok(records) => success(json!({
            "backend": audit::backend_name(),
            "offset": query.offset(),
            "limit": query.limit(),
            "records": records,
        })),
        Err(e) => fail_with_message(500, None, format!("failed to read the audit log: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(limit: Option<usize>, offset: Option<usize>, prefix: Option<&str>) -> ListQuery {
        ListQuery {
            prefix: prefix.map(Into::into),
            limit,
            offset,
        }
    }

    #[test]
    fn limits_are_clamped_into_a_serialisable_range() {
        assert_eq!(query(None, None, None).limit(), DEFAULT_LIMIT);
        assert_eq!(query(Some(0), None, None).limit(), 1);
        assert_eq!(query(Some(usize::MAX), None, None).limit(), MAX_LIMIT);
        assert_eq!(query(Some(25), None, None).limit(), 25);
    }

    #[test]
    fn a_blank_prefix_filters_nothing() {
        assert_eq!(query(None, None, Some("  ")).prefix(), None);
        assert_eq!(query(None, None, Some(" npm/ ")).prefix(), Some("npm/"));
        assert_eq!(query(None, Some(7), None).offset(), 7);
    }
}
