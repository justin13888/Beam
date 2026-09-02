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
        /// The problem document's `type`. See [`BeamError::NotFound`].
        code: String,
    },

    /// The resource does not exist, or was removed by a rescan.
    #[error("not found: {detail}")]
    NotFound {
        /// The server's explanation.
        detail: String,
        /// The problem document's `type`: the stable identifier for *which*
        /// failure this is, where the status alone cannot say.
        ///
        /// `media-not-found` and `source-file-missing` are both 404s and want
        /// different words in front of a viewer -- the second means the
        /// library and the disk have diverged, which is an operator's problem
        /// rather than the viewer's. `about:blank` when the framework answered
        /// rather than the application, which RFC 9457 defines as "the status
        /// code is the whole story".
        code: String,
    },

    /// The server rejected the request as malformed.
    #[error("bad request: {detail}")]
    BadRequest {
        /// The server's explanation.
        detail: String,
        /// The problem document's `type`. See [`BeamError::NotFound`].
        code: String,
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
        /// Whether the same request could plausibly succeed later.
        ///
        /// Carried rather than re-derived, the same way [`BeamError::Network`]
        /// carries its own. A status is not something a client can reason
        /// about on its own here: 5xx means the server failed to handle a
        /// request it accepted, while a 4xx that reaches this variant -- a 415
        /// or a 422, which three operations declare -- is the request itself
        /// being refused, and resending it unchanged fails identically.
        /// Kotlin and Swift each used to decide that for themselves and both
        /// answered "retryable" unconditionally, so a schema-rejected body got
        /// a retry button forever. Deciding it once, here, is what makes that
        /// unrepresentable rather than merely fixed.
        retryable: bool,
        /// The server's explanation, where it sent one.
        detail: String,
        /// The problem document's `type`. See [`BeamError::NotFound`].
        code: String,
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
    /// Whether retrying the identical request *now* could plausibly succeed.
    ///
    /// Not exported across the FFI: Kotlin and Swift read the `retryable`
    /// field that [`BeamError::Server`] and [`BeamError::Network`] carry, which
    /// is where the verdict for a status is decided once. Deliberately
    /// conservative: an error whose retryability is unclear is treated as *not*
    /// retryable. Whether a failed progress sample is kept is a different
    /// question -- see [`BeamError::is_permanent`].
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network { retryable, .. } => *retryable,
            Self::RateLimited { .. } => true,
            // 5xx is the server failing to handle a request it accepted;
            // the same request may well succeed later. A 4xx that reached
            // here -- a 415 or a 422 on the three operations that declare
            // them -- is the request itself being refused, and resending it
            // unchanged fails identically.
            Self::Server { retryable, .. } => *retryable,
            // The device could not write, not the server could not be asked.
            // A full disk or a locked keystore clears, and the viewer who
            // frees space has a real path to success -- so this is stated
            // rather than left to the catch-all below, which had it backwards.
            // Android said retryable here and gave that reason; Rust said not
            // and gave none, because `Storage` simply fell through `_`.
            Self::Storage { .. } => true,
            _ => false,
        }
    }

    /// Whether the identical request will fail identically until the request
    /// itself changes -- so retrying it later, after anything at all has
    /// happened, is pointless.
    ///
    /// This is the question the playback-progress queue asks, and it is not
    /// the complement of [`is_retryable`](Self::is_retryable). Three failures
    /// are not retryable *now* and yet are the whole reason the queue exists:
    /// an expired session, a missing one, and a certificate the user has not
    /// yet trusted. Each clears the moment the user acts, and the sample is
    /// then accepted unchanged -- dropping it would lose exactly the resume
    /// point that an interrupted session is supposed to preserve. Every
    /// variant is named so a new one has to say which side it is on.
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        match self {
            // The request itself is refused: a body the schema rejects, a
            // file the user may not touch, a title that no longer exists, a
            // response that does not match the contract. Resending it changes
            // nothing.
            Self::BadRequest { .. }
            | Self::Forbidden { .. }
            | Self::NotFound { .. }
            | Self::Protocol { .. }
            | Self::InvalidServerUrl { .. } => true,
            // Carried on the error, decided once at the transport.
            Self::Server { retryable, .. } | Self::Network { retryable, .. } => !*retryable,
            // Cured by the user acting: signing in, or trusting the
            // certificate. The sample waits for them.
            Self::SessionExpired | Self::Unauthenticated | Self::UntrustedCertificate { .. } => {
                false
            }
            // Cured by time or by the device: the limiter's window passes, the
            // disk is emptied, the keystore unlocks.
            Self::RateLimited { .. } | Self::Storage { .. } => false,
            // Cured by selecting or adding a server.
            Self::NoActiveServer | Self::UnknownServer { .. } => false,
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

/// A failure from the foreign side's byte source.
///
/// Separate from [`StorageError`] because the two boundaries fail for
/// unrelated reasons and the UI response differs: a locked keystore is a
/// device problem the user can fix, while a byte source that stops mid-file is
/// a network or disk problem that interrupts playback.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum ByteSourceError {
    /// The bytes could not be fetched at all.
    #[error("byte source unavailable: {detail}")]
    Unavailable {
        /// The foreign side's explanation.
        detail: String,
    },

    /// The requested range lies beyond the end of the source.
    #[error("byte source range out of bounds: offset {offset}, length {length}")]
    OutOfBounds {
        /// Where the read started.
        offset: u64,
        /// How many bytes were asked for.
        length: u64,
    },
}

/// A failure while demuxing a container.
///
/// Every variant is a permanent property of the file or the request, not a
/// transient one -- under direct play ([ADR-0004]) there is no server-side
/// remux to fall back on, so a container the client cannot open is a fact the
/// viewer needs told, exactly as an undecodable codec is.
///
/// [ADR-0004]: ../../docs/architecture/decisions/ADR-0004-never-transcode.md
// No `Eq`: the `Seek` variant carries the requested position as an `f64`, which
// is the unit every caller already speaks. Deriving `Eq` would mean rounding the
// position to something integral purely to satisfy a trait no caller uses.
#[derive(Debug, Clone, PartialEq, thiserror::Error, uniffi::Error)]
pub enum ExtractorError {
    /// The bytes are not a container this extractor understands.
    #[error("not a readable Matroska container: {detail}")]
    Malformed {
        /// What the parser objected to.
        detail: String,
    },

    /// The container is well-formed but uses a feature the extractor does not
    /// implement.
    #[error("unsupported container feature: {detail}")]
    Unsupported {
        /// The feature that was refused.
        detail: String,
    },

    /// The underlying byte source failed.
    #[error("byte source failed: {detail}")]
    Source {
        /// The byte source's own explanation.
        detail: String,
    },

    /// A seek was requested to a position the container cannot reach.
    #[error("cannot seek to {seconds}s: {detail}")]
    Seek {
        /// The requested position.
        seconds: f64,
        /// Why it could not be reached.
        detail: String,
    },
}

impl From<ByteSourceError> for ExtractorError {
    fn from(error: ByteSourceError) -> Self {
        Self::Source {
            detail: error.to_string(),
        }
    }
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
                detail: String::new(),
                code: String::new(),
            }
            .is_retryable()
        );
    }

    /// The distinction the progress queue lives on: not retryable now is not
    /// the same as never worth retrying.
    #[test]
    fn a_credential_or_trust_failure_is_not_permanent_while_a_refusal_is() {
        for kept in [
            BeamError::SessionExpired,
            BeamError::Unauthenticated,
            BeamError::UntrustedCertificate {
                host: "beam.test".to_owned(),
                details: CertificateDetails {
                    sha256_fingerprint: "AA:BB".to_owned(),
                    spki_sha256_base64: "c3BraQ==".to_owned(),
                    subject: "CN=beam.local".to_owned(),
                    issuer: "CN=beam.local".to_owned(),
                    not_before_unix: 0,
                    not_after_unix: i64::MAX,
                    subject_alt_names: vec!["beam.local".to_owned()],
                    serial_hex: "01".to_owned(),
                    is_self_signed: true,
                    is_expired: false,
                },
            },
        ] {
            assert!(
                !kept.is_retryable() && !kept.is_permanent(),
                "{kept:?} waits for the user, so it is neither retryable now nor dropped"
            );
        }

        for refused in [
            BeamError::BadRequest {
                detail: String::new(),
                code: String::new(),
            },
            BeamError::Forbidden {
                detail: String::new(),
                code: String::new(),
            },
            BeamError::NotFound {
                detail: String::new(),
                code: String::new(),
            },
            BeamError::Protocol {
                detail: String::new(),
            },
            BeamError::Server {
                status: 422,
                retryable: false,
                detail: String::new(),
                code: String::new(),
            },
        ] {
            assert!(
                refused.is_permanent() && !refused.is_retryable(),
                "{refused:?} fails identically forever"
            );
        }

        for transient in [
            BeamError::RateLimited {
                retry_after_secs: 1,
            },
            BeamError::Server {
                status: 503,
                retryable: true,
                detail: String::new(),
                code: String::new(),
            },
            BeamError::Network {
                detail: String::new(),
                retryable: true,
            },
            BeamError::Storage {
                detail: String::new(),
            },
        ] {
            assert!(
                transient.is_retryable() && !transient.is_permanent(),
                "{transient:?} is worth retrying, so it cannot also be permanent"
            );
        }
    }

    #[test]
    fn storage_failures_widen_into_the_core_taxonomy() {
        let widened: BeamError = StorageError::Denied {
            detail: "keystore locked".to_owned(),
        }
        .into();
        assert!(matches!(widened, BeamError::Storage { .. }));
        // Retryable: the keystore unlocks, the disk is emptied. This used to
        // assert the opposite while `BeamErrorsTest.kt` asserted this, in the
        // same change -- neither could see the other, because the core's
        // verdict never crossed the FFI.
        assert!(widened.is_retryable());
    }
}
