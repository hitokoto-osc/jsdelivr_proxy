use serde::Deserialize;

use crate::preload::listing::DEFAULT_DATA_API;

/// jsDelivr resolves `HEAD` to the repository's default branch.
///
/// Note that `latest` means the newest *tag* to jsDelivr, which is a different
/// thing; this project normalises gh `latest` to `HEAD`, so reaching the newest
/// tag needs an explicit tag or semver range.
pub const GH_DEFAULT_VERSION: &str = "HEAD";

pub const NPM_DEFAULT_VERSION: &str = "latest";

/// An absent section, or an empty target list, disables preloading.
#[derive(Deserialize, Debug)]
pub struct Preload {
    /// Defaults to whether `targets` is non-empty.
    pub enabled: Option<bool>,
    /// Root of the jsDelivr data API, a different service from
    /// `jsdelivr.mirror`: mirrors serve assets but not `/v1/packages`.
    pub data_api: Option<String>,
    /// Must stay below `cache.ttl_secs`, or warmed entries expire between
    /// rounds. Defaults to three quarters of it.
    pub refresh_interval_secs: Option<u64>,
    #[serde(default = "Preload::default_concurrency")]
    pub concurrency: usize,
    /// Keeps a repository with tens of thousands of files from flushing the
    /// whole cache budget. Overridable per target.
    #[serde(default = "Preload::default_max_files")]
    pub max_files: usize,
    /// An array of tables cannot be expressed through environment variables,
    /// so targets are config-file only.
    #[serde(default)]
    pub targets: Vec<Target>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Target {
    /// `npm` or `gh`.
    pub provider: String,
    /// An npm package name, possibly scoped, or a GitHub `owner/repo`.
    pub name: String,
    /// Version, branch, tag or commit. Ends up verbatim in the cache key, so
    /// it must match the version string clients request.
    pub version: Option<String>,
    /// Extensions without the dot, case-insensitive. Empty uses the built-in
    /// frontend set; `"*"` disables extension filtering.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Path prefixes to keep, compared per segment: `/dist` matches
    /// `/dist/vue.js` but not `/dist-old/x.js`. Empty keeps everything.
    #[serde(default)]
    pub include: Vec<String>,
    /// Path prefixes to drop, matched like `include` and taking priority.
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub include_hidden: bool,
    pub max_files: Option<usize>,
}

/// Hand-written because `#[serde(default = "...")]` only applies while
/// deserializing; an absent `[preload]` section goes through `Default` instead,
/// and the two must agree.
impl Default for Preload {
    fn default() -> Self {
        Preload {
            enabled: None,
            data_api: None,
            refresh_interval_secs: None,
            concurrency: Preload::default_concurrency(),
            max_files: Preload::default_max_files(),
            targets: Vec::new(),
        }
    }
}

impl Preload {
    fn default_concurrency() -> usize {
        4
    }

    fn default_max_files() -> usize {
        512
    }

    pub fn is_enabled(&self) -> bool {
        !self.targets.is_empty() && self.enabled.unwrap_or(true)
    }

    pub fn data_api(&self) -> &str {
        match self.data_api.as_deref().map(str::trim) {
            Some(v) if !v.is_empty() => v,
            _ => DEFAULT_DATA_API,
        }
    }

    /// Three quarters of the cache TTL by default, so entries are renewed
    /// before they expire. The 60s floor keeps a `0` from busy-looping.
    pub fn refresh_interval_secs(&self, cache_ttl_secs: u64) -> u64 {
        self.refresh_interval_secs
            .filter(|v| *v > 0)
            .unwrap_or_else(|| cache_ttl_secs.saturating_mul(3) / 4)
            .max(60)
    }

    pub fn concurrency(&self) -> usize {
        self.concurrency.max(1)
    }

    pub fn max_files(&self, target: &Target) -> usize {
        target.max_files.unwrap_or(self.max_files)
    }
}

impl Target {
    pub fn provider(&self) -> String {
        self.provider.trim().to_ascii_lowercase()
    }

    pub fn name(&self) -> &str {
        self.name.trim()
    }

    fn is_gh(&self) -> bool {
        self.provider() == "gh"
    }

    /// The version string used in the cache key and the upstream URL.
    pub fn version_spec(&self) -> String {
        let configured = self
            .version
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        match configured {
            Some(v) if self.is_gh() && v.eq_ignore_ascii_case("latest") => {
                GH_DEFAULT_VERSION.to_string()
            }
            Some(v) => v.to_string(),
            None if self.is_gh() => GH_DEFAULT_VERSION.to_string(),
            None => NPM_DEFAULT_VERSION.to_string(),
        }
    }

    /// Cache-key prefix, e.g. `gh/hitokoto-osc/sentences-bundle@HEAD`.
    pub fn prefix(&self) -> String {
        format!(
            "{}/{}@{}",
            self.provider(),
            self.name(),
            self.version_spec()
        )
    }

    pub fn is_valid(&self) -> bool {
        let provider = self.provider();
        if provider.is_empty() || self.name().is_empty() {
            return false;
        }
        // The data API only lists npm and gh; wp, esm and friends have no
        // file listing at all.
        matches!(provider.as_str(), "npm" | "gh")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(provider: &str, name: &str, version: Option<&str>) -> Target {
        Target {
            provider: provider.into(),
            name: name.into(),
            version: version.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn disabled_without_targets() {
        assert!(!Preload::default().is_enabled());
    }

    /// An absent `[preload]` and an empty one must behave identically.
    #[test]
    fn manual_default_matches_the_serde_defaults() {
        let from_default = Preload::default();
        let from_empty_table: Preload = toml_like_empty_section();
        assert_eq!(from_default.concurrency, from_empty_table.concurrency);
        assert_eq!(from_default.max_files, from_empty_table.max_files);
        assert_eq!(from_default.concurrency, 4);
        assert_eq!(from_default.max_files, 512);
    }

    fn toml_like_empty_section() -> Preload {
        serde_json::from_str("{}").expect("an empty section must deserialize")
    }

    #[test]
    fn enabled_flag_can_keep_targets_but_turn_preload_off() {
        let preload = Preload {
            enabled: Some(false),
            targets: vec![target("gh", "a/b", None)],
            ..Default::default()
        };
        assert!(!preload.is_enabled());
    }

    #[test]
    fn gh_defaults_to_the_repository_default_branch() {
        assert_eq!(target("gh", "a/b", None).version_spec(), "HEAD");
        assert_eq!(target("gh", "a/b", Some("latest")).version_spec(), "HEAD");
        assert_eq!(target("GH", "a/b", Some("LATEST")).version_spec(), "HEAD");
        assert_eq!(target("gh", "a/b", Some("master")).version_spec(), "master");
        assert_eq!(target("gh", "a/b", Some("v1.2.3")).version_spec(), "v1.2.3");
    }

    #[test]
    fn npm_defaults_to_latest_and_keeps_it_verbatim() {
        assert_eq!(target("npm", "vue", None).version_spec(), "latest");
        assert_eq!(
            target("npm", "vue", Some("latest")).version_spec(),
            "latest"
        );
        assert_eq!(
            target("npm", "vue", Some("3.5.42")).version_spec(),
            "3.5.42"
        );
    }

    #[test]
    fn blank_version_is_treated_as_omitted() {
        assert_eq!(target("gh", "a/b", Some("   ")).version_spec(), "HEAD");
        assert_eq!(target("npm", "vue", Some("")).version_spec(), "latest");
    }

    #[test]
    fn prefix_is_the_cache_key_prefix() {
        assert_eq!(
            target("gh", "hitokoto-osc/sentences-bundle", None).prefix(),
            "gh/hitokoto-osc/sentences-bundle@HEAD"
        );
        assert_eq!(
            target("npm", "@hitokoto/core", Some("1.0.0")).prefix(),
            "npm/@hitokoto/core@1.0.0"
        );
    }

    #[test]
    fn only_npm_and_gh_can_be_listed() {
        assert!(target("npm", "vue", None).is_valid());
        assert!(target("gh", "a/b", None).is_valid());
        assert!(!target("wp", "some-plugin", None).is_valid());
        assert!(!target("npm", "  ", None).is_valid());
        assert!(!target("", "vue", None).is_valid());
    }

    #[test]
    fn refresh_interval_stays_below_the_cache_ttl() {
        let preload = Preload::default();
        assert_eq!(preload.refresh_interval_secs(7200), 5400);
        assert_eq!(preload.refresh_interval_secs(10), 60);
        assert_eq!(
            Preload {
                refresh_interval_secs: Some(0),
                ..Default::default()
            }
            .refresh_interval_secs(7200),
            5400
        );
        assert_eq!(
            Preload {
                refresh_interval_secs: Some(900),
                ..Default::default()
            }
            .refresh_interval_secs(7200),
            900
        );
    }

    #[test]
    fn per_target_max_files_overrides_the_global_one() {
        let preload = Preload::default();
        let mut t = target("gh", "a/b", None);
        assert_eq!(preload.max_files(&t), 512);
        t.max_files = Some(10);
        assert_eq!(preload.max_files(&t), 10);
    }
}
