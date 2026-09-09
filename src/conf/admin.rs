use serde::Deserialize;

/// Admin panel, admin API and cache-purge webhook.
///
/// Every capability here is gated on a secret and is off until one is supplied:
/// no `key` disables the admin API and the panel, no `webhook_secret` disables
/// the webhook. A deployment that never touches this section behaves exactly as
/// it did before the section existed.
#[derive(Deserialize, Debug, Default)]
pub struct Admin {
    /// Shared admin key, sent as `Authorization: Bearer <key>`.
    pub key: Option<String>,
    /// HMAC-SHA256 key for `X-Hub-Signature-256`. Deliberately separate from
    /// `key`: a leaked webhook secret must not also grant admin access.
    pub webhook_secret: Option<String>,
    #[serde(default)]
    pub turso: Turso,
}

/// Remote storage for the audit log. Absent = the log lives in a bounded
/// in-memory buffer and is lost on restart.
#[derive(Deserialize, Debug, Default)]
pub struct Turso {
    /// Database URL. The `libsql://` and `turso://` schemes are SDK-level
    /// aliases and are rewritten to `https://`, which is what the HTTP API
    /// actually speaks.
    pub url: Option<String>,
    pub token: Option<String>,
}

/// Treats an unset value and a blank one alike: `KEY=` in an env file must not
/// count as "configured", or a deployment would silently enable the admin API
/// with an empty password.
fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

impl Admin {
    pub fn key(&self) -> Option<&str> {
        non_empty(&self.key)
    }

    pub fn webhook_secret(&self) -> Option<&str> {
        non_empty(&self.webhook_secret)
    }

    pub fn is_enabled(&self) -> bool {
        self.key().is_some()
    }

    pub fn is_webhook_enabled(&self) -> bool {
        self.webhook_secret().is_some()
    }
}

impl Turso {
    pub fn token(&self) -> Option<&str> {
        non_empty(&self.token)
    }

    /// Base URL with no trailing slash, ready to have an API path appended.
    pub fn endpoint(&self) -> Option<String> {
        let url = non_empty(&self.url)?;
        let normalised = match url.split_once("://") {
            // `http://` is preserved so a self-hosted sqld can be reached
            // without TLS; every other scheme means Turso Cloud, which is
            // HTTPS-only.
            Some(("http", rest)) => format!("http://{rest}"),
            Some((_, rest)) => format!("https://{rest}"),
            None => format!("https://{url}"),
        };
        Some(normalised.trim_end_matches('/').to_string())
    }

    pub fn is_configured(&self) -> bool {
        self.endpoint().is_some() && self.token().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turso(url: &str) -> Turso {
        Turso {
            url: Some(url.into()),
            token: Some("token".into()),
        }
    }

    #[test]
    fn blank_secrets_do_not_enable_anything() {
        let admin = Admin {
            key: Some("   ".into()),
            webhook_secret: Some("".into()),
            ..Default::default()
        };
        assert!(!admin.is_enabled());
        assert!(!admin.is_webhook_enabled());
        assert!(!Admin::default().is_enabled());
    }

    #[test]
    fn secrets_are_trimmed() {
        let admin = Admin {
            key: Some("  s3cret \n".into()),
            webhook_secret: Some(" hook ".into()),
            ..Default::default()
        };
        assert_eq!(admin.key(), Some("s3cret"));
        assert_eq!(admin.webhook_secret(), Some("hook"));
    }

    #[test]
    fn sdk_schemes_are_rewritten_to_https() {
        for url in [
            "libsql://db-org.turso.io",
            "turso://db-org.turso.io",
            "https://db-org.turso.io",
            "db-org.turso.io",
            "https://db-org.turso.io/",
        ] {
            assert_eq!(
                turso(url).endpoint().as_deref(),
                Some("https://db-org.turso.io"),
                "url {url:?}"
            );
        }
    }

    #[test]
    fn plain_http_survives_for_self_hosted_sqld() {
        assert_eq!(
            turso("http://127.0.0.1:8080").endpoint().as_deref(),
            Some("http://127.0.0.1:8080")
        );
    }

    #[test]
    fn turso_needs_both_url_and_token() {
        assert!(!Turso::default().is_configured());
        assert!(!Turso {
            url: Some("libsql://db.turso.io".into()),
            token: None,
        }
        .is_configured());
        assert!(turso("libsql://db.turso.io").is_configured());
    }
}
