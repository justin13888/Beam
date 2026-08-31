//! Shared client core for Beam's native clients.
//!
//! Beam is direct-play only ([ADR-0004]), so the logic that decides *which*
//! file a device can actually decode is the substance of a native client --
//! and it is identical on every platform. This crate owns that logic once, in
//! Rust, and exposes it to Android (and later Apple and GTK) over UniFFI,
//! rather than being reimplemented per platform.
//!
//! The REST client is never hand-written. `api/openapi.json` is exported from
//! `beam-server`'s own handler annotations and lowered to Rust by spargen at
//! build time (see `build.rs`).
//!
//! This crate deliberately depends on none of `beam-domain`, `beam-index`, or
//! `beam-server`. `beam-domain` is not reusable here despite the name overlap:
//! it takes a non-optional `sea-orm` dependency and every repository trait
//! spells its error type as `sea_orm::DbErr`, so linking it would drag a
//! Postgres wire-protocol driver into an Android `.so`. `beam-index` links
//! FFmpeg, which does not cross-compile to Android. The small amount of shared
//! vocabulary is reproduced from the generated types instead.
//!
//! [ADR-0004]: ../../docs/architecture/decisions/ADR-0004-never-transcode.md

// `BeamError::UntrustedCertificate` carries the whole certificate, which makes
// the error type large enough to trip `result_large_err`. Boxing is the usual
// remedy and is not available here: UniFFI cannot express a `Box` inside an
// exported error, and it would buy nothing anyway, since the value is copied
// across the FFI boundary either way. Carrying the details inline is also what
// lets the trust prompt render from the error itself, rather than making a
// second call and racing the connection that produced it.
#![allow(clippy::result_large_err)]

/// The REST client, generated from `api/openapi.json` by spargen at build time.
///
/// Names here are derived from the spec's `operationId`s, which salvo emits as
/// fully-qualified Rust paths -- hence `beam_server_routes_media_browse_media`
/// rather than `browse_media`. Callers outside this crate should use the
/// hand-written wrappers rather than these names directly.
pub mod api {
    #![allow(clippy::all, missing_docs, unused)]
    include!(concat!(env!("OUT_DIR"), "/beam_api.rs"));
}

pub mod capability;
pub mod catalog;
pub mod clock;
pub mod error;
pub mod ffi;
pub mod paging;
pub mod ports;
pub mod progress;
pub mod servers;
pub mod session;
pub mod tls;
pub mod transport;
pub mod trust;
pub mod upnext;

pub use capability::{
    AudioCodec, DecoderCapability, DeviceProfile, MediaSourceView, Playability, QualityPolicy,
    RejectedSource, RejectionReason, SourceSelection, VideoCodec, select_source,
};
pub use catalog::{
    AdminEvent, AdminLogEntry, AdminStatus, AdminUser, AdminUserPage, BrowseQuery,
    ContinueWatchingEntry, DeviceSession, EpisodeSummary, HistoryEntry, HistoryPage,
    LibraryFileSummary, LibrarySummary, MediaDetail, MediaKind, MediaPage, MediaSortField,
    MediaSummary, MediaTypeFilter, SeasonSummary, ServerHealth, SortOrder,
};
pub use clock::{Clock, SystemClock};
pub use error::{BeamError, StorageError};
pub use paging::{CursorPager, PageAdvance};
pub use ports::kv::KeyValueStore;
pub use progress::{ProgressOutcome, ProgressQueue, ProgressThrottle, QueuedProgress};
pub use servers::{ServerRecord, normalize_base_url, server_id_for};
pub use session::{SessionEvent, SessionState, UserSummary};
pub use tls::{TrustDecision, install_crypto_provider};
pub use trust::CertificateDetails;
pub use upnext::{UpNextEpisode, UpNextSeason, next_playable_episode};

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests {
    use super::api::types::*;

    /// The catalog's `MediaMetadata` is a Serde externally-tagged enum, which
    /// reaches the spec as a `oneOf` of two single-required-property objects
    /// with no `discriminator`. That shape is only safe to generate if the
    /// branches are statically disjoint; this asserts the generated
    /// `Deserialize` actually discriminates rather than first-match-wins.
    #[test]
    fn media_metadata_discriminates_movie_from_show() {
        let movie = serde_json::json!({
            "Movie": {
                "id": "11111111-1111-4111-8111-111111111111",
                "title": { "original": "Arrival" },
                "genres": [],
                "ratings": {},
                "identifiers": {},
                "streams": [],
            }
        });
        let show = serde_json::json!({
            "Show": {
                "id": "22222222-2222-4222-8222-222222222222",
                "title": { "original": "Severance" },
                "seasons": [],
                "genres": [],
            }
        });

        let decoded_movie: BeamServerModelsMediaMediaMetadata =
            serde_json::from_value(movie).expect("movie decodes");
        assert!(matches!(
            decoded_movie,
            BeamServerModelsMediaMediaMetadata::BeamServerModelsMediaMediaMetadataVariant1(_)
        ));

        let decoded_show: BeamServerModelsMediaMediaMetadata =
            serde_json::from_value(show).expect("show decodes");
        assert!(matches!(
            decoded_show,
            BeamServerModelsMediaMediaMetadata::BeamServerModelsMediaMediaMetadataVariant0(_)
        ));
    }

    /// A payload matching neither branch must be rejected, not silently
    /// coerced into one of them.
    #[test]
    fn media_metadata_rejects_an_unknown_shape() {
        let neither = serde_json::json!({ "Episode": { "id": "x" } });
        let result: Result<BeamServerModelsMediaMediaMetadata, _> = serde_json::from_value(neither);
        assert!(result.is_err(), "an unknown variant must not decode");
    }
}
