pub mod allowlist;
pub mod types;
use axum::{
    extract::Path as PathParam,
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use serde_json::Value;
use tracing::{error, instrument, warn};

use crate::cache::{self, CacheError, CachedResource};
use crate::upstream::{self, UpstreamError};
use crate::utils::response::{fail, fail_with_message, APIResponse};
use crate::CONFIG;

use self::types::FetchJSDelivrFailureError;

/// Rocket 的 `ContentType::Plain`，作为无法解析上游 Content-Type 时的回落值。
const FALLBACK_CONTENT_TYPE: HeaderValue = HeaderValue::from_static("text/plain; charset=utf-8");

pub enum JSDelivrResponse {
    Json(APIResponse<Value>),
    Raw(Box<(HeaderValue, Bytes)>),
}

impl IntoResponse for JSDelivrResponse {
    fn into_response(self) -> Response {
        match self {
            JSDelivrResponse::Json(v) => v.into_response(),
            JSDelivrResponse::Raw(raw) => {
                let (content_type, data) = *raw;
                ([(header::CONTENT_TYPE, content_type)], data).into_response()
            }
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

/// 读缓存；未命中时回源 jsDelivr 并写入缓存。
///
/// 缓存键直接使用请求路径：进程内缓存不与任何其他业务共享 keyspace，
/// 不再需要 Redis 时代用 SHA-256 摘要来规避键名冲突与非法字符，
/// 省掉一次逐请求的哈希计算；键自身的字节数已计入缓存容量核算。
///
/// 错误由 moka 以 `Arc` 返回（同一键上被合并的并发请求共享同一个错误对象）。
async fn remember_jsdelivr_resource(
    path: String,
) -> Result<CachedResource, CacheError<FetchJSDelivrFailureError>> {
    let key = path.clone();
    cache::get_or_fetch(key, async move {
        Ok::<_, FetchJSDelivrFailureError>(upstream::fetch(&path).await?)
    })
    .await
}

#[instrument]
pub async fn get(PathParam(path): PathParam<String>) -> JSDelivrResponse {
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

    match remember_jsdelivr_resource(path).await {
        Ok(resource) => {
            let content_type =
                HeaderValue::from_str(resource.mime.as_str()).unwrap_or(FALLBACK_CONTENT_TYPE);
            JSDelivrResponse::Raw(Box::new((content_type, resource.data)))
        }
        Err(e) => {
            error!("{:?}", e);
            match &e {
                // A body this process compressed itself failed to decompress:
                // the request is not at fault, so it cannot be a 4xx.
                CacheError::Decompress(_) => {
                    JSDelivrResponse::Json(fail_with_message(500, None, e.to_string()))
                }
                CacheError::Fetch(cause) => match cause.as_ref() {
                    FetchJSDelivrFailureError::Upstream(UpstreamError::ReqwestOperation(_)) => {
                        JSDelivrResponse::Json(fail_with_message(500, None, e.to_string()))
                    }
                    FetchJSDelivrFailureError::Upstream(UpstreamError::RequestStatusCheck(
                        status,
                    )) => JSDelivrResponse::Json(fail(*status as i64, None)),
                    _ => JSDelivrResponse::Json(fail_with_message(400, None, e.to_string())),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
