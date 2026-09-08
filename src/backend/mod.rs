mod controller;

use crate::CONFIG;
use anyhow::Context;
use axum::{routing::get, Router};
use controller::index;
use tower_http::trace::TraceLayer;
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
        .route("/{*path}", get(index::jsdelivr::get))
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

    axum::serve(listener, router()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确认具体路由与 `{*path}` 通配符路由可以同时注册而不会 panic。
    #[test]
    fn router_builds_without_route_conflict() {
        let _ = router();
    }
}
