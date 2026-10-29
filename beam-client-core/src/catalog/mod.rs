//! The vocabulary the UI browses, normalised from the generated client.
//!
//! The generated types are faithful to the OpenAPI document, which makes them
//! correct but awkward to render: every scalar field carries its own alias
//! (`Titleoriginal` rather than `String`), optional numbers arrive as `i64`
//! because JSON Schema has no unsigned type, and image URLs are
//! server-relative. Translating once here means no screen has to know
//! any of that, and it means the same normalisation is shared by every native
//! client rather than reimplemented per platform.
//!
//! Nothing in this module talks to the network. It is a pure mapping layer, so
//! it is exhaustively testable against hand-built generated values.

mod admin;
mod library;
mod media;
mod watching;

pub use admin::{
    AdminCounts, AdminEvent, AdminLogEntry, AdminLogPage, AdminStatus, AdminUser, AdminUserPage,
    DeviceSession, EnrichmentCounts, EventCategory, LogLevel, RecentScan, ServerHealth,
};
pub use library::{
    FileContentType, FileIndexStatus, LibraryFileSummary, LibrarySummary, ScanJob, ScanJobState,
};
pub use media::{
    BrowseQuery, EpisodeSummary, MediaDetail, MediaKind, MediaPage, MediaSortField, MediaSummary,
    MediaTypeFilter, SeasonSummary, SortOrder, browse_params,
};
pub use watching::{ContinueWatchingEntry, HistoryEntry, HistoryPage, progress_fraction};

use crate::servers::ServerRecord;

// -- conversions ----------------------------------------------------------
//
// Every numeric narrowing below is deliberate. The server's own types are
// unsigned, but JSON Schema has no unsigned integer, so kynos's `uint32` and
// `uint64` formats reach the generated client as `i64`. A negative value is
// therefore impossible in practice and meaningless if it occurred; each
// conversion treats one as absent rather than wrapping it into an enormous
// positive, which would render as a 4-billion-minute runtime.

/// Narrow a signed metadata integer, discarding a nonsensical negative.
fn narrow_u32(value: Option<i64>) -> Option<u32> {
    value.and_then(|raw| u32::try_from(raw).ok())
}

/// The server's 0-10 TMDB rating as the percentage the UniFFI record has
/// always carried, rounded to the nearest point: 7.25 is 73.
///
/// A rating off the 0-10 scale, which the server's schema bounds, is dropped
/// rather than clamped into one that looks real.
fn rating_percent(tmdb: Option<f64>) -> Option<u32> {
    tmdb.filter(|rating| (0.0..=10.0).contains(rating))
        // In 0..=100 after the check above, so the cast neither wraps nor
        // truncates anything but the fraction `round` already removed.
        .map(|rating| (rating * 10.0).round() as u32)
}

/// Resolve an optional server-relative URL against the server that served it.
///
/// A URL that cannot be resolved -- malformed, or off the server's origin,
/// which [`ServerRecord::absolute_url`] refuses -- is dropped rather than
/// failing the whole page: a missing poster is a cosmetic defect, and refusing
/// to show the library because one artwork path is unusable is not an
/// improvement.
fn absolute(record: &ServerRecord, url: Option<String>) -> Option<String> {
    url.and_then(|raw| record.absolute_url(&raw).ok())
}

/// Fixtures the catalog's tests share.
#[cfg(test)]
mod test_support {
    use crate::servers::{ServerRecord, normalize_base_url};

    /// A server to resolve relative artwork against.
    pub(super) fn record() -> ServerRecord {
        let origin = normalize_base_url("https://beam.local:8000").expect("a valid origin");
        ServerRecord::new(&origin, Some("Test"), 0)
    }
}
