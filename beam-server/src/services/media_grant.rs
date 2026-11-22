//! Signed, file-scoped media grants (ADR-0022).
//!
//! A grant lets a client that holds no `beam_session` cookie -- an AirPlay or
//! Cast receiver, an external player such as VLC -- fetch one file's bytes for
//! a bounded time. It is a capability, not a session: it names one file and
//! one user, it expires, and it is accepted by the four delivery operations
//! and nothing else. Access is still the user's policy, checked on every
//! request the grant is presented with, so a grant outlives no restriction.
//!
//! The token is `file_id || user_id || expiry || tag`, base64url without
//! padding, where `tag` is HMAC-SHA256 under a key the process draws at start
//! over a domain label and the first three fields. Nothing is stored: the
//! signature is the whole of the server's state, so verifying a grant costs
//! no read. The key is never persisted, so a restart voids every grant
//! outstanding -- acceptable for a credential that lives hours, and the one
//! way to revoke them all at once.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use uuid::Uuid;

/// How long a grant is honoured after it is issued.
///
/// Hours rather than minutes: a receiver fetches the file range by range for
/// as long as it plays, and every request is checked against the expiry, so a
/// grant must outlive the longest film it is likely to carry, pauses
/// included. A grant that expired mid-film would stop playback with no way
/// for the receiver to recover. Four hours is that, and still a sliver of a
/// session's lifetime (ADR-0022).
pub const MEDIA_GRANT_TTL_SECS: i64 = 4 * 60 * 60;

/// Separates this MAC's inputs from any other the key could ever sign.
const DOMAIN: &[u8] = b"beam/media-grant/v1";

/// `file_id`, `user_id` and the expiry's whole seconds, big-endian.
const CLAIMS_LEN: usize = 16 + 16 + 8;

/// An HMAC-SHA256 tag.
const TAG_LEN: usize = 32;

/// What a grant says, once its signature has been checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantClaims {
    /// The one file the grant may fetch.
    pub file_id: Uuid,
    /// The user it was issued to, whose access policy every request applies.
    pub user_id: Uuid,
    /// The first instant at which it is refused.
    pub expires_at: DateTime<Utc>,
}

/// Why a presented grant was refused. Every reason is the same 401 on the
/// wire; they are told apart only for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrantRefused {
    /// Not base64url, or not the length a grant is.
    #[error("the grant is malformed")]
    Malformed,
    /// Its tag is not the one this process's key gives its claims.
    #[error("the grant's signature does not verify")]
    BadSignature,
    /// Its expiry has passed.
    #[error("the grant has expired")]
    Expired,
}

/// A grant as issued: the token to put in a URL, and when it stops working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedGrant {
    /// The opaque value of the `grant` query parameter.
    pub token: String,
    /// The first instant at which it is refused.
    pub expires_at: DateTime<Utc>,
}

/// Issues and verifies grants under one key.
pub struct MediaGrantSigner {
    key: Vec<u8>,
}

/// Never the key: a key read from a log forges every grant until restart.
impl std::fmt::Debug for MediaGrantSigner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaGrantSigner")
            .finish_non_exhaustive()
    }
}

impl MediaGrantSigner {
    /// A signer under a fresh key of 32 random bytes.
    #[must_use]
    pub fn random() -> Self {
        Self {
            key: beam_auth::utils::opaque_token::generate().into_bytes(),
        }
    }

    /// A signer under `key`, so a test can mint a grant another signer did not.
    #[must_use]
    pub fn with_key(key: &[u8]) -> Self {
        Self { key: key.to_vec() }
    }

    /// A grant of `file_id` to `user_id`, honoured until
    /// [`MEDIA_GRANT_TTL_SECS`] after `now`, to the whole second.
    #[must_use]
    pub fn issue(&self, file_id: Uuid, user_id: Uuid, now: DateTime<Utc>) -> IssuedGrant {
        let expiry = now.timestamp().saturating_add(MEDIA_GRANT_TTL_SECS);
        let claims = encode_claims(file_id, user_id, expiry);
        let tag = self.tag(&claims);

        let mut token = Vec::with_capacity(CLAIMS_LEN + TAG_LEN);
        token.extend_from_slice(&claims);
        token.extend_from_slice(&tag);

        IssuedGrant {
            token: URL_SAFE_NO_PAD.encode(token),
            expires_at: instant(expiry),
        }
    }

    /// What `token` grants, if this signer issued it and it has not expired
    /// at `now`.
    ///
    /// The signature is checked before the expiry, in constant time, so a
    /// forged grant learns nothing from which of the two refused it.
    ///
    /// # Errors
    ///
    /// [`GrantRefused`], naming the first check that failed.
    pub fn verify(&self, token: &str, now: DateTime<Utc>) -> Result<GrantClaims, GrantRefused> {
        let bytes = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| GrantRefused::Malformed)?;
        if bytes.len() != CLAIMS_LEN + TAG_LEN {
            return Err(GrantRefused::Malformed);
        }
        let (claims, tag) = bytes.split_at(CLAIMS_LEN);

        let mut mac = self.mac();
        mac.update(claims);
        mac.verify_slice(tag)
            .map_err(|_| GrantRefused::BadSignature)?;

        let (file_id, user_id, expiry) = decode_claims(claims);
        if now.timestamp() >= expiry {
            return Err(GrantRefused::Expired);
        }

        Ok(GrantClaims {
            file_id,
            user_id,
            expires_at: instant(expiry),
        })
    }

    /// A MAC under this signer's key, already fed the domain label.
    fn mac(&self) -> Hmac<Sha256> {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.key)
            .expect("HMAC takes a key of any length");
        mac.update(DOMAIN);
        mac
    }

    fn tag(&self, claims: &[u8]) -> [u8; TAG_LEN] {
        let mut mac = self.mac();
        mac.update(claims);
        mac.finalize().into_bytes().into()
    }
}

fn encode_claims(file_id: Uuid, user_id: Uuid, expiry: i64) -> [u8; CLAIMS_LEN] {
    let mut claims = [0u8; CLAIMS_LEN];
    claims[..16].copy_from_slice(file_id.as_bytes());
    claims[16..32].copy_from_slice(user_id.as_bytes());
    claims[32..].copy_from_slice(&expiry.to_be_bytes());
    claims
}

/// [`encode_claims`] reversed. `claims` is exactly [`CLAIMS_LEN`] long.
fn decode_claims(claims: &[u8]) -> (Uuid, Uuid, i64) {
    let mut file_id = [0u8; 16];
    let mut user_id = [0u8; 16];
    let mut expiry = [0u8; 8];
    file_id.copy_from_slice(&claims[..16]);
    user_id.copy_from_slice(&claims[16..32]);
    expiry.copy_from_slice(&claims[32..]);
    (
        Uuid::from_bytes(file_id),
        Uuid::from_bytes(user_id),
        i64::from_be_bytes(expiry),
    )
}

/// `secs` since the epoch as an instant. Out of range only for an expiry no
/// signer issued, which verification has already refused by then.
fn instant(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).unwrap_or(DateTime::<Utc>::MAX_UTC)
}

#[cfg(test)]
#[path = "media_grant_tests.rs"]
mod media_grant_tests;
