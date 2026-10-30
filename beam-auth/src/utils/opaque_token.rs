//! The opaque credentials this crate mints -- session tokens and device
//! handles -- and the hash each is stored and looked up by.
//!
//! A credential is handed to its client once and never persisted: only
//! [`hash`] of it reaches the database, so a leaked table holds nothing a
//! client could present. [`seconds`] is the one conversion of the TTLs their
//! expiries are computed from.

use sha2::{Digest, Sha256};

/// A fresh opaque credential: 32 random bytes, URL-safe base64 without
/// padding.
#[must_use]
pub fn generate() -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::Rng;

    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The at-rest form of `credential`: its SHA-256, lowercase hex.
#[must_use]
pub fn hash(credential: &str) -> String {
    beam_domain::utils::hex::encode_lower(&Sha256::digest(credential.as_bytes()))
}

/// `secs` as a chrono duration, for adding a TTL or interval to an instant.
///
/// Clamped into `i64` rather than cast: `secs as i64` wraps a `u64` past
/// `i64::MAX` to a negative duration, an expiry in the past. Every caller
/// passes a configured TTL or a capped IdP value, far inside what a duration
/// holds.
pub(crate) fn seconds(secs: u64) -> chrono::Duration {
    chrono::Duration::seconds(i64::try_from(secs).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::{generate, hash};

    #[test]
    fn a_credential_is_32_bytes_of_url_safe_base64() {
        let credential = generate();
        // 32 bytes is 43 unpadded base64 characters.
        assert_eq!(credential.len(), 43, "{credential}");
        assert!(
            credential
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "only the URL-safe alphabet, no padding: {credential}"
        );
    }

    #[test]
    fn two_credentials_differ() {
        assert_ne!(generate(), generate());
    }

    #[test]
    fn a_token_hashes_to_the_lowercase_sha256_hex_of_its_bytes() {
        // Ground truth from `printf 'session-token-example' | sha256sum`, not
        // from this crate. Session rows are looked up by this string, so a
        // change to the digest or its encoding silently invalidates every
        // stored session -- the failure this test exists to catch.
        assert_eq!(
            hash("session-token-example"),
            "467bb7ff1ca961ab8a9bf090853e52da11c3ce8a4eddb0c7f57e3b67e9259505"
        );
    }

    #[test]
    fn the_plaintext_token_does_not_survive_hashing() {
        let hashed = hash("session-token-example");

        assert!(!hashed.contains("session-token"), "{hashed}");
        assert_eq!(hashed.len(), 64, "a SHA-256 digest is 64 hex characters");
    }
}
