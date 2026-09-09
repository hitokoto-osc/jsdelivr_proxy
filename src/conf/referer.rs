use axum::http::{header, HeaderMap};
use serde::Deserialize;
use url::Url;

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct RefererCheck {
    pub enabled: bool,
    pub allow_empty: bool,
    pub domains: Vec<String>,
}

impl RefererCheck {
    pub fn allows(&self, headers: &HeaderMap) -> bool {
        if !self.enabled {
            return true;
        }
        let mut values = headers.get_all(header::REFERER).iter();
        let Some(value) = values.next() else {
            return self.allow_empty;
        };
        if values.next().is_some() {
            return false;
        }
        let Ok(value) = value.to_str() else {
            return false;
        };
        if value.trim().is_empty() {
            return self.allow_empty;
        }
        let Ok(url) = Url::parse(value) else {
            return false;
        };
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some_and(|host| {
                self.domains
                    .iter()
                    .any(|domain| host.eq_ignore_ascii_case(domain.trim()))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::REFERER, value.parse().unwrap());
        headers
    }

    fn policy() -> RefererCheck {
        RefererCheck {
            enabled: true,
            domains: vec![" Example.COM ".into()],
            ..Default::default()
        }
    }

    #[test]
    fn disabled_preserves_existing_behavior() {
        let policy = RefererCheck::default();
        assert!(policy.allows(&HeaderMap::new()));
        assert!(policy.allows(&headers("invalid")));
        assert!(policy.allows(&headers("https://other.example/")));
    }

    #[test]
    fn empty_referer_is_controlled_separately() {
        let mut policy = policy();
        for headers in [HeaderMap::new(), headers(""), headers("   ")] {
            policy.allow_empty = false;
            assert!(!policy.allows(&headers));
            policy.allow_empty = true;
            assert!(policy.allows(&headers));
        }
        assert!(!policy.allows(&headers("invalid")));
        assert!(!policy.allows(&headers("https://other.example/")));
    }

    #[test]
    fn matches_only_the_complete_http_or_https_host() {
        let policy = policy();
        for value in [
            "https://example.com/",
            "http://EXAMPLE.COM/page?q=1",
            "https://example.com:8443/path",
        ] {
            assert!(policy.allows(&headers(value)), "{value}");
        }
        for value in [
            "https://example.com.evil.test/",
            "https://evil-example.com/",
            "https://sub.example.com/",
            "https://example.com@evil.test/",
            "https://evil.test/example.com",
            "https://evil.test/?host=example.com",
            "//example.com/path",
            "/path",
            "null",
            "ftp://example.com/",
            "data:text/plain,example.com",
        ] {
            assert!(!policy.allows(&headers(value)), "{value}");
        }
    }

    #[test]
    fn empty_domain_list_does_not_allow_nonempty_referers() {
        let policy = RefererCheck {
            enabled: true,
            allow_empty: true,
            ..Default::default()
        };
        assert!(!policy.allows(&headers("https://example.com/")));
        assert!(policy.allows(&HeaderMap::new()));
    }

    #[test]
    fn malformed_and_duplicate_headers_are_not_empty() {
        let mut policy = policy();
        policy.allow_empty = true;
        let mut headers = headers("https://example.com/");
        headers.append(header::REFERER, HeaderValue::from_static(""));
        assert!(!policy.allows(&headers));
        headers.insert(header::REFERER, HeaderValue::from_bytes(b"\xff").unwrap());
        assert!(!policy.allows(&headers));
    }
}
