//! The error taxonomy the core presents to its foreign callers.
//!
//! This is deliberately *not* the generated client's error type. The generated
//! taxonomy is transport-shaped (request construction, decode, undocumented
//! status); a UI needs to know whether to offer "sign in again", "trust this
//! certificate", "retry", or "this file will not play on this device". The
//! mapping between the two lives in [`crate::transport`].

use crate::trust::CertificateDetails;

/// A failure the foreign caller can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum BeamError {
    /// No server has been selected, and the operation needs one.
    #[error("no active server")]
    NoActiveServer,

    /// The referenced server is not in the registry.
    #[error("unknown server: {server_id}")]
    UnknownServer {
        /// The identifier that did not resolve.
        server_id: String,
    },

    /// The supplied base URL is not a usable HTTP(S) origin.
    #[error("invalid server URL: {detail}")]
    InvalidServerUrl {
        /// Why the URL was rejected.
        detail: String,
    },

    /// There is no session for this server; the caller should sign in.
    #[error("not authenticated")]
    Unauthenticated,

    /// The session existed but the server has now rejected it.
    ///
    /// Distinct from [`BeamError::Unauthenticated`] because the UI response
    /// differs: an expired session interrupts work in progress and should
    /// preserve it, rather than sending the user back to a cold sign-in.
    #[error("session expired")]
    SessionExpired,

    /// Authenticated, but not permitted. Typically a non-admin calling an
    /// admin route.
    #[error("forbidden: {detail}")]
    Forbidden {
        /// The server's explanation.
        detail: String,
    },

    /// The resource does not exist, or was removed by a rescan.
    #[error("not found: {detail}")]
    NotFound {
        /// The server's explanation.
        detail: String,
    },

    /// The server rejected the request as malformed.
    #[error("bad request: {detail}")]
    BadRequest {
        /// The server's explanation.
        detail: String,
    },

    /// Rate limited. `beam-server` applies this to the browse/search class and
    /// to auth, and returns `Retry-After`.
    #[error("rate limited; retry in {retry_after_secs}s")]
    RateLimited {
        /// Seconds to wait, from `Retry-After` where the server sent one.
        retry_after_secs: u64,
    },

    /// The server failed to handle an otherwise valid request.
    #[error("server error {status}: {detail}")]
    Server {
        /// The HTTP status code.
        status: u16,
        /// The server's explanation, where it sent one.
        detail: String,
    },

    /// The request never produced a response.
    #[error("network error: {detail}")]
    Network {
        /// A human-readable cause.
        detail: String,
        /// Whether retrying the identical request could plausibly succeed.
        /// Drives whether the progress queue enqueues or drops.
        retryable: bool,
    },

    /// The server's certificate is not trusted, and the user has not pinned
    /// it. Carries everything the trust prompt needs to show.
    #[error("untrusted certificate for {host}")]
    UntrustedCertificate {
        /// The host whose certificate was rejected.
        host: String,
        /// What to show the user before they decide.
        details: CertificateDetails,
    },

    /// A response did not match the contract the client was generated from.
    #[error("malformed response: {detail}")]
    Protocol {
        /// What could not be decoded.
        detail: String,
    },

    /// Persistence provided by the foreign side failed.
    #[error("storage error: {detail}")]
    Storage {
        /// The foreign side's explanation.
        detail: String,
    },
}

impl BeamError {
    /// Whether retrying the identical request could plausibly succeed.
    ///
    /// Used by the playback-progress queue to decide between enqueueing and
    /// dropping. Deliberately conservative: an error whose retryability is
    /// unclear is treated as *not* retryable, so a permanently-failing sample
    /// cannot occupy the queue forever.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network { retryable, .. } => *retryable,
            Self::RateLimited { .. } => true,
            // 5xx is the server failing to handle a request it accepted;
            // the same request may well succeed later.
            Self::Server { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

/// A failure from the foreign side's key/value storage.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum StorageError {
    /// Storage could not be reached (disk full, database closed, and so on).
    #[error("storage unavailable: {detail}")]
    Unavailable {
        /// The foreign side's explanation.
        detail: String,
    },

    /// Storage refused the operation, typically a locked keystore.
    #[error("storage denied: {detail}")]
    Denied {
        /// The foreign side's explanation.
        detail: String,
    },
}

impl From<StorageError> for BeamError {
    fn from(error: StorageError) -> Self {
        Self::Storage {
            detail: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryability_follows_the_transport_verdict() {
        assert!(
            BeamError::Network {
                detail: "connection reset".to_owned(),
                retryable: true,
            }
            .is_retryable()
        );
        assert!(
            !BeamError::Network {
                detail: "invalid URL".to_owned(),
                retryable: false,
            }
            .is_retryable()
        );
    }

    #[test]
    fn server_errors_are_retryable_only_from_500_up() {
        for status in [500_u16, 502, 503] {
            assert!(
                BeamError::Server {
                    status,
                    detail: String::new(),
                }
                .is_retryable(),
                "{status} should be retryable"
            );
        }
        // A 4xx that reached the Server variant is still the caller's fault.
        assert!(
            !BeamError::Server {
                status: 418,
                detail: String::new(),
            }
            .is_retryable()
        );
    }

    #[test]
    fn rate_limiting_is_retryable_but_authentication_is_not() {
        assert!(
            BeamError::RateLimited {
                retry_after_secs: 30
            }
            .is_retryable()
        );
        assert!(!BeamError::SessionExpired.is_retryable());
        assert!(!BeamError::Unauthenticated.is_retryable());
        assert!(
            !BeamError::NotFound {
                detail: String::new()
            }
            .is_retryable()
        );
    }

    #[test]
    fn storage_failures_widen_into_the_core_taxonomy() {
        let widened: BeamError = StorageError::Denied {
            detail: "keystore locked".to_owned(),
        }
        .into();
        assert!(matches!(widened, BeamError::Storage { .. }));
        assert!(!widened.is_retryable());
    }
}
