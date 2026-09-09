use axum::{
    extract::{Path, RawQuery},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde_json::Value;
use tracing::error;

use super::jsdelivr::{remember_resource, validate_path};
use crate::{
    cache,
    upstream::{self, UpstreamError},
    utils::response::{fail, fail_with_message},
    CONFIG,
};

fn cache_key(path: &str, query: Option<&str>) -> String {
    match query {
        Some(query) => format!("{path}?{query}"),
        None => path.to_string(),
    }
}

pub async fn get(
    Path(hash): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let path = format!("avatar/{hash}");
    if let Err(error) = validate_path(&path) {
        return fail_with_message::<Value>(400, None, error.to_string()).into_response();
    }
    match remember_resource(
        cache_key(&path, query.as_deref()),
        &headers,
        cache::shared(),
        upstream::fetch_gravatar_response(
            upstream::client(),
            &CONFIG.gravatar.upstream,
            &path,
            query.as_deref(),
        ),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            error!(%error, "Gravatar proxy request failed");
            match error.downcast_ref::<UpstreamError>() {
                Some(UpstreamError::RequestStatusCheck(status)) => {
                    fail::<Value>(*status as i64, None).into_response()
                }
                Some(UpstreamError::UrlParse(_) | UpstreamError::RequestContentTypeConvert(_)) => {
                    fail_with_message::<Value>(400, None, error.to_string()).into_response()
                }
                _ => fail_with_message::<Value>(500, None, error.to_string()).into_response(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cache::ResourceCache, conf::cache::Cache};
    use axum::{http::StatusCode, routing::get, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn upstream_queries_cache_hits_and_errors() {
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let app = Router::new().route(
            "/mirror/avatar/{hash}",
            get(move |RawQuery(query): RawQuery, headers: HeaderMap| {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(headers["accept-encoding"], "identity");
                    let query = query.unwrap_or_default();
                    if query == "d=404" {
                        return StatusCode::NOT_FOUND.into_response();
                    }
                    ([(axum::http::header::CONTENT_TYPE, "image/png")], query).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/mirror/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        let cache = ResourceCache::new(&Cache::default());
        let path = "avatar/abc.jpg";
        let headers = HeaderMap::new();
        let queries = [
            "s=80&d=https%3A%2F%2Fexample.com%2Favatar.png",
            "s=200&d=identicon",
        ];
        for query in queries {
            let key = cache_key(path, Some(query));
            let response = remember_resource(
                key.clone(),
                &headers,
                cache.clone(),
                upstream::fetch_gravatar_response(&client, &base, path, Some(query)),
            )
            .await
            .unwrap();
            assert_eq!(response.headers()["content-type"], "image/png");
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap(),
                query
            );
            let _guard = cache.lock_fetch(&key).await;
        }
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        for query in queries {
            let response = remember_resource(
                cache_key(path, Some(query)),
                &headers,
                cache.clone(),
                async { panic!("cache hit must not fetch upstream") },
            )
            .await
            .unwrap();
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap(),
                query
            );
        }
        let key = cache_key(path, Some("d=404"));
        for _ in 0..2 {
            let error = remember_resource(
                key.clone(),
                &headers,
                cache.clone(),
                upstream::fetch_gravatar_response(&client, &base, path, Some("d=404")),
            )
            .await
            .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<UpstreamError>(),
                Some(UpstreamError::RequestStatusCheck(404))
            ));
            assert!(cache.get_encoded(&key, |_| true).await.unwrap().is_none());
        }
        assert_eq!(requests.load(Ordering::SeqCst), 4);
        server.abort();
    }
}
