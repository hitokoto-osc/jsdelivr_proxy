//! Cache-purge webhook.
//!
//! Authentication is an HMAC over the raw body rather than the admin key, for
//! two reasons: a secret handed to a CI job or a repository must not also open
//! the admin API, and a signature is the only credential services like GitHub
//! and Gitea can be configured to send.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use serde_json::{json, Value};
use tracing::warn;

use crate::audit::{self, Actor, Record};
use crate::backend::controller::purge::PurgeRequest;
use crate::cache::purge;
use crate::utils::hash::verify_hmac_sha256;
use crate::utils::response::{fail_with_message, success, APIResponse};
use crate::CONFIG;

/// GitHub's header name, which Gitea and Gogs also send.
const SIGNATURE_HEADER: &str = "x-hub-signature-256";

/// `body` is taken as raw [`Bytes`] because the MAC covers the exact bytes the
/// sender signed; deserialising first and re-serialising would not reproduce
/// them. As a body-consuming extractor it must come last.
pub async fn purge_cache(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> APIResponse<Value> {
    let Some(secret) = CONFIG.admin.webhook_secret() else {
        return fail_with_message(
            503,
            None,
            "the cache webhook is disabled: no webhook secret is configured".to_string(),
        );
    };

    let signature = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !verify_hmac_sha256(secret.as_bytes(), &body, signature) {
        // Deliberately not written to the audit log: this endpoint is
        // unauthenticated until this check passes, so recording every failure
        // would let anyone fill a remote audit database. The application log
        // still carries it.
        warn!(peer = %peer, "rejected a cache webhook with a missing or invalid signature");
        return fail_with_message(401, None, format!("missing or invalid {SIGNATURE_HEADER}"));
    }

    let remote = Some(peer.to_string());
    let scope = match serde_json::from_slice::<PurgeRequest>(&body)
        .map_err(|e| e.to_string())
        .and_then(|request| request.into_scope().map_err(str::to_string))
    {
        Ok(scope) => scope,
        Err(reason) => {
            audit::record(
                Record::new(Actor::Webhook, "cache.purge")
                    .remote_addr(remote)
                    .failed(reason.clone()),
            )
            .await;
            return fail_with_message(400, None, reason);
        }
    };

    let outcome = purge::run(&scope).await;
    audit::record(
        Record::new(Actor::Webhook, "cache.purge")
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
