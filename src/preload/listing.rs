//! jsDelivr data API client, listing the files of one package version.
//!
//! The listing endpoint only accepts concrete versions: `npm/vue@latest` is
//! rejected with "Make sure you use a specific version number, and not a
//! version range or an npm tag", so dist-tags and semver ranges have to go
//! through `/resolved?specifier=` first. Git branches (including `HEAD`) are
//! the mirror image: the listing accepts them, but `/resolved` returns
//! `version: null` for them.
//!
//! Hence list first and only resolve on a 404, which covers both forms.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tracing::debug;
use url::Url;

use crate::upstream;

pub const DEFAULT_DATA_API: &str = "https://data.jsdelivr.com";

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteFile {
    /// Repository path, leading with `/`.
    pub name: String,
    /// Base64 SHA-256, used to skip re-downloading unchanged files.
    #[serde(default)]
    pub hash: Option<String>,
    pub size: u64,
}

#[derive(Debug, Deserialize)]
struct FlatListing {
    #[serde(default)]
    files: Vec<RemoteFile>,
}

#[derive(Debug, Deserialize)]
struct Resolved {
    version: Option<String>,
}

fn base(data_api: &str) -> &str {
    data_api.trim_end_matches('/')
}

pub fn listing_url(data_api: &str, provider: &str, name: &str, spec: &str) -> String {
    format!(
        "{}/v1/packages/{}/{}@{}?structure=flat",
        base(data_api),
        provider,
        name,
        spec
    )
}

pub fn resolve_url(data_api: &str, provider: &str, name: &str) -> String {
    format!(
        "{}/v1/packages/{}/{}/resolved",
        base(data_api),
        provider,
        name
    )
}

pub async fn list(
    data_api: &str,
    provider: &str,
    name: &str,
    spec: &str,
) -> Result<Vec<RemoteFile>> {
    if let Some(files) = try_list(data_api, provider, name, spec).await? {
        return Ok(files);
    }

    // Not directly listable, so most likely a dist-tag or a semver range.
    let concrete = resolve(data_api, provider, name, spec)
        .await?
        .ok_or_else(|| anyhow!("`{}` is neither listable nor resolvable", spec))?;
    debug!(spec, resolved = %concrete, "specifier resolved to a concrete version");

    try_list(data_api, provider, name, &concrete)
        .await?
        .ok_or_else(|| anyhow!("resolved version `{}` has no listing", concrete))
}

/// `Ok(None)` means the upstream answered 404, as opposed to a transport
/// error, which must not trigger the resolve fallback.
async fn try_list(
    data_api: &str,
    provider: &str,
    name: &str,
    spec: &str,
) -> Result<Option<Vec<RemoteFile>>> {
    let url = listing_url(data_api, provider, name, spec);
    let response = upstream::client()
        .get(&url)
        .send()
        .await
        .with_context(|| format!("failed to request the file listing: {}", url))?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!(
            "listing endpoint returned {}: {}",
            status.as_u16(),
            url
        ));
    }

    let listing: FlatListing = response
        .json()
        .await
        .with_context(|| format!("failed to parse the file listing: {}", url))?;
    Ok(Some(listing.files))
}

async fn resolve(data_api: &str, provider: &str, name: &str, spec: &str) -> Result<Option<String>> {
    // Let `url` encode the query: `^` and `>` in semver ranges must be escaped.
    let mut url = Url::parse(&resolve_url(data_api, provider, name))
        .with_context(|| format!("data API address is not a valid URL: {}", data_api))?;
    url.query_pairs_mut().append_pair("specifier", spec);

    let response = upstream::client()
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("failed to resolve the version: {}", url))?;

    if !response.status().is_success() {
        return Ok(None);
    }
    let resolved: Resolved = response
        .json()
        .await
        .with_context(|| format!("failed to parse the resolve response: {}", url))?;
    Ok(resolved.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_flat_listing_url() {
        assert_eq!(
            listing_url(
                DEFAULT_DATA_API,
                "gh",
                "hitokoto-osc/sentences-bundle",
                "HEAD"
            ),
            "https://data.jsdelivr.com/v1/packages/gh/hitokoto-osc/sentences-bundle@HEAD?structure=flat"
        );
        assert_eq!(
            listing_url(DEFAULT_DATA_API, "npm", "@hitokoto/core", "1.0.0"),
            "https://data.jsdelivr.com/v1/packages/npm/@hitokoto/core@1.0.0?structure=flat"
        );
    }

    #[test]
    fn trailing_slash_on_the_data_api_does_not_double_up() {
        assert_eq!(
            listing_url("https://data.jsdelivr.com/", "npm", "vue", "3.5.42"),
            "https://data.jsdelivr.com/v1/packages/npm/vue@3.5.42?structure=flat"
        );
        assert_eq!(
            resolve_url("https://data.jsdelivr.com/", "npm", "vue"),
            "https://data.jsdelivr.com/v1/packages/npm/vue/resolved"
        );
    }

    #[test]
    fn parses_the_flat_listing_payload() {
        let payload = r#"{
            "type": "gh",
            "name": "hitokoto-osc/sentences-bundle",
            "version": "HEAD",
            "default": null,
            "files": [
                { "name": "/.gitignore", "hash": "TVQ8", "size": 1610 },
                { "name": "/categories.json", "hash": "JtM5", "size": 2741 }
            ]
        }"#;
        let listing: FlatListing = serde_json::from_str(payload).unwrap();
        assert_eq!(listing.files.len(), 2);
        assert_eq!(listing.files[1].name, "/categories.json");
        assert_eq!(listing.files[1].hash.as_deref(), Some("JtM5"));
        assert_eq!(listing.files[1].size, 2741);
    }

    /// A branch ref resolves to `version: null`, which is not an error.
    #[test]
    fn parses_an_unresolvable_specifier() {
        let resolved: Resolved =
            serde_json::from_str(r#"{"type":"gh","name":"a/b","version":null,"links":{}}"#)
                .unwrap();
        assert_eq!(resolved.version, None);

        let resolved: Resolved =
            serde_json::from_str(r#"{"type":"npm","name":"vue","version":"3.5.42"}"#).unwrap();
        assert_eq!(resolved.version.as_deref(), Some("3.5.42"));
    }
}
