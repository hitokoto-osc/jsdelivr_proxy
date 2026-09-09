//! Constant-time secret handling for the admin API and the cache webhook.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Compares two secrets without leaking their contents *or their lengths*
/// through timing. `==` on byte slices short-circuits on the first mismatch,
/// and a length check leaks the length before any comparison happens; hashing
/// both sides first makes every call compare exactly 32 bytes.
pub fn secret_eq(a: &[u8], b: &[u8]) -> bool {
    let a = Sha256::digest(a);
    let b = Sha256::digest(b);
    a.ct_eq(&b).into()
}

/// Verifies a GitHub-style `X-Hub-Signature-256` value against the raw request
/// body. Both `sha256=<hex>` and a bare `<hex>` digest are accepted.
///
/// `body` must be the exact bytes received: deserialising and re-serialising
/// the JSON first would change the byte sequence the sender signed.
pub fn verify_hmac_sha256(secret: &[u8], body: &[u8], signature: &str) -> bool {
    let signature = signature.trim();
    let digest = signature.strip_prefix("sha256=").unwrap_or(signature);
    let Ok(expected) = hex::decode(digest) else {
        return false;
    };
    // HMAC accepts a key of any length, so this cannot fail in practice.
    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    // `verify_slice` is itself constant-time and rejects a wrong-length digest.
    mac.verify_slice(&expected).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"secret";
    const BODY: &[u8] = b"payload";
    /// Well-formed hex of the right length, but not the MAC of `BODY`.
    const WRONG_DIGEST: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    #[test]
    fn secret_comparison_matches_equality() {
        assert!(secret_eq(b"abc", b"abc"));
        assert!(!secret_eq(b"abc", b"abd"));
        assert!(!secret_eq(b"abc", b"abcd"));
        assert!(!secret_eq(b"", b"a"));
        assert!(secret_eq(b"", b""));
    }

    #[test]
    fn signature_round_trips_through_the_verifier() {
        let mut mac = HmacSha256::new_from_slice(SECRET).unwrap();
        mac.update(BODY);
        let signature = hex::encode(mac.finalize().into_bytes());

        assert!(verify_hmac_sha256(SECRET, BODY, &signature));
        assert!(verify_hmac_sha256(
            SECRET,
            BODY,
            &format!("sha256={signature}")
        ));
        assert!(verify_hmac_sha256(
            SECRET,
            BODY,
            &format!("  sha256={signature}  ")
        ));
    }

    #[test]
    fn a_tampered_body_or_key_fails() {
        let mut mac = HmacSha256::new_from_slice(SECRET).unwrap();
        mac.update(BODY);
        let signature = hex::encode(mac.finalize().into_bytes());

        assert!(!verify_hmac_sha256(SECRET, b"payloaD", &signature));
        assert!(!verify_hmac_sha256(b"other", BODY, &signature));
    }

    #[test]
    fn malformed_signatures_are_rejected_rather_than_panicking() {
        for signature in ["", "sha256=", "not-hex", "sha1=deadbeef", "deadbeef"] {
            assert!(
                !verify_hmac_sha256(SECRET, BODY, signature),
                "signature {signature:?}"
            );
        }
        assert!(!verify_hmac_sha256(SECRET, BODY, WRONG_DIGEST));
    }
}
