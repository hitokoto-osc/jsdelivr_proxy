//! Wire format of a purge request, shared by the admin API and the webhook.

use serde::Deserialize;

use crate::cache::purge::Scope;

/// Exactly one of the three fields selects what to purge.
#[derive(Debug, Default, Deserialize)]
pub struct PurgeRequest {
    #[serde(default)]
    pub all: bool,
    pub prefix: Option<String>,
    #[serde(default)]
    pub keys: Vec<String>,
}

impl PurgeRequest {
    /// Rejects a request that names more than one scope instead of picking
    /// one: silently ignoring `keys` because `all` was also set would purge
    /// far more than the caller asked for.
    pub fn into_scope(self) -> Result<Scope, &'static str> {
        let prefix = self
            .prefix
            .map(|prefix| prefix.trim().to_string())
            .filter(|prefix| !prefix.is_empty());
        let keys: Vec<String> = self
            .keys
            .into_iter()
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .collect();

        match (self.all, prefix, keys.is_empty()) {
            (true, None, true) => Ok(Scope::All),
            (false, Some(prefix), true) => Ok(Scope::Prefix(prefix)),
            (false, None, false) => Ok(Scope::Keys(keys)),
            (false, None, true) => Err("specify one of `all`, `prefix` or `keys`"),
            _ => Err("specify exactly one of `all`, `prefix` or `keys`"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Scope, &'static str> {
        serde_json::from_str::<PurgeRequest>(json)
            .expect("valid JSON")
            .into_scope()
    }

    #[test]
    fn each_field_selects_its_scope() {
        assert_eq!(parse(r#"{"all": true}"#), Ok(Scope::All));
        assert_eq!(
            parse(r#"{"prefix": "gh/owner/repo@HEAD"}"#),
            Ok(Scope::Prefix("gh/owner/repo@HEAD".into()))
        );
        assert_eq!(
            parse(r#"{"keys": ["npm/vue@3/dist/vue.js"]}"#),
            Ok(Scope::Keys(vec!["npm/vue@3/dist/vue.js".into()]))
        );
    }

    #[test]
    fn whitespace_only_values_count_as_absent() {
        assert!(parse(r#"{"prefix": "   "}"#).is_err());
        assert!(parse(r#"{"keys": ["", "  "]}"#).is_err());
        assert_eq!(
            parse(r#"{"keys": [" npm/lodash "]}"#),
            Ok(Scope::Keys(vec!["npm/lodash".into()]))
        );
    }

    #[test]
    fn an_empty_or_ambiguous_request_is_rejected() {
        assert!(parse("{}").is_err());
        assert!(parse(r#"{"all": false}"#).is_err());
        assert!(parse(r#"{"all": true, "prefix": "npm/vue"}"#).is_err());
        assert!(parse(r#"{"all": true, "keys": ["npm/vue"]}"#).is_err());
        assert!(parse(r#"{"prefix": "npm/vue", "keys": ["npm/lodash"]}"#).is_err());
    }
}
