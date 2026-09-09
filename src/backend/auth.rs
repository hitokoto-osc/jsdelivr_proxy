//! Admin-key authentication.
//!
//! Authentication is an extractor rather than a layer so that it shows up in
//! every protected handler's signature: a new admin route cannot be added
//! without deciding, in the type system, whether it is authenticated.

use axum::extract::FromRequestParts;
use axum::http::{header, request::Parts};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::utils::hash::secret_eq;
use crate::utils::response::fail_with_message;
use crate::CONFIG;

/// Proof that the request carried the configured admin key.
pub struct AdminAuth;

pub enum AuthError {
    /// No key is configured, so the admin API is off. Kept distinct from a
    /// wrong key because the operator who forgot the environment variable
    /// needs to see the difference, and there is no secret to leak here.
    Disabled,
    Missing,
    Invalid,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (code, message) = match self {
            AuthError::Disabled => (503, "the admin API is disabled: no admin key is configured"),
            AuthError::Missing => (401, "missing `Authorization: Bearer <admin key>`"),
            AuthError::Invalid => (401, "invalid admin key"),
        };
        fail_with_message::<Value>(code, None, message.to_string()).into_response()
    }
}

impl<S> FromRequestParts<S> for AdminAuth
where
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let Some(expected) = CONFIG.admin.key() else {
            return Err(AuthError::Disabled);
        };
        let presented = bearer_token(parts).ok_or(AuthError::Missing)?;
        if secret_eq(presented.as_bytes(), expected.as_bytes()) {
            Ok(AdminAuth)
        } else {
            Err(AuthError::Invalid)
        }
    }
}

/// The scheme is compared case-insensitively because RFC 7235 defines it that
/// way, and clients do send `bearer`.
fn bearer_token(parts: &Parts) -> Option<&str> {
    let value = parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(token.trim())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    fn parts_with(authorization: Option<&str>) -> Parts {
        let mut builder = Request::builder().uri("/admin/api/stats");
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        builder.body(()).unwrap().into_parts().0
    }

    #[test]
    fn a_bearer_token_is_extracted_whatever_the_scheme_casing() {
        assert_eq!(bearer_token(&parts_with(Some("Bearer key"))), Some("key"));
        assert_eq!(bearer_token(&parts_with(Some("bearer key"))), Some("key"));
        assert_eq!(bearer_token(&parts_with(Some("BEARER  key "))), Some("key"));
    }

    #[test]
    fn other_authorization_schemes_are_not_accepted() {
        assert!(bearer_token(&parts_with(None)).is_none());
        assert!(bearer_token(&parts_with(Some("Basic a2V5"))).is_none());
        assert!(bearer_token(&parts_with(Some("key"))).is_none());
    }

    #[test]
    fn rejections_carry_the_documented_status_codes() {
        assert_eq!(AuthError::Disabled.into_response().status(), 503);
        assert_eq!(AuthError::Missing.into_response().status(), 401);
        assert_eq!(AuthError::Invalid.into_response().status(), 401);
    }
}
