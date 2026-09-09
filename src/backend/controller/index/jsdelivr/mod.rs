pub mod allowlist;
pub mod types;
use axum::{
    body::Body,
    extract::Path as PathParam,
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
};
use bytes::BytesMut;
use serde_json::Value;
use tokio::sync::{mpsc, OwnedMutexGuard};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, instrument, warn};

use crate::cache::{self, CachedResource, ResourceCache};
use crate::conf::cache::Compression;
use crate::upstream::{self, UpstreamError};
use crate::utils::response::{fail, fail_with_message, APIResponse};
use crate::CONFIG;

use self::types::FetchJSDelivrFailureError;

/// Rocket 的 `ContentType::Plain`，作为无法解析上游 Content-Type 时的回落值。
const FALLBACK_CONTENT_TYPE: HeaderValue = HeaderValue::from_static("text/plain; charset=utf-8");

pub enum JSDelivrResponse {
    Json(APIResponse<Value>),
    Raw(Response),
}

impl IntoResponse for JSDelivrResponse {
    fn into_response(self) -> Response {
        match self {
            JSDelivrResponse::Json(v) => v.into_response(),
            JSDelivrResponse::Raw(response) => response,
        }
    }
}

/// 校验通配符路由捕获到的路径。
///
/// Rocket 的 `PathBuf` 请求守卫会拒绝 `..` 等穿越片段，而 axum 的 `{*path}`
/// 不做任何过滤，因此这里必须显式实现同等强度的校验：
///
/// * 拒绝空路径、空片段（`//`、结尾 `/`）；
/// * 拒绝 `.` 与 `..` 片段；
/// * 拒绝百分号编码残留的 `%2e`（不区分大小写），防止二次编码绕过；
/// * 拒绝反斜杠与 NUL，避免不同平台下的路径语义差异。
fn validate_path(path: &str) -> Result<(), FetchJSDelivrFailureError> {
    if path.is_empty() {
        return Err(FetchJSDelivrFailureError::InvalidPath);
    }
    if path.contains('\\') || path.contains('\0') {
        return Err(FetchJSDelivrFailureError::InvalidPath);
    }
    // axum 已经做过一次百分号解码，若仍残留 %2e 说明客户端做了二次编码
    let lowered = path.to_ascii_lowercase();
    if lowered.contains("%2e") || lowered.contains("%2f") || lowered.contains("%5c") {
        return Err(FetchJSDelivrFailureError::InvalidPath);
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(FetchJSDelivrFailureError::InvalidPath);
        }
    }
    Ok(())
}

fn encoding_name(codec: Compression) -> Option<&'static str> {
    match codec {
        Compression::None => None,
        Compression::Zstd => Some("zstd"),
        Compression::Brotli => Some("br"),
    }
}

fn accepts_encoding(headers: &HeaderMap, codec: Compression) -> bool {
    let Some(encoding) = encoding_name(codec) else {
        return true;
    };
    let mut wildcard = false;
    let mut explicit = None;
    for value in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(value) = value.to_str() else { continue };
        for item in value.split(',') {
            let mut parts = item.split(';');
            let name = parts.next().unwrap().trim();
            let mut quality = 1.0_f32;
            for parameter in parts {
                if let Some((name, value)) = parameter.trim().split_once('=') {
                    if name.trim().eq_ignore_ascii_case("q") {
                        quality = value.trim().parse().unwrap_or(0.0);
                    }
                }
            }
            let accepted = quality > 0.0 && quality <= 1.0;
            if name.eq_ignore_ascii_case(encoding) {
                explicit = Some(accepted);
            } else if name == "*" {
                wildcard = accepted;
            }
        }
    }
    explicit.unwrap_or(wildcard)
}

fn resource_response(mime: &str, body: Body, codec: Compression) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime).unwrap_or(FALLBACK_CONTENT_TYPE),
    );
    headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    if let Some(encoding) = encoding_name(codec) {
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static(encoding));
    }
    response
}

fn stream_response(
    mut upstream: reqwest::Response,
    path: String,
    cache: ResourceCache,
    fetch_guard: OwnedMutexGuard<()>,
) -> Result<Response, UpstreamError> {
    let mime = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|value| value.to_str())
        .transpose()?
        .unwrap_or("text/plain")
        .to_string();
    let (sender, receiver) = mpsc::channel(8);
    let mut response = resource_response(
        &mime,
        Body::from_stream(ReceiverStream::new(receiver)),
        Compression::None,
    );
    *response.status_mut() = upstream.status();
    tokio::spawn(async move {
        let _fetch_guard = fetch_guard;
        let mut data = BytesMut::new();
        loop {
            match upstream.chunk().await {
                Ok(Some(chunk)) => {
                    data.extend_from_slice(&chunk);
                    // Bound the forwarding queue; a disconnected client must not cancel cache fill.
                    let _ = sender.send(Ok::<_, reqwest::Error>(chunk)).await;
                }
                Ok(None) => break,
                Err(error) => {
                    warn!(%path, %error, "upstream body download failed");
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            }
        }
        // Closing the response must not wait for cache compression or admission.
        drop(sender);
        cache
            .insert(
                path,
                CachedResource {
                    mime,
                    data: data.freeze(),
                },
            )
            .await;
    });
    Ok(response)
}

async fn remember_jsdelivr_resource(
    path: String,
    headers: &HeaderMap,
) -> Result<Response, anyhow::Error> {
    let cache = cache::shared();
    if let Some((resource, codec)) = cache
        .get_encoded(&path, |codec| accepts_encoding(headers, codec))
        .await?
    {
        return Ok(resource_response(
            &resource.mime,
            Body::from(resource.data),
            codec,
        ));
    }
    let fetch_guard = cache.lock_fetch(&path).await;
    // Another request may have filled the cache while this one waited.
    if let Some((resource, codec)) = cache
        .get_encoded(&path, |codec| accepts_encoding(headers, codec))
        .await?
    {
        return Ok(resource_response(
            &resource.mime,
            Body::from(resource.data),
            codec,
        ));
    }
    Ok(stream_response(
        upstream::fetch_response(&path).await?,
        path,
        cache,
        fetch_guard,
    )?)
}

#[instrument(skip(headers))]
pub async fn get(PathParam(path): PathParam<String>, headers: HeaderMap) -> Response {
    let policy = &CONFIG.jsdelivr.referer_check;
    let mut response = if policy.allows(&headers) {
        get_resource(path, headers).await.into_response()
    } else {
        warn!("referer check denied path {:?}", path);
        fail_with_message::<Value>(403, None, "Referer not allowed".into()).into_response()
    };
    if policy.enabled {
        // Downstream caches must not reuse an allowed response for another Referer.
        response
            .headers_mut()
            .append(header::VARY, HeaderValue::from_static("Referer"));
    }
    response
}

async fn get_resource(path: String, headers: HeaderMap) -> JSDelivrResponse {
    if let Err(e) = validate_path(&path) {
        error!("{:?}", e);
        return JSDelivrResponse::Json(fail_with_message(400, None, e.to_string()));
    }

    // 白名单校验必须发生在读缓存与回源之前：被拒绝的请求不应该消耗任何上游流量，
    // 也不应该在缓存里留下条目。
    if let Err(e) = allowlist::check(&CONFIG.jsdelivr.allowlist, &path) {
        warn!("allowlist denied path {:?}: {}", path, e);
        return JSDelivrResponse::Json(fail_with_message(403, None, e.to_string()));
    }

    match remember_jsdelivr_resource(path, &headers).await {
        Ok(response) => JSDelivrResponse::Raw(response),
        Err(e) => {
            error!("{:?}", e);
            match e.downcast_ref::<UpstreamError>() {
                Some(UpstreamError::RequestStatusCheck(status)) => {
                    JSDelivrResponse::Json(fail(*status as i64, None))
                }
                Some(UpstreamError::UrlParse(_) | UpstreamError::RequestContentTypeConvert(_)) => {
                    JSDelivrResponse::Json(fail_with_message(400, None, e.to_string()))
                }
                _ => JSDelivrResponse::Json(fail_with_message(500, None, e.to_string())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::{http::Request, routing::get, Router};
    use bytes::Bytes;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::oneshot,
        time::{timeout, Duration},
    };
    use tokio_stream::StreamExt;
    use tower::ServiceExt;
    use tower_http::compression::CompressionLayer;

    fn test_cache(codec: Compression) -> ResourceCache {
        ResourceCache::new(&crate::conf::cache::Cache {
            compression: codec,
            ..Default::default()
        })
    }

    async fn slow_upstream() -> (reqwest::Response, oneshot::Sender<bool>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (release, finish) = oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\n\r\nhello").await.unwrap();
            if finish.await.unwrap_or(false) {
                socket.write_all(b"world").await.unwrap();
            }
        });
        let response = reqwest::Client::new()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .unwrap();
        (response, release)
    }

    #[tokio::test]
    async fn miss_streams_before_completion_and_caches_after_disconnect() {
        let cache = test_cache(Compression::None);
        let (upstream, release) = slow_upstream().await;
        let guard = cache.lock_fetch("asset").await;
        let response = stream_response(upstream, "asset".into(), cache.clone(), guard).unwrap();
        let mut body = response.into_body().into_data_stream();
        assert_eq!(
            timeout(Duration::from_secs(2), body.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            "hello"
        );
        assert!(cache
            .get_encoded("asset", |_| true)
            .await
            .unwrap()
            .is_none());
        assert!(
            timeout(Duration::from_millis(20), cache.lock_fetch("asset"))
                .await
                .is_err()
        );
        drop(body);
        release.send(true).unwrap();
        let _guard = timeout(Duration::from_secs(2), cache.lock_fetch("asset"))
            .await
            .unwrap();
        let (resource, _) = cache.get_encoded("asset", |_| true).await.unwrap().unwrap();
        assert_eq!(resource.data, "helloworld");
    }

    #[tokio::test]
    async fn truncated_upstream_errors_the_stream_and_is_not_cached() {
        let cache = test_cache(Compression::None);
        let (upstream, release) = slow_upstream().await;
        let guard = cache.lock_fetch("asset").await;
        let response = stream_response(upstream, "asset".into(), cache.clone(), guard).unwrap();
        release.send(false).unwrap();
        assert!(axum::body::to_bytes(response.into_body(), 1024)
            .await
            .is_err());
        let _guard = timeout(Duration::from_secs(2), cache.lock_fetch("asset"))
            .await
            .unwrap();
        assert!(cache
            .get_encoded("asset", |_| true)
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn encoding_negotiation_respects_quality_wildcards_and_multiple_fields() {
        for (value, expected) in [
            ("zstd", true),
            ("gzip, ZSTD; q=0.5", true),
            ("zstd;q=0, *;q=1", false),
            ("*;q=0", false),
            ("*;q=0.5", true),
            ("", false),
            ("gzip", false),
            ("zstd;q=invalid", false),
            ("zstd;q=2", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT_ENCODING, value.parse().unwrap());
            assert_eq!(
                accepts_encoding(&headers, Compression::Zstd),
                expected,
                "{value}"
            );
        }
        let mut headers = HeaderMap::new();
        assert!(!accepts_encoding(&headers, Compression::Zstd));
        headers.append(header::ACCEPT_ENCODING, "gzip".parse().unwrap());
        headers.append(header::ACCEPT_ENCODING, "br, zstd".parse().unwrap());
        assert!(accepts_encoding(&headers, Compression::Zstd));
        assert!(accepts_encoding(&headers, Compression::Brotli));
    }

    #[tokio::test]
    async fn cached_encodings_pass_through_and_fallback_uses_global_compression() {
        let original = Bytes::from("console.log('streaming');".repeat(1024));
        for codec in [Compression::Zstd, Compression::Brotli] {
            let cache = test_cache(codec);
            cache
                .insert(
                    "asset".into(),
                    CachedResource {
                        mime: "application/javascript".into(),
                        data: original.clone(),
                    },
                )
                .await;
            let (stored, stored_codec) =
                cache.get_encoded("asset", |_| true).await.unwrap().unwrap();
            assert_eq!(stored_codec, codec);
            assert!(stored.data.len() < original.len());
            let app = Router::new()
                .route(
                    "/",
                    get(move |headers: HeaderMap| {
                        let cache = cache.clone();
                        async move {
                            let (resource, codec) = cache
                                .get_encoded("asset", |codec| accepts_encoding(&headers, codec))
                                .await
                                .unwrap()
                                .unwrap();
                            resource_response(&resource.mime, Body::from(resource.data), codec)
                        }
                    }),
                )
                .layer(CompressionLayer::new());
            for encoding in [encoding_name(codec).unwrap(), "deflate", "identity"] {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri("/")
                            .header(header::ACCEPT_ENCODING, encoding)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert!(response.headers().get_all(header::VARY).iter().any(|v| v
                    .to_str()
                    .unwrap()
                    .to_ascii_lowercase()
                    .contains("accept-encoding")));
                if encoding == "identity" {
                    assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
                } else {
                    assert_eq!(response.headers()[header::CONTENT_ENCODING], encoding);
                }
                let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                if encoding == encoding_name(codec).unwrap() {
                    assert_eq!(body, stored.data);
                }
                if encoding == "identity" {
                    assert_eq!(body, original);
                } else if encoding == "deflate" {
                    let mut decoded = Vec::new();
                    std::io::Read::read_to_end(
                        &mut flate2::read::ZlibDecoder::new(body.as_ref()),
                        &mut decoded,
                    )
                    .unwrap();
                    assert_eq!(decoded, original);
                }
            }
        }
    }

    #[test]
    fn accepts_normal_jsdelivr_paths() {
        for path in [
            "npm/vue@3/dist/vue.global.js",
            "gh/jquery/jquery@3.6.0/dist/jquery.min.js",
            "npm/lodash",
            "npm/@scope/pkg@1.0.0/index.js",
        ] {
            assert!(
                validate_path(path).is_ok(),
                "expected {} to be accepted",
                path
            );
        }
    }

    #[test]
    fn rejects_path_traversal() {
        for path in [
            "..",
            "../etc/passwd",
            "npm/../../etc/passwd",
            "npm/..",
            "npm/./vue",
            ".",
            "npm//vue",
            "npm/vue/",
            "",
            "npm\\..\\etc",
            "npm/%2e%2e/%2e%2e/etc/passwd",
            "npm/%2E%2E/etc/passwd",
            "npm/%2f/etc",
        ] {
            assert!(
                validate_path(path).is_err(),
                "expected {} to be rejected",
                path
            );
        }
    }

    #[test]
    fn rejected_path_is_reported_as_invalid_path() {
        let err = validate_path("npm/../../etc/passwd").unwrap_err();
        assert!(matches!(err, FetchJSDelivrFailureError::InvalidPath));
    }
}
