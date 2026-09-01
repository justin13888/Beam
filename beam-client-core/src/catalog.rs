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

use crate::api::types as wire;
use crate::error::BeamError;
use crate::servers::ServerRecord;

/// Whether a catalog entry is a film or a series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaKind {
    /// A single film.
    Movie,
    /// A series with seasons and episodes.
    Show,
}

/// A catalog entry as a grid tile or list row needs it.
///
/// Deliberately flat: a tile renders from this alone, with no follow-up
/// request and no nested optionals to unwrap on the UI thread.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MediaSummary {
    /// Stable identifier, used for detail and source lookups.
    pub id: String,
    /// Whether this is a film or a series.
    pub kind: MediaKind,
    /// The title to display, preferring the localized form.
    pub title: String,
    /// The original-language title, for a subtitle when it differs.
    pub original_title: String,
    /// Release year, where metadata determined one.
    pub year: Option<u32>,
    /// Synopsis.
    pub description: Option<String>,
    /// Absolute poster URL.
    pub poster_url: Option<String>,
    /// Absolute backdrop URL.
    pub backdrop_url: Option<String>,
    /// Genre names.
    pub genres: Vec<String>,
    /// Runtime in whole minutes, for films.
    pub runtime_minutes: Option<u32>,
    /// TMDB rating as a percentage, where one is known.
    pub tmdb_rating: Option<u32>,
    /// The file to play, for a film that has exactly one.
    pub file_id: Option<String>,
    /// Number of seasons, for a series.
    pub season_count: u32,
    /// Number of episodes across every season, for a series.
    pub episode_count: u32,
}

/// One page of a cursor-paginated browse.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MediaPage {
    /// The entries on this page, in server order.
    pub items: Vec<MediaSummary>,
    /// Cursor to pass as `after` to fetch the next page.
    pub end_cursor: Option<String>,
    /// Whether another page exists.
    pub has_next_page: bool,
}

/// One episode of a series.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct EpisodeSummary {
    /// Stable identifier.
    pub id: String,
    /// Position within its season, as metadata numbered it.
    pub episode_number: u32,
    /// Episode title.
    pub title: String,
    /// Synopsis.
    pub description: Option<String>,
    /// Absolute thumbnail URL.
    pub thumbnail_url: Option<String>,
    /// Air date, as the server recorded it.
    pub air_date: Option<String>,
    /// Duration in seconds, where probing determined one.
    pub duration_secs: Option<f64>,
    /// The file to play, when one is indexed.
    ///
    /// `None` means the episode is known to metadata but has no file, which
    /// is what up-next skips over.
    pub file_id: Option<String>,
}

/// One season of a series.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SeasonSummary {
    /// Season number, as metadata numbered it.
    pub season_number: u32,
    /// Absolute poster URL for this season.
    pub poster_url: Option<String>,
    /// Typical episode runtime in whole minutes.
    pub episode_runtime_minutes: Option<u32>,
    /// Genre names recorded against this season.
    pub genres: Vec<String>,
    /// Episodes, in the order the server returned them.
    pub episodes: Vec<EpisodeSummary>,
}

/// Everything a detail screen shows.
///
/// A sealed alternative rather than one struct with empty collections: a film
/// has no seasons and a series has no single `file_id`, and encoding that in
/// the type stops a screen rendering a "Play" button for a series that has no
/// file behind it.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum MediaDetail {
    /// A film, playable directly.
    Movie {
        /// Tile-level fields.
        summary: MediaSummary,
    },
    /// A series, played through one of its episodes.
    Show {
        /// Tile-level fields.
        summary: MediaSummary,
        /// Seasons in server order.
        seasons: Vec<SeasonSummary>,
    },
}

impl MediaDetail {
    /// The tile-level fields, whichever kind this is.
    #[must_use]
    pub fn summary(&self) -> &MediaSummary {
        match self {
            Self::Movie { summary } | Self::Show { summary, .. } => summary,
        }
    }
}

/// Field to order a browse by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaSortField {
    /// Alphabetical by title.
    Title,
    /// By release year.
    Year,
    /// By rating.
    Rating,
    /// By when the file was indexed.
    DateAdded,
    /// By runtime.
    Runtime,
}

/// Ascending or descending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SortOrder {
    /// Smallest or earliest first.
    Ascending,
    /// Largest or latest first.
    Descending,
}

/// Restrict a browse to one kind of entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaTypeFilter {
    /// Films only.
    Movie,
    /// Series only.
    Show,
}

/// Everything the explore screen can ask for.
///
/// One record rather than a dozen arguments, so adding a filter later is not a
/// breaking change to every caller on three platforms.
#[derive(Debug, Clone, PartialEq, Eq, Default, uniffi::Record)]
pub struct BrowseQuery {
    /// Page size.
    pub first: Option<u32>,
    /// Cursor from a previous page's `end_cursor`.
    pub after: Option<String>,
    /// Field to order by.
    pub sort_by: Option<MediaSortField>,
    /// Direction to order in.
    pub sort_order: Option<SortOrder>,
    /// Restrict to films or series.
    pub media_type: Option<MediaTypeFilter>,
    /// Restrict to one genre.
    pub genre: Option<String>,
    /// Restrict to one exact year.
    pub year: Option<u32>,
    /// Earliest year, inclusive.
    pub year_from: Option<u32>,
    /// Latest year, inclusive.
    pub year_to: Option<u32>,
    /// Free-text search.
    pub query: Option<String>,
    /// Minimum rating percentage.
    pub min_rating: Option<u32>,
}

/// A library as the libraries screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LibrarySummary {
    /// Stable identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Operator-supplied description.
    pub description: Option<String>,
    /// Number of indexed files.
    pub size: u32,
    /// Files seen by the most recent scan.
    pub last_scan_file_count: Option<u32>,
    /// When the most recent scan started, as a Unix timestamp.
    pub last_scan_started_at_unix: Option<i64>,
    /// When the most recent scan finished, as a Unix timestamp.
    ///
    /// A started time with no finished time is a scan still running, which is
    /// what the libraries screen shows a progress indicator for.
    pub last_scan_finished_at_unix: Option<i64>,
}

/// What an indexed file was classified as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FileContentType {
    /// Matched to a film.
    Movie,
    /// Matched to an episode.
    Episode,
    /// Indexed but not matched to any title.
    Unclassified,
}

/// Whether the indexer's record of a file is current.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FileIndexStatus {
    /// Indexed and unchanged since.
    Known,
    /// Changed on disk since it was indexed.
    Changed,
    /// Seen but not yet probed.
    Unknown,
}

/// One indexed file, as the library detail screen lists it.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct LibraryFileSummary {
    /// Stable identifier.
    pub id: String,
    /// The library that owns it.
    pub library_id: String,
    /// Path on the server.
    pub path: String,
    /// Size on disk.
    pub size_bytes: u64,
    /// Container format as probed.
    pub container_format: Option<String>,
    /// MIME type as recorded.
    pub mime_type: Option<String>,
    /// Duration in seconds, where probing determined one.
    pub duration_secs: Option<f64>,
    /// What it was matched to.
    pub content_type: FileContentType,
    /// Whether the record is current.
    pub status: FileIndexStatus,
    /// When it was last scanned, as a Unix timestamp.
    pub scanned_at_unix: i64,
}

/// A partially-watched title, ready to resume.
///
/// `media` is populated by the core rather than the server: the API returns
/// only identifiers, so a tile would otherwise have nothing to draw. See
/// [`crate::ffi::BeamClient::continue_watching`] for how it is filled in.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ContinueWatchingEntry {
    /// The title this progress belongs to.
    pub media_id: String,
    /// The episode, when the title is a series.
    pub episode_id: Option<String>,
    /// The file to resume.
    pub file_id: String,
    /// Whether the title is a film or a series.
    pub kind: MediaKind,
    /// Where the user stopped.
    pub position_secs: f64,
    /// Total duration, where one is known.
    pub duration_secs: Option<f64>,
    /// How far through, from 0.0 to 1.0, or `None` without a duration.
    pub progress_fraction: Option<f64>,
    /// When progress was last reported, as a Unix timestamp.
    pub updated_at_unix: i64,
    /// The title's metadata, resolved by the core.
    ///
    /// `None` when the lookup failed, which the UI renders as a plain tile
    /// rather than dropping the entry -- the user's place is still valid.
    pub media: Option<MediaSummary>,
    /// The episode's own metadata, when this is a series and it resolved.
    pub episode: Option<EpisodeSummary>,
}

/// One watch-history row.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct HistoryEntry {
    /// The title this row belongs to.
    pub media_id: String,
    /// The episode, when the title is a series.
    pub episode_id: Option<String>,
    /// The file that was played.
    pub file_id: String,
    /// Whether the title is a film or a series.
    pub kind: MediaKind,
    /// Where the user stopped.
    pub position_secs: f64,
    /// Total duration, where one is known.
    pub duration_secs: Option<f64>,
    /// How far through, from 0.0 to 1.0, or `None` without a duration.
    pub progress_fraction: Option<f64>,
    /// Whether the title was watched to the end.
    pub completed: bool,
    /// When it was last played, as a Unix timestamp.
    pub updated_at_unix: i64,
    /// The title's metadata, resolved by the core.
    pub media: Option<MediaSummary>,
    /// The episode's own metadata, when this is a series and it resolved.
    pub episode: Option<EpisodeSummary>,
}

/// One page of watch history.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct HistoryPage {
    /// The rows on this page.
    pub items: Vec<HistoryEntry>,
    /// Total rows across every page, for a count in the UI.
    pub total: u64,
}

/// A signed-in device, as the profile screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DeviceSession {
    /// Stable identifier, used to revoke it.
    pub id: String,
    /// Opaque per-device hash the server assigned.
    pub device_hash: String,
    /// Address the session was last seen from.
    pub ip: String,
    /// When it was created, as a Unix timestamp.
    pub created_at_unix: i64,
    /// When it was last active, as a Unix timestamp.
    pub last_active_unix: i64,
}

/// Severity of an operational log line or event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LogLevel {
    /// Routine.
    Info,
    /// Notable but not a failure.
    Warning,
    /// A failure.
    Error,
}

/// What part of the server an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum EventCategory {
    /// Progress or completion of a library scan.
    LibraryScan,
    /// Anything else the server reports.
    System,
}

/// Aggregate counts on the admin dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AdminCounts {
    /// Libraries configured.
    pub libraries: u64,
    /// Files indexed.
    pub files: u64,
    /// Users known to the server.
    pub users: u64,
}

/// Progress of the metadata enrichment queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct EnrichmentCounts {
    /// Waiting to be enriched.
    pub pending: u64,
    /// Enriched successfully.
    pub enriched: u64,
    /// Enrichment failed.
    pub failed: u64,
    /// Probed but matched to no title.
    pub unmatched: u64,
}

/// A recent scan line on the admin dashboard.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RecentScan {
    /// Severity.
    pub level: LogLevel,
    /// What happened.
    pub message: String,
    /// When, as a Unix timestamp.
    pub timestamp_unix: i64,
}

/// The admin dashboard's snapshot of the server.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AdminStatus {
    /// Server version.
    pub version: String,
    /// How long the server has been up.
    pub uptime_secs: u64,
    /// Aggregate counts.
    pub counts: AdminCounts,
    /// Enrichment queue progress.
    pub enrichment: EnrichmentCounts,
    /// Most recent scan activity, newest first.
    pub recent_scans: Vec<RecentScan>,
}

/// A user account, as the admin user list shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AdminUser {
    /// Stable identifier.
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Email address, where the identity provider supplied one.
    pub email: Option<String>,
    /// Absolute avatar URL.
    pub avatar_url: Option<String>,
    /// Whether the account holds administrative rights.
    pub is_admin: bool,
    /// Whether the account is currently blocked from signing in.
    pub disabled: bool,
    /// When the account was created, as a Unix timestamp.
    pub created_at_unix: i64,
}

/// One page of the admin user list.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AdminUserPage {
    /// The accounts on this page.
    pub items: Vec<AdminUser>,
    /// Total accounts across every page.
    pub total: u64,
}

/// One operational log line.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AdminLogEntry {
    /// Stable identifier.
    pub id: String,
    /// Severity.
    pub level: LogLevel,
    /// Subsystem that emitted it.
    pub category: String,
    /// What happened.
    pub message: String,
    /// Structured detail, as the server serialised it.
    pub details: Option<String>,
    /// When, as the server recorded it.
    pub created_at: String,
}

/// One server event, as the admin activity feed shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AdminEvent {
    /// Stable identifier.
    pub id: String,
    /// Severity.
    pub level: LogLevel,
    /// What part of the server it came from.
    pub category: EventCategory,
    /// What happened.
    pub message: String,
    /// The library it concerns, for a scan event.
    pub library_id: Option<String>,
    /// That library's name, so the feed need not resolve it.
    pub library_name: Option<String>,
    /// When, as a Unix timestamp.
    pub timestamp_unix: i64,
}

/// The server's own health report.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ServerHealth {
    /// Overall status string.
    pub status: String,
    /// Server version.
    pub version: String,
    /// How long the server has been up.
    pub uptime_secs: u64,
    /// Database connectivity, as the server reports it.
    pub database: String,
}

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

/// Resolve an optional server-relative URL against the server that served it.
///
/// A URL that cannot be resolved is dropped rather than failing the whole
/// page: a missing poster is a cosmetic defect, and refusing to show the
/// library because one artwork path is malformed is not an improvement.
fn absolute(record: &ServerRecord, url: Option<String>) -> Option<String> {
    url.and_then(|raw| record.absolute_url(&raw).ok())
}

/// The fraction of a title watched, where a duration is known.
///
/// Clamped because a player that reports a position past the end -- which
/// happens when a file's probed duration is slightly short -- must not produce
/// a progress bar wider than its track.
#[must_use]
pub fn progress_fraction(position_secs: f64, duration_secs: Option<f64>) -> Option<f64> {
    match duration_secs {
        Some(duration) if duration > 0.0 => Some((position_secs / duration).clamp(0.0, 1.0)),
        _ => None,
    }
}

impl MediaSummary {
    /// Normalise a generated catalog entry.
    fn from_generated(node: wire::MediaMetadata, record: &ServerRecord) -> Self {
        match node {
            wire::MediaMetadata::MediaMetadataVariant1(boxed) => {
                Self::from_movie(&boxed.movie, record)
            }
            wire::MediaMetadata::MediaMetadataVariant0(boxed) => {
                Self::from_show(&boxed.show, record)
            }
        }
    }

    fn from_movie(movie: &wire::MovieMetadata, record: &ServerRecord) -> Self {
        let title = &movie.title;
        Self {
            id: movie.id.clone(),
            kind: MediaKind::Movie,
            title: title
                .localized
                .clone()
                .unwrap_or_else(|| title.original.clone()),
            original_title: title.original.clone(),
            year: narrow_u32(movie.year),
            description: movie.description.clone(),
            poster_url: absolute(record, movie.poster_url.clone()),
            backdrop_url: absolute(record, movie.backdrop_url.clone()),
            genres: movie.genres.clone(),
            runtime_minutes: narrow_u32(movie.runtime),
            tmdb_rating: narrow_u32(movie.ratings.as_ref().and_then(|ratings| ratings.tmdb)),
            file_id: movie.file_id.clone(),
            season_count: 0,
            episode_count: 0,
        }
    }

    fn from_show(show: &wire::ShowMetadata, record: &ServerRecord) -> Self {
        let title = &show.title;
        // A series carries its artwork and genres on its seasons rather than
        // on itself, so a tile takes the first season that has any. Without
        // this every series renders as a blank placeholder.
        let poster = show
            .seasons
            .iter()
            .find_map(|season| season.poster_url.clone());
        let genres = show
            .seasons
            .iter()
            .find(|season| !season.genres.is_empty())
            .map(|season| season.genres.clone())
            .unwrap_or_default();
        let rating = show
            .seasons
            .iter()
            .find_map(|season| season.ratings.as_ref().and_then(|ratings| ratings.tmdb));
        let runtime = show
            .seasons
            .iter()
            .find_map(|season| season.episode_runtime);

        Self {
            id: show.id.clone(),
            kind: MediaKind::Show,
            title: title
                .localized
                .clone()
                .unwrap_or_else(|| title.original.clone()),
            original_title: title.original.clone(),
            year: narrow_u32(show.year),
            description: show.description.clone(),
            poster_url: absolute(record, poster),
            backdrop_url: None,
            genres,
            runtime_minutes: narrow_u32(runtime),
            tmdb_rating: narrow_u32(rating),
            file_id: None,
            season_count: u32::try_from(show.seasons.len()).unwrap_or(u32::MAX),
            episode_count: show
                .seasons
                .iter()
                .map(|season| u32::try_from(season.episodes.len()).unwrap_or(u32::MAX))
                .sum(),
        }
    }
}

impl EpisodeSummary {
    fn from_generated(episode: &wire::EpisodeMetadata, record: &ServerRecord) -> Self {
        Self {
            id: episode.id.clone(),
            episode_number: narrow_u32(Some(episode.episode_number)).unwrap_or(0),
            title: episode.title.clone(),
            description: episode.description.clone(),
            thumbnail_url: absolute(record, episode.thumbnail_url.clone()),
            air_date: episode.air_date.clone(),
            duration_secs: episode.duration,
            file_id: episode.file_id.clone(),
        }
    }
}

impl SeasonSummary {
    fn from_generated(season: &wire::SeasonMetadata, record: &ServerRecord) -> Self {
        Self {
            season_number: narrow_u32(Some(season.season_number)).unwrap_or(0),
            poster_url: absolute(record, season.poster_url.clone()),
            episode_runtime_minutes: narrow_u32(season.episode_runtime),
            genres: season.genres.clone(),
            episodes: season
                .episodes
                .iter()
                .map(|episode| EpisodeSummary::from_generated(episode, record))
                .collect(),
        }
    }
}

impl MediaDetail {
    /// Normalise a generated detail response.
    #[must_use]
    pub fn from_generated(node: wire::MediaMetadata, record: &ServerRecord) -> Self {
        match node {
            wire::MediaMetadata::MediaMetadataVariant1(boxed) => Self::Movie {
                summary: MediaSummary::from_movie(&boxed.movie, record),
            },
            wire::MediaMetadata::MediaMetadataVariant0(boxed) => Self::Show {
                summary: MediaSummary::from_show(&boxed.show, record),
                seasons: boxed
                    .show
                    .seasons
                    .iter()
                    .map(|season| SeasonSummary::from_generated(season, record))
                    .collect(),
            },
        }
    }
}

impl MediaPage {
    /// Normalise a generated connection.
    #[must_use]
    pub fn from_generated(connection: wire::MediaConnection, record: &ServerRecord) -> Self {
        let wire::MediaConnection { edges, page_info } = connection;
        Self {
            items: edges
                .into_iter()
                .map(|edge| MediaSummary::from_generated(edge.node, record))
                .collect(),
            end_cursor: page_info.end_cursor,
            has_next_page: page_info.has_next_page,
        }
    }
}

impl LibrarySummary {
    /// Normalise a generated library.
    #[must_use]
    pub fn from_generated(library: wire::Library) -> Self {
        let wire::Library {
            description,
            id,
            last_scan_file_count,
            last_scan_finished_at,
            last_scan_started_at,
            name,
            size,
        } = library;
        Self {
            id,
            name,
            description,
            size: u32::try_from(size).unwrap_or(0),
            last_scan_file_count: last_scan_file_count.and_then(|count| u32::try_from(count).ok()),
            last_scan_started_at_unix: last_scan_started_at.map(|at| at.0.unix_timestamp()),
            last_scan_finished_at_unix: last_scan_finished_at.map(|at| at.0.unix_timestamp()),
        }
    }
}

impl LibraryFileSummary {
    /// Normalise a generated library file.
    #[must_use]
    pub fn from_generated(file: wire::LibraryFile) -> Self {
        let wire::LibraryFile {
            container_format,
            content_type,
            duration_secs,
            hash: _,
            id,
            library_id,
            mime_type,
            path,
            scanned_at,
            size_bytes,
            status,
            updated_at: _,
        } = file;
        Self {
            id,
            library_id,
            path,
            size_bytes: u64::try_from(size_bytes).unwrap_or(0),
            container_format,
            mime_type,
            duration_secs,
            content_type: match content_type {
                wire::FileContentType::Movie => FileContentType::Movie,
                wire::FileContentType::Episode => FileContentType::Episode,
                wire::FileContentType::Unclassified => FileContentType::Unclassified,
            },
            status: match status {
                wire::FileIndexStatus::Known => FileIndexStatus::Known,
                wire::FileIndexStatus::Changed => FileIndexStatus::Changed,
                wire::FileIndexStatus::Unknown => FileIndexStatus::Unknown,
            },
            scanned_at_unix: scanned_at.0.unix_timestamp(),
        }
    }
}

impl DeviceSession {
    /// Normalise a generated session summary.
    #[must_use]
    pub fn from_generated(session: wire::SessionSummary) -> Self {
        let wire::SessionSummary {
            created_at,
            device_hash,
            id,
            ip,
            last_active,
        } = session;
        Self {
            id,
            device_hash,
            ip,
            created_at_unix: created_at,
            last_active_unix: last_active,
        }
    }
}

/// Map the server's `media_type` string onto the core's own enum.
///
/// The playback endpoints type this as a bare string rather than an enum, so
/// anything unrecognised is treated as a film: a film is the shape with no
/// episode, which is exactly how an entry with no `episode_id` renders.
fn kind_from_str(raw: &str) -> MediaKind {
    if raw.eq_ignore_ascii_case("show") || raw.eq_ignore_ascii_case("episode") {
        MediaKind::Show
    } else {
        MediaKind::Movie
    }
}

impl ContinueWatchingEntry {
    /// Normalise a generated continue-watching row, before hydration.
    #[must_use]
    pub fn from_generated(item: wire::ContinueWatchingItem) -> Self {
        let wire::ContinueWatchingItem {
            duration_secs,
            episode_id,
            file_id,
            media_id,
            media_type,
            position_secs,
            updated_at,
        } = item;
        Self {
            media_id,
            episode_id,
            file_id,
            kind: kind_from_str(&media_type),
            position_secs,
            duration_secs,
            progress_fraction: progress_fraction(position_secs, duration_secs),
            updated_at_unix: updated_at.0.unix_timestamp(),
            media: None,
            episode: None,
        }
    }
}

impl HistoryEntry {
    /// Normalise a generated history row, before hydration.
    #[must_use]
    pub fn from_generated(item: wire::HistoryItem) -> Self {
        let wire::HistoryItem {
            completed,
            duration_secs,
            episode_id,
            file_id,
            media_id,
            media_type,
            position_secs,
            updated_at,
        } = item;
        Self {
            media_id,
            episode_id,
            file_id,
            kind: kind_from_str(&media_type),
            position_secs,
            duration_secs,
            progress_fraction: progress_fraction(position_secs, duration_secs),
            completed,
            updated_at_unix: updated_at.0.unix_timestamp(),
            media: None,
            episode: None,
        }
    }
}

impl LogLevel {
    fn from_log(level: wire::AdminLogLevelDto) -> Self {
        match level {
            wire::AdminLogLevelDto::Info => Self::Info,
            wire::AdminLogLevelDto::Warning => Self::Warning,
            wire::AdminLogLevelDto::Error => Self::Error,
        }
    }

    fn from_event(level: wire::AdminEventLevelDto) -> Self {
        match level {
            wire::AdminEventLevelDto::Info => Self::Info,
            wire::AdminEventLevelDto::Warning => Self::Warning,
            wire::AdminEventLevelDto::Error => Self::Error,
        }
    }
}

impl AdminStatus {
    /// Normalise a generated status response.
    #[must_use]
    pub fn from_generated(status: wire::AdminStatusResponse) -> Self {
        let wire::AdminStatusResponse {
            counts,
            enrichment,
            recent_scans,
            uptime_secs,
            version,
        } = status;
        Self {
            version,
            uptime_secs: u64::try_from(uptime_secs).unwrap_or(0),
            counts: AdminCounts {
                libraries: u64::try_from(counts.libraries).unwrap_or(0),
                files: u64::try_from(counts.files).unwrap_or(0),
                users: u64::try_from(counts.users).unwrap_or(0),
            },
            enrichment: EnrichmentCounts {
                pending: u64::try_from(enrichment.pending).unwrap_or(0),
                enriched: u64::try_from(enrichment.enriched).unwrap_or(0),
                failed: u64::try_from(enrichment.failed).unwrap_or(0),
                unmatched: u64::try_from(enrichment.unmatched).unwrap_or(0),
            },
            recent_scans: recent_scans
                .into_iter()
                .map(|scan| RecentScan {
                    level: LogLevel::from_log(scan.level),
                    message: scan.message,
                    timestamp_unix: scan.timestamp.0.unix_timestamp(),
                })
                .collect(),
        }
    }
}

impl AdminUser {
    /// Normalise a generated admin user row.
    #[must_use]
    pub fn from_generated(user: wire::AdminUserDto, record: &ServerRecord) -> Self {
        let wire::AdminUserDto {
            avatar_url,
            created_at,
            disabled,
            display_name,
            email,
            id,
            is_admin,
        } = user;
        Self {
            id,
            display_name,
            email,
            avatar_url: absolute(record, avatar_url),
            is_admin,
            disabled,
            created_at_unix: created_at.0.unix_timestamp(),
        }
    }
}

impl AdminLogEntry {
    /// Normalise a generated log line.
    #[must_use]
    pub fn from_generated(entry: wire::AdminLogEntryDto) -> Self {
        let wire::AdminLogEntryDto {
            category,
            created_at,
            details,
            id,
            level,
            message,
        } = entry;
        Self {
            id,
            level: LogLevel::from_log(level),
            category,
            message,
            // `details` is an untyped JSON value in the contract, so it is
            // carried across the boundary as text and rendered verbatim
            // rather than being given a shape the server never promised.
            details: details.map(|value| value.to_string()),
            created_at,
        }
    }
}

impl AdminEvent {
    /// Normalise a generated server event.
    #[must_use]
    pub fn from_generated(event: wire::AdminEventDto) -> Self {
        let wire::AdminEventDto {
            category,
            id,
            level,
            library_id,
            library_name,
            message,
            timestamp,
        } = event;
        Self {
            id,
            level: LogLevel::from_event(level),
            category: match category {
                wire::AdminEventCategoryDto::LibraryScan => EventCategory::LibraryScan,
                wire::AdminEventCategoryDto::System => EventCategory::System,
            },
            message,
            library_id,
            library_name,
            timestamp_unix: timestamp.0.unix_timestamp(),
        }
    }
}

impl ServerHealth {
    /// Normalise a generated health report.
    #[must_use]
    pub fn from_generated(health: wire::HealthStatus) -> Self {
        let wire::HealthStatus {
            checks,
            status,
            timestamp: _,
            uptime_secs,
            version,
        } = health;
        Self {
            status,
            version,
            uptime_secs: u64::try_from(uptime_secs).unwrap_or(0),
            database: checks.database,
        }
    }
}

/// Translate a browse query into the generated parameter type.
///
/// # Errors
///
/// Returns [`BeamError::BadRequest`] when the query cannot be expressed, which
/// today means only a page size the server would reject.
pub fn browse_params(query: &BrowseQuery) -> Result<crate::api::BrowseMediaParams, BeamError> {
    let first = match query.first {
        Some(0) => {
            return Err(BeamError::BadRequest {
                detail: "a page size of zero would return nothing".to_owned(),
            });
        }
        Some(value) => Some(i64::from(value)),
        None => None,
    };

    Ok(crate::api::BrowseMediaParams {
        first,
        after: query.after.clone(),
        last: None,
        before: None,
        // The query-parameter enums are generated separately from the schema
        // enums of the same shape, so these map onto `types::SortBy` and
        // friends rather than onto `MediaSortField`.
        sort_by: query.sort_by.map(|field| match field {
            MediaSortField::Title => wire::SortBy::Title,
            MediaSortField::Year => wire::SortBy::Year,
            MediaSortField::Rating => wire::SortBy::Rating,
            MediaSortField::DateAdded => wire::SortBy::DateAdded,
            MediaSortField::Runtime => wire::SortBy::Runtime,
        }),
        sort_order: query.sort_order.map(|order| match order {
            SortOrder::Ascending => wire::SortOrderFe86ad6c::Asc,
            SortOrder::Descending => wire::SortOrderFe86ad6c::Desc,
        }),
        media_type: query.media_type.map(|kind| match kind {
            MediaTypeFilter::Movie => wire::MediaType::Movie,
            MediaTypeFilter::Show => wire::MediaType::Show,
        }),
        genre: query.genre.clone(),
        year: query.year.map(i64::from),
        year_from: query.year_from.map(i64::from),
        year_to: query.year_to.map(i64::from),
        query: query.query.clone(),
        min_rating: query.min_rating.map(i64::from),
        origin: None,
        referer: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::normalize_base_url;

    /// A server to resolve relative artwork against.
    fn record() -> ServerRecord {
        let origin = normalize_base_url("https://beam.local:8000").expect("a valid origin");
        ServerRecord::new(&origin, Some("Test"), 0)
    }

    /// Parsing the wire shape rather than hand-building the generated structs
    /// means these tests also assert the contract the server actually
    /// publishes, not just this module's arithmetic.
    fn node(json: &str) -> wire::MediaMetadata {
        serde_json::from_str(json).expect("the fixture should match the generated type")
    }

    const MOVIE: &str = r#"{"Movie":{
        "id":"m1",
        "title":{"original":"Le Samourai","localized":"The Samurai"},
        "genres":["Crime","Drama"],
        "streams":[],
        "year":1967,
        "runtime":105,
        "duration":6300.0,
        "description":"A contract killer.",
        "poster_url":"/artwork/m1/poster.jpg",
        "backdrop_url":"/artwork/m1/backdrop.jpg",
        "file_id":"f1",
        "ratings":{"tmdb":81}
    }}"#;

    const SHOW: &str = r#"{"Show":{
        "id":"s1",
        "title":{"original":"Le Bureau"},
        "description":"Undercover.",
        "year":2015,
        "seasons":[
            {"id":"s1","season_number":1,"dates":{},"genres":[],"episodes":[
                {"id":"e1","episode_number":1,"title":"Pilot","streams":[],"file_id":"f1"},
                {"id":"e2","episode_number":2,"title":"Second","streams":[]}
            ]},
            {"id":"s2","season_number":2,"dates":{},"genres":["Thriller"],"episode_runtime":52,
             "poster_url":"/artwork/s1/2.jpg","ratings":{"tmdb":88},"episodes":[
                {"id":"e3","episode_number":1,"title":"Return","streams":[],"file_id":"f3",
                 "thumbnail_url":"/artwork/e3.jpg","duration":3120.0,"air_date":"2016-01-01"}
            ]}
        ]
    }}"#;

    #[test]
    fn movie_prefers_the_localized_title_and_keeps_the_original() {
        let summary = MediaSummary::from_generated(node(MOVIE), &record());
        assert_eq!(summary.title, "The Samurai");
        assert_eq!(summary.original_title, "Le Samourai");
        assert_eq!(summary.kind, MediaKind::Movie);
        assert_eq!(summary.year, Some(1967));
        assert_eq!(summary.runtime_minutes, Some(105));
        assert_eq!(summary.tmdb_rating, Some(81));
        assert_eq!(summary.file_id.as_deref(), Some("f1"));
    }

    #[test]
    fn a_title_without_a_localized_form_falls_back_to_the_original() {
        let summary = MediaSummary::from_generated(node(SHOW), &record());
        assert_eq!(summary.title, "Le Bureau");
        assert_eq!(summary.original_title, "Le Bureau");
    }

    #[test]
    fn relative_artwork_is_resolved_against_the_serving_server() {
        let summary = MediaSummary::from_generated(node(MOVIE), &record());
        assert_eq!(
            summary.poster_url.as_deref(),
            Some("https://beam.local:8000/artwork/m1/poster.jpg")
        );
        assert_eq!(
            summary.backdrop_url.as_deref(),
            Some("https://beam.local:8000/artwork/m1/backdrop.jpg")
        );
    }

    #[test]
    fn a_show_borrows_artwork_and_genres_from_the_first_season_that_has_them() {
        // The catalog records these per season, so a series that took them
        // only from itself would render as an untitled grey placeholder.
        let summary = MediaSummary::from_generated(node(SHOW), &record());
        assert_eq!(summary.kind, MediaKind::Show);
        assert_eq!(
            summary.poster_url.as_deref(),
            Some("https://beam.local:8000/artwork/s1/2.jpg")
        );
        assert_eq!(summary.genres, vec!["Thriller".to_owned()]);
        assert_eq!(summary.tmdb_rating, Some(88));
        assert_eq!(summary.runtime_minutes, Some(52));
    }

    #[test]
    fn a_show_counts_its_seasons_and_every_episode_across_them() {
        let summary = MediaSummary::from_generated(node(SHOW), &record());
        assert_eq!(summary.season_count, 2);
        assert_eq!(summary.episode_count, 3);
        assert_eq!(summary.file_id, None, "a series is not itself playable");
    }

    #[test]
    fn show_detail_carries_seasons_with_their_episodes() {
        let detail = MediaDetail::from_generated(node(SHOW), &record());
        let MediaDetail::Show { seasons, summary } = &detail else {
            panic!("a Show node must lower to a Show detail");
        };
        assert_eq!(summary.id, "s1");
        assert_eq!(seasons.len(), 2);
        assert_eq!(seasons[0].episodes.len(), 2);
        assert_eq!(seasons[1].episodes[0].id, "e3");
        assert_eq!(
            seasons[1].episodes[0].thumbnail_url.as_deref(),
            Some("https://beam.local:8000/artwork/e3.jpg")
        );
        assert_eq!(seasons[1].episodes[0].duration_secs, Some(3120.0));
        assert_eq!(
            seasons[1].episodes[0].air_date.as_deref(),
            Some("2016-01-01")
        );
    }

    #[test]
    fn an_episode_with_no_indexed_file_keeps_a_none_file_id() {
        // Up-next relies on this to skip past episodes that exist in metadata
        // but have nothing to play.
        let detail = MediaDetail::from_generated(node(SHOW), &record());
        let MediaDetail::Show { seasons, .. } = &detail else {
            panic!("a Show node must lower to a Show detail");
        };
        assert_eq!(seasons[0].episodes[0].file_id.as_deref(), Some("f1"));
        assert_eq!(seasons[0].episodes[1].file_id, None);
    }

    #[test]
    fn movie_detail_lowers_to_the_movie_branch() {
        let detail = MediaDetail::from_generated(node(MOVIE), &record());
        assert!(matches!(detail, MediaDetail::Movie { .. }));
        assert_eq!(detail.summary().id, "m1");
    }

    #[test]
    fn a_connection_becomes_a_page_that_carries_its_cursor() {
        let json = format!(
            r#"{{"edges":[{{"cursor":"c1","node":{MOVIE}}}],
                 "page_info":{{"has_next_page":true,"has_previous_page":false,"end_cursor":"c1"}}}}"#
        );
        let connection: wire::MediaConnection =
            serde_json::from_str(&json).expect("a valid connection");
        let page = MediaPage::from_generated(connection, &record());
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.end_cursor.as_deref(), Some("c1"));
        assert!(page.has_next_page);
    }

    #[test]
    fn progress_is_a_fraction_only_when_a_duration_is_known() {
        assert_eq!(progress_fraction(30.0, Some(120.0)), Some(0.25));
        assert_eq!(progress_fraction(30.0, None), None);
        assert_eq!(
            progress_fraction(30.0, Some(0.0)),
            None,
            "a zero duration must not divide"
        );
    }

    #[test]
    fn a_position_past_the_end_clamps_rather_than_overflowing_the_bar() {
        // Probed durations are sometimes slightly short of the real file, so
        // the player genuinely does report positions past the end.
        assert_eq!(progress_fraction(130.0, Some(120.0)), Some(1.0));
        assert_eq!(progress_fraction(-5.0, Some(120.0)), Some(0.0));
    }

    #[test]
    fn continue_watching_classifies_by_media_type_and_defaults_to_a_film() {
        let make = |media_type: &str| {
            let json = format!(
                r#"{{"file_id":"f1","media_id":"m1","media_type":"{media_type}",
                     "position_secs":30.0,"duration_secs":120.0,
                     "updated_at":"2026-01-01T00:00:00Z"}}"#
            );
            let item: wire::ContinueWatchingItem =
                serde_json::from_str(&json).expect("a valid item");
            ContinueWatchingEntry::from_generated(item)
        };
        assert_eq!(make("show").kind, MediaKind::Show);
        assert_eq!(make("episode").kind, MediaKind::Show);
        assert_eq!(make("movie").kind, MediaKind::Movie);
        // The field is an untyped string in the contract, so anything else is
        // treated as the shape with no episode rather than failing the row.
        assert_eq!(make("something-new").kind, MediaKind::Movie);
    }

    #[test]
    fn a_continue_watching_row_starts_unhydrated_and_carries_its_progress() {
        let json = r#"{"file_id":"f1","media_id":"m1","media_type":"movie",
                       "position_secs":30.0,"duration_secs":120.0,
                       "updated_at":"2026-01-01T00:00:00Z"}"#;
        let item: wire::ContinueWatchingItem = serde_json::from_str(json).expect("a valid item");
        let entry = ContinueWatchingEntry::from_generated(item);
        assert_eq!(entry.progress_fraction, Some(0.25));
        assert_eq!(entry.updated_at_unix, 1_767_225_600);
        assert!(entry.media.is_none(), "hydration is the client's job");
        assert!(entry.episode.is_none());
    }

    #[test]
    fn a_history_row_carries_its_completion_flag() {
        let json = r#"{"file_id":"f1","media_id":"m1","media_type":"show",
                       "episode_id":"e1","completed":true,
                       "position_secs":120.0,"duration_secs":120.0,
                       "updated_at":"2026-01-01T00:00:00Z"}"#;
        let item: wire::HistoryItem = serde_json::from_str(json).expect("a valid item");
        let entry = HistoryEntry::from_generated(item);
        assert!(entry.completed);
        assert_eq!(entry.episode_id.as_deref(), Some("e1"));
        assert_eq!(entry.progress_fraction, Some(1.0));
    }

    #[test]
    fn a_library_reports_a_running_scan_as_started_but_unfinished() {
        let json = r#"{"id":"l1","name":"Films","size":42,
                       "last_scan_started_at":"2026-01-01T00:00:00Z"}"#;
        let library: wire::Library = serde_json::from_str(json).expect("a valid library");
        let summary = LibrarySummary::from_generated(library);
        assert_eq!(summary.size, 42);
        assert_eq!(summary.last_scan_started_at_unix, Some(1_767_225_600));
        assert_eq!(summary.last_scan_finished_at_unix, None);
    }

    #[test]
    fn an_admin_log_entry_carries_untyped_details_as_text() {
        // `details` has no schema in the contract, so it is rendered verbatim
        // rather than given a shape the server never promised.
        let json = r#"{"id":"1","level":"warning","category":"scan",
                       "message":"skipped","created_at":"2026-01-01",
                       "details":{"path":"/media/x.mkv"}}"#;
        let entry: wire::AdminLogEntryDto = serde_json::from_str(json).expect("a valid log entry");
        let mapped = AdminLogEntry::from_generated(entry);
        assert_eq!(mapped.level, LogLevel::Warning);
        assert_eq!(
            mapped.details.as_deref(),
            Some(r#"{"path":"/media/x.mkv"}"#)
        );
    }

    #[test]
    fn browse_params_refuses_a_page_size_of_zero() {
        let query = BrowseQuery {
            first: Some(0),
            ..BrowseQuery::default()
        };
        assert!(matches!(
            browse_params(&query),
            Err(BeamError::BadRequest { .. })
        ));
    }

    #[test]
    fn browse_params_maps_every_filter_onto_the_query_enums() {
        let query = BrowseQuery {
            first: Some(24),
            after: Some("cursor".to_owned()),
            sort_by: Some(MediaSortField::DateAdded),
            sort_order: Some(SortOrder::Descending),
            media_type: Some(MediaTypeFilter::Show),
            genre: Some("Drama".to_owned()),
            year: Some(1999),
            year_from: Some(1990),
            year_to: Some(2000),
            query: Some("bureau".to_owned()),
            min_rating: Some(70),
        };
        let params = browse_params(&query).expect("a valid query");
        assert_eq!(params.first, Some(24));
        assert_eq!(params.after.as_deref(), Some("cursor"));
        assert!(matches!(params.sort_by, Some(wire::SortBy::DateAdded)));
        assert!(matches!(
            params.sort_order,
            Some(wire::SortOrderFe86ad6c::Desc)
        ));
        assert!(matches!(params.media_type, Some(wire::MediaType::Show)));
        assert_eq!(params.genre.as_deref(), Some("Drama"));
        assert_eq!(params.year, Some(1999));
        assert_eq!(params.year_from, Some(1990));
        assert_eq!(params.year_to, Some(2000));
        assert_eq!(params.min_rating, Some(70));
        assert_eq!(params.last, None, "forward paging only");
        assert_eq!(params.before, None);
    }

    #[test]
    fn an_empty_query_sends_no_filters_at_all() {
        let params = browse_params(&BrowseQuery::default()).expect("a valid query");
        assert_eq!(params.first, None);
        assert!(params.sort_by.is_none());
        assert!(params.genre.is_none());
    }

    #[test]
    fn a_negative_metadata_integer_is_treated_as_absent() {
        // JSON Schema has no unsigned type, so these arrive as i64. A negative
        // year is impossible from the server; the point is that it must not
        // wrap into a plausible-looking enormous number.
        assert_eq!(narrow_u32(Some(-1)), None);
        assert_eq!(narrow_u32(Some(1967)), Some(1967));
        assert_eq!(narrow_u32(None), None);
    }

    #[test]
    fn unresolvable_artwork_is_dropped_rather_than_failing_the_page() {
        let record = record();
        assert_eq!(absolute(&record, None), None);
        assert_eq!(
            absolute(&record, Some("/ok.jpg".to_owned())).as_deref(),
            Some("https://beam.local:8000/ok.jpg")
        );
    }

    #[test]
    fn a_health_report_narrows_its_uptime() {
        let json = r#"{"status":"ok","version":"0.1.0","uptime_secs":1234,
                       "timestamp":"2026-01-01","checks":{"database":"ok"}}"#;
        let health: wire::HealthStatus = serde_json::from_str(json).expect("a valid health report");
        let mapped = ServerHealth::from_generated(health);
        assert_eq!(mapped.uptime_secs, 1234);
        assert_eq!(mapped.database, "ok");
    }
}
