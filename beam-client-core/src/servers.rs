//! Which Beam servers this device knows about.
//!
//! Multi-server is the primitive rather than a later addition, and a single
//! server is the degenerate case. Households run one; someone evaluating Beam
//! against their existing setup runs two; and retrofitting the distinction
//! later would mean revisiting every storage key and every cached response.
//!
//! Each server owns its own session cookie, trust pins, and progress queue.
//! Leaking any of those across servers would be a security bug, so removal
//! clears all of them together.

use crate::error::BeamError;
use serde::{Deserialize, Serialize};
use url::Url;

/// A stable identifier for a server, derived from its origin.
///
/// Deterministic rather than random, so re-adding the same server is
/// idempotent and the identity survives a reinstall. Readable rather than
/// hashed, because it appears in storage keys and log lines, and an operator
/// debugging a stale pin benefits from seeing which host it belongs to.
#[must_use]
pub fn server_id_for(origin: &Url) -> String {
    let host = origin.host_str().unwrap_or_default();
    let scheme = origin.scheme();
    match origin.port() {
        Some(port) => format!("{scheme}-{host}-{port}"),
        None => format!("{scheme}-{host}"),
    }
    .replace(['.', ':', '/'], "-")
}

/// Normalise a user-typed address into an origin.
///
/// People type `beam.local`, `beam.local:8000`, or a full URL with a path and
/// a trailing slash. All of those mean the same server, and treating them as
/// different ones would silently duplicate registry entries and split a user's
/// sessions across them.
///
/// # Errors
///
/// Returns [`BeamError::InvalidServerUrl`] when the input cannot be read as an
/// HTTP or HTTPS origin.
pub fn normalize_base_url(input: &str) -> Result<Url, BeamError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(BeamError::InvalidServerUrl {
            detail: "Enter your server's address".to_owned(),
        });
    }

    // Default to HTTPS rather than HTTP: a self-hoster who meant plaintext can
    // say so, but silently downgrading someone who typed a bare hostname would
    // put their session cookie on the wire in clear.
    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };

    let mut url = Url::parse(&candidate).map_err(|error| BeamError::InvalidServerUrl {
        detail: format!("That does not look like an address ({error})"),
    })?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(BeamError::InvalidServerUrl {
            detail: format!("{} addresses are not supported", url.scheme()),
        });
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(BeamError::InvalidServerUrl {
            detail: "That address has no host".to_owned(),
        });
    }

    // The origin is what identifies a server; a path, query or fragment the
    // user pasted from their browser is not part of it.
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// A server the user has added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRecord {
    /// Stable identifier, from [`server_id_for`].
    pub id: String,
    /// The name shown in the UI. Defaults to the host.
    pub display_name: String,
    /// The normalised origin.
    pub base_url: String,
    /// When the server was added, as a Unix timestamp.
    pub added_at_unix: i64,
    /// Certificate fingerprints the user has explicitly trusted for this
    /// server, as whole-certificate SHA-256 digests.
    pub trusted_fingerprints: Vec<String>,
}

impl ServerRecord {
    /// Build a record for a normalised origin.
    #[must_use]
    pub fn new(base_url: &Url, display_name: Option<&str>, added_at_unix: i64) -> Self {
        let host = base_url.host_str().unwrap_or("server").to_owned();
        Self {
            id: server_id_for(base_url),
            display_name: display_name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map_or(host, str::to_owned),
            base_url: base_url.as_str().trim_end_matches('/').to_owned(),
            added_at_unix,
            trusted_fingerprints: Vec::new(),
        }
    }

    /// Resolve a server-relative path against this server's origin.
    ///
    /// `MediaSource.stream_url` and `download_url` arrive relative
    /// (`/v1/files/{id}/stream`), and Media3 needs an absolute URL. Doing this
    /// in one place stops each call site inventing its own concatenation.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::InvalidServerUrl`] if the stored origin or the
    /// path cannot be joined.
    pub fn absolute_url(&self, relative: &str) -> Result<String, BeamError> {
        let base = Url::parse(&format!("{}/", self.base_url)).map_err(|error| {
            BeamError::InvalidServerUrl {
                detail: format!("stored server URL is unusable: {error}"),
            }
        })?;
        base.join(relative.trim_start_matches('/'))
            .map(|url| url.to_string())
            .map_err(|error| BeamError::InvalidServerUrl {
                detail: format!("could not resolve {relative}: {error}"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: &str) -> Url {
        normalize_base_url(input).expect("a valid url")
    }

    #[test]
    fn a_bare_host_defaults_to_https() {
        // Silently choosing http would put the session cookie on the wire in
        // clear for someone who simply typed their hostname.
        assert_eq!(url("beam.local").scheme(), "https");
    }

    #[test]
    fn an_explicit_scheme_is_respected() {
        assert_eq!(url("http://beam.local:8000").scheme(), "http");
    }

    #[test]
    fn every_spelling_of_one_server_normalises_to_one_origin() {
        // Otherwise the registry silently grows duplicates and splits the
        // user's sessions between them.
        let expected = url("https://beam.local:8000");
        for spelling in [
            "beam.local:8000",
            "https://beam.local:8000",
            "https://beam.local:8000/",
            "https://beam.local:8000/libraries",
            "  https://beam.local:8000/media?q=x#top  ",
        ] {
            assert_eq!(url(spelling), expected, "{spelling} should normalise");
        }
    }

    #[test]
    fn the_identifier_is_stable_and_readable() {
        assert_eq!(
            server_id_for(&url("https://beam.local:8000")),
            "https-beam-local-8000"
        );
        assert_eq!(
            server_id_for(&url("https://beam.local")),
            "https-beam-local"
        );
    }

    #[test]
    fn re_adding_the_same_server_produces_the_same_identifier() {
        assert_eq!(
            server_id_for(&url("beam.local:8000")),
            server_id_for(&url("https://beam.local:8000/some/path"))
        );
    }

    #[test]
    fn a_different_port_is_a_different_server() {
        assert_ne!(
            server_id_for(&url("https://beam.local:8000")),
            server_id_for(&url("https://beam.local:9000"))
        );
    }

    #[test]
    fn an_empty_or_schemeless_nonsense_address_is_rejected() {
        assert!(normalize_base_url("").is_err());
        assert!(normalize_base_url("   ").is_err());
        assert!(normalize_base_url("ftp://beam.local").is_err());
        assert!(normalize_base_url("https://").is_err());
    }

    #[test]
    fn the_display_name_falls_back_to_the_host() {
        let record = ServerRecord::new(&url("https://beam.local:8000"), None, 0);
        assert_eq!(record.display_name, "beam.local");

        let blank = ServerRecord::new(&url("https://beam.local:8000"), Some("   "), 0);
        assert_eq!(blank.display_name, "beam.local");

        let named = ServerRecord::new(&url("https://beam.local:8000"), Some("Home"), 0);
        assert_eq!(named.display_name, "Home");
    }

    #[test]
    fn a_relative_stream_url_resolves_against_the_origin() {
        // This is the one the player depends on: MediaSource URLs arrive
        // relative and Media3 needs them absolute.
        let record = ServerRecord::new(&url("https://beam.local:8000"), None, 0);
        assert_eq!(
            record.absolute_url("/v1/files/abc/stream").expect("joins"),
            "https://beam.local:8000/v1/files/abc/stream"
        );
        // Leading slash or not, the result is the same.
        assert_eq!(
            record.absolute_url("v1/files/abc/stream").expect("joins"),
            "https://beam.local:8000/v1/files/abc/stream"
        );
    }

    #[test]
    fn a_record_round_trips_through_json() {
        // The registry is persisted through the key/value port, so this is
        // the actual storage path.
        let record =
            ServerRecord::new(&url("https://beam.local:8000"), Some("Home"), 1_700_000_000);
        let encoded = serde_json::to_string(&record).expect("encode");
        let decoded: ServerRecord = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(record, decoded);
    }
}
