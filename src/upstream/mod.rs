//! Upstream (jsDelivr or a mirror) access, shared by the request handler and
//! by preload.
//!
//! The HTTP client is a process-wide singleton. Building one per request, as
//! the previous implementation did, allocates a fresh connection pool and TLS
//! configuration every time and never reuses a connection.

use bytes::Bytes;
use reqwest::{Client, Url};
use thiserror::Error;
use url::ParseError;

use crate::cache::CachedResource;
use crate::CONFIG;

pub const DEFAULT_MIRROR: &str = "https://gcore.jsdelivr.net";

lazy_static! {
    /// `build()` only fails when the TLS backend cannot initialise, in which
    /// case every fetch is doomed anyway; failing at startup beats returning
    /// 500 from every request.
    static ref CLIENT: Client = Client::builder()
        .user_agent(user_agent())
        .build()
        .expect("failed to build the shared HTTP client");
}

#[derive(Error, Debug)]
pub enum UpstreamError {
    #[error("UpstreamUrlParse: {0}")]
    UrlParse(#[from] ParseError),
    #[error("ReqwestOperation failed: {0}")]
    ReqwestOperation(#[from] reqwest::Error),
    #[error("RequestStatusCheck failed: {0}")]
    RequestStatusCheck(u16),
    #[error("RequestContentTypeConvert: {0}")]
    RequestContentTypeConvert(#[from] reqwest::header::ToStrError),
}

fn user_agent() -> &'static str {
    match CONFIG.jsdelivr.user_agent.as_deref() {
        Some(v) => v,
        None => concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
    }
}

pub fn client() -> &'static Client {
    &CLIENT
}

pub fn mirror() -> &'static str {
    match CONFIG.jsdelivr.mirror.as_deref() {
        Some(v) => v,
        None => DEFAULT_MIRROR,
    }
}

fn referer() -> &'static str {
    match CONFIG.jsdelivr.referer.as_deref() {
        Some(v) => v,
        None => mirror(),
    }
}

/// A mirror may carry a base path (`https://example.com/cdn`), which rules out
/// `Url::join` because it would drop the last segment. `std::path` is no good
/// either: it produces backslashes on Windows.
fn convert_url(base: &str, path: &str) -> Result<Url, UpstreamError> {
    let mut url = Url::parse(base)?;
    let base_path = url.path().trim_end_matches('/').to_string();
    url.set_path(&format!("{}/{}", base_path, path.trim_start_matches('/')));
    Ok(url)
}

/// `path` is a jsDelivr path without a leading slash, e.g.
/// `gh/hitokoto-osc/sentences-bundle@HEAD/categories.json`.
pub async fn fetch(path: &str) -> Result<CachedResource, UpstreamError> {
    let response = fetch_response(path).await?;
    let mime: String = match response.headers().get(reqwest::header::CONTENT_TYPE) {
        Some(value) => value.to_str()?.to_string(),
        None => "text/plain".to_string(),
    };
    let data: Bytes = response.bytes().await?;
    Ok(CachedResource { mime, data })
}

pub async fn fetch_response(path: &str) -> Result<reqwest::Response, UpstreamError> {
    let response = CLIENT
        .get(convert_url(mirror(), path)?)
        .header("Referer", referer())
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(UpstreamError::RequestStatusCheck(status.as_u16()));
    }
    Ok(response)
}

pub async fn fetch_gravatar_response(
    client: &Client,
    base: &str,
    path: &str,
    query: Option<&str>,
) -> Result<reqwest::Response, UpstreamError> {
    let mut url = convert_url(base, path)?;
    url.set_query(query);
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(UpstreamError::RequestStatusCheck(
            response.status().as_u16(),
        ));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_the_path_to_a_bare_mirror() {
        let url = convert_url("https://gcore.jsdelivr.net", "npm/vue@3/dist/vue.js").unwrap();
        assert_eq!(
            url.as_str(),
            "https://gcore.jsdelivr.net/npm/vue@3/dist/vue.js"
        );
    }

    #[test]
    fn keeps_the_base_path_of_the_mirror() {
        for base in ["https://example.com/cdn", "https://example.com/cdn/"] {
            let url = convert_url(base, "npm/vue@3/dist/vue.js").unwrap();
            assert_eq!(
                url.as_str(),
                "https://example.com/cdn/npm/vue@3/dist/vue.js"
            );
        }
    }

    #[test]
    fn rejects_a_mirror_that_is_not_a_url() {
        assert!(convert_url("not a url", "npm/vue").is_err());
    }
}
