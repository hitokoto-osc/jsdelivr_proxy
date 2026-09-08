pub mod allowlist;
pub mod types;
use axum::{
    extract::Path as PathParam,
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use reqwest::{Client, Url};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, instrument, warn};

use crate::cache::{self, CachedResource};
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

fn convert_url(base: &str, path: PathBuf) -> Result<Url, types::FetchJSDelivrFailureError> {
    let mut url = Url::parse(base)?;
    let mut path = match path.into_os_string().into_string() {
        Ok(v) => v,
        Err(_) => return Err(types::FetchJSDelivrFailureError::PathCovert),
    };
    let raw_path = url.path();
    if raw_path != "/" {
        path = match Path::new("/")
            .join(raw_path)
            .join(path)
            .into_os_string()
            .into_string()
        {
            Ok(v) => v,
            Err(_) => return Err(types::FetchJSDelivrFailureError::PathCovert),
        };
    }
    url.set_path(path.as_str());
    Ok(url)
}

async fn fetch_jsdelivr(
    path: PathBuf,
) -> Result<(String, Bytes), types::FetchJSDelivrFailureError> {
    let client = Client::builder()
        .user_agent(match &CONFIG.jsdelivr.user_agent {
            Some(v) => v,
            None => concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
        })
        .build()?;
    let mirror = match &CONFIG.jsdelivr.mirror {
        Some(v) => v,
        None => "https://gcore.jsdelivr.net",
    };
    let response = client
        .get(convert_url(mirror, path)?)
        .header(
            "Referer",
            match &CONFIG.jsdelivr.referer {
                Some(v) => v,
                None => mirror,
            },
        )
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(types::FetchJSDelivrFailureError::RequestStatusCheck(
            status.as_u16(),
        ));
    }
    // 由于只使用 GET 方法获取 JSDelivr CDN 的资源，因此 Content-Type 应该就是 Mime
    let mime: String = if let Some(value) = response.headers().get(reqwest::header::CONTENT_TYPE) {
        value.to_str()?.to_string()
    } else {
        "text/plain".to_string()
    };
    Ok((mime, response.bytes().await?))
}

/// 读缓存；未命中时回源 jsDelivr 并写入缓存。
///
/// 缓存键直接使用请求路径：进程内缓存不与任何其他业务共享 keyspace，
/// 不再需要 Redis 时代用 SHA-256 摘要来规避键名冲突与非法字符，
/// 省掉一次逐请求的哈希计算；键自身的字节数已计入缓存容量核算。
///
/// 错误由 moka 以 `Arc` 返回（同一键上被合并的并发请求共享同一个错误对象）。
async fn remember_jsdelivr_resource(
    path: PathBuf,
) -> Result<CachedResource, Arc<FetchJSDelivrFailureError>> {
    let key = path.to_string_lossy().into_owned();
    cache::get_or_fetch(key, async move {
        let (mime, data) = fetch_jsdelivr(path).await?;
        Ok::<_, FetchJSDelivrFailureError>(CachedResource { mime, data })
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

    match remember_jsdelivr_resource(PathBuf::from(path)).await {
        Ok(resource) => {
            let content_type =
                HeaderValue::from_str(resource.mime.as_str()).unwrap_or(FALLBACK_CONTENT_TYPE);
            JSDelivrResponse::Raw(Box::new((content_type, resource.data)))
        }
        Err(e) => {
            error!("{:?}", e);
            match e.as_ref() {
                types::FetchJSDelivrFailureError::ReqwestOperation(_) => {
                    JSDelivrResponse::Json(fail_with_message(500, None, e.to_string()))
                }
                types::FetchJSDelivrFailureError::RequestStatusCheck(status) => {
                    JSDelivrResponse::Json(fail(*status as i64, None))
                }
                _ => JSDelivrResponse::Json(fail_with_message(400, None, e.to_string())),
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
