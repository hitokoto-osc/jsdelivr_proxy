use std::sync::atomic::{AtomicU64, Ordering};

use axum::{extract::Request, middleware::Next, response::Response};
use serde::Serialize;

use crate::utils::time::must_get_timestamp;

static REQUESTS: Counters = Counters::new();

struct Counters {
    total: AtomicU64,
    statuses: [AtomicU64; 5],
}

#[derive(Serialize)]
pub struct RequestMetrics {
    total: u64,
    statuses: [u64; 5],
    sampled_at: u128,
}

impl Counters {
    const fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            statuses: [const { AtomicU64::new(0) }; 5],
        }
    }

    fn snapshot(&self) -> RequestMetrics {
        RequestMetrics {
            total: self.total.load(Ordering::Relaxed),
            statuses: std::array::from_fn(|i| self.statuses[i].load(Ordering::Relaxed)),
            sampled_at: must_get_timestamp(),
        }
    }

    async fn track(&self, request: Request, next: Next) -> Response {
        // Match the registered admin routes, not arbitrary proxy paths that
        // happen to start with the same prefix.
        let admin = matches!(
            request.uri().path(),
            "/admin"
                | "/admin/api/stats"
                | "/admin/api/events"
                | "/admin/api/cache"
                | "/admin/api/cache/purge"
                | "/admin/api/audit"
        );
        if !admin {
            self.total.fetch_add(1, Ordering::Relaxed);
        }
        let response = next.run(request).await;
        if !admin {
            if let Some(counter) = self
                .statuses
                .get(usize::from(response.status().as_u16() / 100 - 1))
            {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        }
        response
    }
}

pub fn snapshot() -> RequestMetrics {
    REQUESTS.snapshot()
}

pub async fn track(request: Request, next: Next) -> Response {
    REQUESTS.track(request, next).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::StatusCode, middleware, routing::get, Router};
    use std::sync::Arc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn counts_errors_and_fallbacks_but_excludes_admin_traffic() {
        let counters = Arc::new(Counters::new());
        let state = counters.clone();
        let app = Router::new()
            .route("/ok", get(|| async { StatusCode::OK }))
            .route("/error", get(|| async { StatusCode::BAD_GATEWAY }))
            .route(
                "/admin/api/events",
                get(|| async { StatusCode::UNAUTHORIZED }),
            )
            .layer(middleware::from_fn(move |request, next| {
                let state = state.clone();
                async move { state.track(request, next).await }
            }));
        for (method, path) in [
            ("GET", "/ok"),
            ("GET", "/error"),
            ("GET", "/administrator"),
            ("POST", "/ok"),
            ("GET", "/admin/api/events?prefix=npm"),
        ] {
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
        }
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.total, 4);
        assert_eq!(snapshot.statuses, [0, 1, 0, 2, 1]);
    }
}
