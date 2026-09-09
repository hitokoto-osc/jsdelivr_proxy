// `pub` so that `preload` can reuse the allowlist check rather than
// reimplementing jsDelivr path parsing.
pub mod auth;
pub mod controller;

use std::net::SocketAddr;

use crate::CONFIG;
use anyhow::Context;
use axum::{
    routing::{get, post},
    Router,
};
use controller::{admin, index, webhook};
use tower_http::{compression::CompressionLayer, trace::TraceLayer};
use tracing::info;

/// 组装路由表。
///
/// 注意：axum 0.8 的通配符语法是 `{*path}`；具体路由（`/about` 等）与通配符
/// 路由可以共存，matchit 会优先匹配更具体的路径。
fn router() -> Router {
    Router::new()
        .route("/", get(index::index))
        .route("/favicon.ico", get(index::favicon))
        .route("/about", get(index::about))
        .route("/admin", get(admin::panel))
        .route("/admin/api/stats", get(admin::stats))
        .route("/admin/api/cache", get(admin::cache_list))
        .route("/admin/api/cache/purge", post(admin::cache_purge))
        .route("/admin/api/audit", get(admin::audit_list))
        .route("/webhook/cache/purge", post(webhook::purge_cache))
        .route("/{*path}", get(index::jsdelivr::get))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
}

pub async fn init() -> anyhow::Result<()> {
    let host = match &CONFIG.server.host {
        Some(v) => v.to_owned(),
        None => "0.0.0.0".to_string(),
    };
    let port = match &CONFIG.server.port {
        Some(v) => *v,
        None => 28319,
    };
    let addr = format!("{}:{}", host, port);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind {}", addr))?;
    info!(
        "HTTP Server is listening on http://{}",
        listener.local_addr()?
    );

    // `ConnectInfo` is what lets the audit log record where an admin request
    // came from; without this the extractor fails at runtime.
    axum::serve(
        listener,
        router().into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// 确认具体路由与 `{*path}` 通配符路由可以同时注册而不会 panic。
    #[test]
    fn router_builds_without_route_conflict() {
        let _ = router();
    }

    /// Mirrors the route table with inert handlers. The real ones all read
    /// `CONFIG`, whose initialisation parses the process command line and
    /// would choke on the test harness's arguments; this keeps the question
    /// under test — matchit preferring static segments over `{*path}` —
    /// answerable without booting the whole program.
    fn probe_router() -> Router {
        Router::new()
            .route("/", get(|| async { "index" }))
            .route("/favicon.ico", get(|| async { "favicon" }))
            .route("/about", get(|| async { "about" }))
            .route("/admin", get(|| async { "panel" }))
            .route("/admin/api/stats", get(|| async { "stats" }))
            .route("/admin/api/cache", get(|| async { "cache" }))
            .route("/admin/api/cache/purge", post(|| async { "purge" }))
            .route("/admin/api/audit", get(|| async { "audit" }))
            .route("/webhook/cache/purge", post(|| async { "webhook" }))
            .route("/{*path}", get(|| async { "jsdelivr" }))
    }

    async fn route(method: &str, path: &str) -> (StatusCode, String) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = probe_router().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn admin_and_webhook_routes_win_over_the_wildcard() {
        assert_eq!(route("GET", "/admin").await.1, "panel");
        assert_eq!(route("GET", "/admin/api/stats").await.1, "stats");
        assert_eq!(route("GET", "/admin/api/cache?prefix=npm").await.1, "cache");
        assert_eq!(route("POST", "/admin/api/cache/purge").await.1, "purge");
        assert_eq!(route("GET", "/admin/api/audit").await.1, "audit");
        assert_eq!(route("POST", "/webhook/cache/purge").await.1, "webhook");
    }

    /// A path that merely begins with the same letters is still a jsDelivr
    /// request; so is an unknown path below `/admin`.
    #[tokio::test]
    async fn everything_else_still_reaches_the_jsdelivr_handler() {
        assert_eq!(route("GET", "/npm/vue@3/dist/vue.js").await.1, "jsdelivr");
        assert_eq!(route("GET", "/administrator/x.js").await.1, "jsdelivr");
        assert_eq!(route("GET", "/admin/api/unknown").await.1, "jsdelivr");
    }

    /// The wildcard is GET-only, so a POST to an arbitrary path must not be
    /// answered by the jsDelivr handler.
    #[tokio::test]
    async fn a_post_to_an_unknown_path_is_rejected() {
        assert_eq!(
            route("POST", "/npm/vue@3").await.0,
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
}
