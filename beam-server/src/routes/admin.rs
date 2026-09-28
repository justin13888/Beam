//! `/v1/libraries` (any authenticated user -- browsing libraries is a normal
//! user action) and `/v1/admin/*` (admin-only: mutating a library, or seeing
//! operational logs/events). Tightens gap G8 from the pre-REST GraphQL
//! surface, where library create/scan/delete were only `AuthGuard`-gated
//! (any signed-in user, not just admins).
//!
//! The admin gate is `AdminAuth` -- `Scoped<SessionCookie, Admin>` -- taken in
//! the handler signature. Under Salvo it was a `require_admin(req, state)` call
//! in the body, which no describer could see, so the emitted document said
//! nothing about who may call these.

use std::time::Duration;

use async_stream::stream;
use kynos::prelude::*;
use kynos::response::headers::WithHeaders;
use kynos::response::status::{Accepted, NoContent};
use kynos::response::stream::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;

use crate::models::{
    AdminEventDto, AdminLogCountResponse, AdminLogEntryDto, AdminStatusCounts, AdminStatusResponse,
    AdminUserDto, AdminUserListResponse, CreateLibraryRequest, EnrichmentQueueCounts, Library,
    LibraryFile, LibraryTelemetryPreview, PlaybackTelemetryReport, RecentScanDto, ScanJob,
    UpdateAdminUserRequest, WatcherStatus,
};
use crate::routes::api_error::{
    AdminAuth, AdminUserError, InternalError, LibraryCreateError, LibraryRefError,
    LibraryScanError, LibraryScanReadError, MediaRefreshError, PlaybackTelemetryReportError,
    SessionAuth,
};
use crate::routes::tags::Admin;
use crate::services::library::LibraryError;
use crate::services::metadata::{MediaFilter, MetadataError};
use crate::services::playback_telemetry::ReportError;
use crate::services::telemetry::LibraryReportPreview;
use crate::state::AppState;

// The service errors are one vocabulary per service; the route errors are one
// type per operation shape. These conversions are where the two meet, and each
// is written for the operations that use it rather than shared across all of
// them -- that is what lets `deleteLibrary`'s 404 mean "library not found"
// instead of inheriting whichever 404 was declared first.
//
// An arm marked unreachable maps to 500 rather than to a plausible-looking 4xx.
// A `PathOutsideRoot` arriving from an operation that resolves an id would be a
// bug in this crate, and blaming the caller for it is how a 500 gets reported
// as a 400 and stops being investigated.

impl From<MetadataError> for MediaRefreshError {
    fn from(err: MetadataError) -> Self {
        match err {
            MetadataError::InvalidId => Self::InvalidMediaId(err.to_string()),
            MetadataError::MediaNotFound => Self::MediaNotFound(err.to_string()),
            // `refresh_metadata` applies to any media id; there is no
            // show-level restriction for it to report.
            // Nor does it page or search, so a cursor, page size or search
            // text is never its to refuse.
            MetadataError::Unsupported(msg)
            | MetadataError::InternalError(msg)
            | MetadataError::InvalidCursor(msg)
            | MetadataError::InvalidPagination(msg)
            | MetadataError::InvalidSearchQuery(msg) => Self::Internal(msg),
        }
    }
}

impl From<LibraryError> for LibraryRefError {
    fn from(err: LibraryError) -> Self {
        match err {
            LibraryError::InvalidId => Self::InvalidLibraryId(err.to_string()),
            LibraryError::LibraryNotFound => Self::LibraryNotFound(err.to_string()),
            // Unreachable: a path is only validated when a library is
            // registered or rescanned, never when one is resolved by id.
            LibraryError::PathNotFound(_)
            | LibraryError::PathOutsideRoot(_)
            | LibraryError::PathOverlapsLibrary
            | LibraryError::PathOverlapsDataDir
            | LibraryError::ScanInProgress
            | LibraryError::Db(_) => Self::Internal(err.to_string()),
        }
    }
}

impl From<LibraryError> for LibraryCreateError {
    fn from(err: LibraryError) -> Self {
        match err {
            LibraryError::PathNotFound(_) => Self::PathNotFound(err.to_string()),
            LibraryError::PathOutsideRoot(_) => Self::PathOutsideRoot(err.to_string()),
            LibraryError::PathOverlapsLibrary => Self::PathOverlapsLibrary(err.to_string()),
            LibraryError::PathOverlapsDataDir => Self::PathOverlapsDataDir(err.to_string()),
            // Unreachable: creation names no existing library, parses no id and
            // starts no scan.
            LibraryError::InvalidId
            | LibraryError::LibraryNotFound
            | LibraryError::ScanInProgress
            | LibraryError::Db(_) => Self::Internal(err.to_string()),
        }
    }
}

impl From<LibraryError> for LibraryScanError {
    fn from(err: LibraryError) -> Self {
        match err {
            LibraryError::LibraryNotFound => Self::LibraryNotFound(err.to_string()),
            // Reachable here: a rescan revisits the root, which may have gone.
            // Passed through verbatim: `IndexError::PathNotFound` guarantees
            // its message carries no filesystem path (NFR-108).
            LibraryError::PathNotFound(_) => Self::PathNotFound(err.to_string()),
            LibraryError::ScanInProgress => Self::ScanInProgress(err.to_string()),
            // Unreachable: the id arrives parsed, and containment and overlap
            // are decided at registration.
            LibraryError::InvalidId
            | LibraryError::PathOutsideRoot(_)
            | LibraryError::PathOverlapsLibrary
            | LibraryError::PathOverlapsDataDir
            | LibraryError::Db(_) => Self::Internal(err.to_string()),
        }
    }
}

impl From<LibraryError> for LibraryScanReadError {
    fn from(err: LibraryError) -> Self {
        match err {
            LibraryError::LibraryNotFound => Self::LibraryNotFound(err.to_string()),
            // Unreachable: the id arrives parsed, and reading a job validates
            // no path and starts no scan.
            LibraryError::InvalidId
            | LibraryError::PathNotFound(_)
            | LibraryError::PathOutsideRoot(_)
            | LibraryError::PathOverlapsLibrary
            | LibraryError::PathOverlapsDataDir
            | LibraryError::ScanInProgress
            | LibraryError::Db(_) => Self::Internal(err.to_string()),
        }
    }
}

/// What `/v1/libraries/{id}` and its subresources capture.
#[derive(Debug, Schema, PathParams)]
pub struct LibraryPath {
    /// Library id (UUID).
    pub id: String,
}

/// What `/v1/admin/libraries/{id}/scan` captures. A `Uuid` rather than
/// [`LibraryPath`]'s string, per the wire conventions: a malformed id is the
/// `Path` extractor's 400. The other library routes move with #190.
#[derive(Debug, Schema, PathParams)]
pub struct LibraryScanPath {
    /// Library id (UUID).
    pub id: uuid::Uuid,
}

/// What `/v1/admin/media/{id}/refresh` captures.
#[derive(Debug, Schema, PathParams)]
pub struct MediaPath {
    /// Media id (movie or show UUID).
    pub id: String,
}

/// What `/v1/admin/users/{id}` captures.
#[derive(Debug, Schema, PathParams)]
pub struct UserPath {
    /// User id (UUID).
    pub id: uuid::Uuid,
}

/// How the admin log list is paged.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct LogsQuery {
    /// Max entries to return (default 50).
    pub limit: Option<u32>,
    /// Number of entries to skip.
    pub offset: Option<u32>,
}

/// How the admin event snapshot is bounded.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct EventsQuery {
    /// Max events to return (default 100, max 1000).
    #[schema(maximum = 1000)]
    pub limit: Option<u32>,
}

/// How the admin user list is paged.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct UsersQuery {
    /// Max users to return (default 50, clamped to 1..=100).
    #[schema(minimum = 1, maximum = 100)]
    pub limit: Option<u64>,
    /// Number of users to skip.
    pub offset: Option<u64>,
}

// ── Library reads (any authenticated user) ─────────────────────────────────

/// Every library the caller can see.
// `InternalError` rather than one of the library error types: this reads a
// collection and parses no identifier, so `get_libraries` can only fail on the
// database. The wider type made the operation advertise a 400 and a 404 it has
// no way to produce, which a generated client turns into dead branches.
//
// Deliberately not a doc comment: Kynos publishes those as the operation's
// `description`, and why a Rust return type was narrowed is not something an
// API consumer has any use for.
#[kynos::get("/libraries", tag = Admin, operation_id = "listLibraries")]
pub async fn list_libraries(
    auth: SessionAuth,
    Inject(state): Inject<AppState>,
) -> Result<Json<Vec<Library>>, InternalError> {
    let libraries = state
        .services
        .library
        .get_libraries(auth.0.user_id.clone())
        .await
        .map_err(|e| InternalError::Internal(e.to_string()))?;
    Ok(Json(libraries))
}

/// One library by id.
#[kynos::get("/libraries/{id}", tag = Admin, operation_id = "getLibrary")]
pub async fn get_library(
    _auth: SessionAuth,
    Path(path): Path<LibraryPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<Library>, LibraryRefError> {
    match state
        .services
        .library
        .get_library_by_id(path.id.clone())
        .await?
    {
        Some(library) => Ok(Json(library)),
        None => Err(LibraryRefError::LibraryNotFound(format!(
            "library {} not found",
            path.id
        ))),
    }
}

/// The files indexed under one library.
#[kynos::get(
    "/libraries/{id}/files",
    tag = Admin,
    operation_id = "getLibraryFiles"
)]
pub async fn get_library_files(
    _auth: SessionAuth,
    Path(path): Path<LibraryPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<Vec<LibraryFile>>, LibraryRefError> {
    let files = state.services.library.get_library_files(path.id).await?;
    Ok(Json(files))
}

// ── Library mutations (admin only) ──────────────────────────────────────────

/// Register a new library root.
#[kynos::post("/admin/libraries", tag = Admin, operation_id = "createLibrary")]
pub async fn create_library(
    _auth: AdminAuth,
    Inject(state): Inject<AppState>,
    Json(body): Json<CreateLibraryRequest>,
) -> Result<Json<Library>, LibraryCreateError> {
    let library = state
        .services
        .library
        .create_library(body.name, body.root_path)
        .await?;
    Ok(Json(library))
}

/// Start a rescan of a library root. Answers at once with the scan job,
/// queued; the scan runs in the background, one per library at a time.
/// Follow it at `GET /v1/admin/libraries/{id}/scan`, or on the admin event
/// stream's `scan_progress` events.
#[kynos::post(
    "/admin/libraries/{id}/scan",
    tag = Admin,
    operation_id = "scanLibrary"
)]
pub async fn scan_library(
    _auth: AdminAuth,
    Path(path): Path<LibraryScanPath>,
    Inject(state): Inject<AppState>,
) -> Result<Accepted<Json<ScanJob>>, LibraryScanError> {
    let job = state.services.library.start_scan(path.id).await?;
    Ok(Accepted::new(Json(job)))
}

/// The latest scan of a library since the server started, running or
/// finished.
#[kynos::get(
    "/admin/libraries/{id}/scan",
    tag = Admin,
    operation_id = "getLibraryScan"
)]
pub async fn get_library_scan(
    _auth: AdminAuth,
    Path(path): Path<LibraryScanPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<ScanJob>, LibraryScanReadError> {
    match state.services.library.get_scan(path.id).await? {
        Some(job) => Ok(Json(job)),
        None => Err(LibraryScanReadError::ScanNotFound(format!(
            "library {} has not been scanned since the server started",
            path.id
        ))),
    }
}

/// Force a specific movie/show to re-run metadata enrichment on the next
/// worker sweep (FR-603). Does not rematch against a different external
/// title -- see `MetadataService::refresh_metadata`'s `MediaFilter::ByMediaId`
/// semantics.
#[kynos::post(
    "/admin/media/{id}/refresh",
    tag = Admin,
    operation_id = "refreshMediaMetadata"
)]
pub async fn refresh_media_metadata(
    _auth: AdminAuth,
    Path(path): Path<MediaPath>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, MediaRefreshError> {
    state
        .services
        .metadata
        .refresh_metadata(MediaFilter::ByMediaId(path.id))
        .await?;
    Ok(NoContent)
}

/// Remove a library and everything indexed under it.
#[kynos::delete(
    "/admin/libraries/{id}",
    tag = Admin,
    operation_id = "deleteLibrary"
)]
pub async fn delete_library(
    _auth: AdminAuth,
    Path(path): Path<LibraryPath>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, LibraryRefError> {
    if state
        .services
        .library
        .delete_library(path.id.clone())
        .await?
    {
        Ok(NoContent)
    } else {
        Err(LibraryRefError::LibraryNotFound(format!(
            "library {} not found",
            path.id
        )))
    }
}

// ── Admin logs (admin only) ─────────────────────────────────────────────────

/// A page of the operational log.
#[kynos::get("/admin/logs", tag = Admin, operation_id = "getAdminLogs")]
pub async fn get_admin_logs(
    _auth: AdminAuth,
    Query(query): Query<LogsQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<Vec<AdminLogEntryDto>>, InternalError> {
    let logs = state
        .services
        .admin_log
        .get_logs(query.limit.unwrap_or(50), query.offset.unwrap_or(0))
        .await
        .map_err(|e| InternalError::Internal(e.to_string()))?;
    Ok(Json(logs.into_iter().map(AdminLogEntryDto::from).collect()))
}

/// How many operational log entries exist.
#[kynos::get(
    "/admin/logs/count",
    tag = Admin,
    operation_id = "getAdminLogCount"
)]
pub async fn get_admin_log_count(
    _auth: AdminAuth,
    Inject(state): Inject<AppState>,
) -> Result<Json<AdminLogCountResponse>, InternalError> {
    let count = state
        .services
        .admin_log
        .count()
        .await
        .map_err(|e| InternalError::Internal(e.to_string()))?;
    Ok(Json(AdminLogCountResponse { count }))
}

// ── Admin events: snapshot + SSE live stream (admin only) ───────────────────

/// The most recent admin events, as a snapshot.
#[kynos::get("/admin/events", tag = Admin, operation_id = "getAdminEvents")]
pub async fn get_admin_events(
    _auth: AdminAuth,
    Query(query): Query<EventsQuery>,
    Inject(state): Inject<AppState>,
) -> Json<Vec<AdminEventDto>> {
    let limit = (query.limit.unwrap_or(100) as usize).min(1000);
    let events = state.services.notification.recent_events(limit);
    Json(events.into_iter().map(AdminEventDto::from).collect())
}

/// Headers an SSE response needs to survive an intermediary.
///
/// Kynos's `Sse` sets `Content-Type` and nothing else, so these are Beam's to
/// supply. Without `X-Accel-Buffering` an nginx in front of the server buffers
/// the stream and the admin dashboard updates in bursts minutes apart; without
/// `Cache-Control` an intermediary may serve a replay of the first events to a
/// reconnecting client.
#[derive(Schema, HeaderParams)]
pub struct StreamHeaders {
    #[header(rename = "Cache-Control")]
    cache_control: String,

    #[header(rename = "X-Accel-Buffering")]
    accel_buffering: String,
}

/// Live stream of admin events over Server-Sent Events, replacing the old
/// GraphQL-subscription-over-websocket transport with a plain HTTP stream a
/// browser's `EventSource` can consume directly.
///
/// This is the one operation that forces the whole document to OpenAPI 3.2:
/// `Sse<S>` describes itself with `itemSchema`, and the JSON in each event's
/// `data` field with `contentMediaType`/`contentSchema`. Under Salvo this
/// endpoint's `200` was emitted with no content type and no schema at all.
///
/// Authentication resolves before the stream is committed -- `AdminAuth` is an
/// extractor, so a 401 or 403 is a normal response rather than an error
/// arriving after a 200 is already on the wire.
#[kynos::get(
    "/admin/events/stream",
    tag = Admin,
    operation_id = "streamAdminEvents"
)]
pub async fn stream_admin_events(
    _auth: AdminAuth,
    Inject(state): Inject<AppState>,
) -> WithHeaders<
    Sse<impl futures_core::Stream<Item = Result<Event<AdminEventDto>, Infallible>>>,
    StreamHeaders,
> {
    let mut receiver = state.services.notification.subscribe();

    let events = stream! {
        loop {
            match receiver.recv().await {
                Ok(event) => yield Ok(Event::new(AdminEventDto::from(event))),
                // The sender is gone: the process is shutting down.
                Err(RecvError::Closed) => break,
                // A slow consumer fell behind the broadcast buffer. Beam owns
                // retention policy, and the policy is "skip and keep going":
                // these are advisory operational events, and a dashboard that
                // died because it blinked is worse than one that missed a row.
                Err(RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "admin events SSE stream lagged");
                }
            }
        }
    };

    WithHeaders::new(
        Sse::new(events).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .comment("still connected"),
        ),
        StreamHeaders {
            cache_control: "no-cache".to_owned(),
            accel_buffering: "no".to_owned(),
        },
    )
}

/// The error type an admin event stream cannot produce.
///
/// Named rather than `!` so the `Sse` bound has something concrete to resolve:
/// the loop above either yields an event or ends, and there is no third case.
pub type Infallible = std::convert::Infallible;

// ── Admin user management (admin only, issue #85) ───────────────────────────

/// Paginated list of every account, for the admin users tab. `is_admin` in
/// each row is informational/read-only: it is derived from the IdP-asserted
/// admin claim at every login, never editable locally.
#[kynos::get("/admin/users", tag = Admin, operation_id = "listAdminUsers")]
pub async fn list_admin_users(
    _auth: AdminAuth,
    Query(query): Query<UsersQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<AdminUserListResponse>, InternalError> {
    let internal = |e: sea_orm::DbErr| InternalError::Internal(e.to_string());

    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);

    let items = state
        .services
        .user_repo
        .list_page(limit, offset)
        .await
        .map_err(internal)?;
    let total = state.services.user_repo.count().await.map_err(internal)?;

    Ok(Json(AdminUserListResponse {
        items: items.into_iter().map(AdminUserDto::from).collect(),
        total,
    }))
}

/// Sets a user's `disabled` moderation flag. Disabling immediately revokes
/// every session of the target (they cannot act again until re-enabled) and
/// blocks future logins at the OIDC callback. An admin cannot disable their
/// own account (400).
///
/// `disabled` is deliberately the only mutable field: `is_admin` is derived
/// from the IdP-asserted admin claim and recomputed on every login, so a
/// local toggle would be silently overwritten at the user's next login --
/// admin grants/revocations belong at the IdP (issue #85).
#[kynos::patch("/admin/users/{id}", tag = Admin, operation_id = "updateAdminUser")]
pub async fn update_admin_user(
    auth: AdminAuth,
    Path(path): Path<UserPath>,
    Inject(state): Inject<AppState>,
    Json(body): Json<UpdateAdminUserRequest>,
) -> Result<NoContent, AdminUserError> {
    let internal = |e: sea_orm::DbErr| AdminUserError::Internal(e.to_string());

    let target = state
        .services
        .user_repo
        .find_by_id(path.id)
        .await
        .map_err(internal)?
        .ok_or_else(|| AdminUserError::UserNotFound(format!("user {} not found", path.id)))?;

    if body.disabled && target.id.to_string() == auth.0.user_id {
        return Err(AdminUserError::CannotDisableSelf(
            "cannot disable your own account".to_owned(),
        ));
    }

    state
        .services
        .user_repo
        .set_disabled(target.id, body.disabled)
        .await
        .map_err(internal)?;

    if body.disabled {
        // Revoke every live session so the disable takes effect immediately,
        // not just at the next login attempt.
        state
            .services
            .session_store
            .delete_all_for_user(&target.id.to_string())
            .await
            .map_err(|e| AdminUserError::Internal(e.to_string()))?;
    }

    Ok(NoContent)
}

// ── Admin system status (admin only, issue #85) ─────────────────────────────

/// How many of the newest `library_scan` admin log entries the status
/// endpoint returns as recent scan history.
const RECENT_SCANS_LIMIT: u32 = 10;

/// Operational snapshot for the admin system-status tab: process uptime and
/// version, entity counts, the metadata-enrichment queue state, recent
/// library-scan history, and how the filesystem watcher observes each library.
#[kynos::get("/admin/status", tag = Admin, operation_id = "getAdminStatus")]
pub async fn get_admin_status(
    _auth: AdminAuth,
    Inject(state): Inject<AppState>,
) -> Result<Json<AdminStatusResponse>, InternalError> {
    let internal = |e: sea_orm::DbErr| InternalError::Internal(e.to_string());

    let users = state.services.user_repo.count().await.map_err(internal)?;
    let libraries = state
        .services
        .library_repo
        .count()
        .await
        .map_err(internal)?;
    let files = state
        .services
        .file_repo
        .count_all()
        .await
        .map_err(internal)?;
    let enrichment = state
        .services
        .enrichment_repo
        .count_by_status()
        .await
        .map_err(internal)?;
    let recent_scans = state
        .services
        .admin_log
        .get_logs_by_category(
            beam_domain::models::AdminLogCategory::LibraryScan,
            RECENT_SCANS_LIMIT,
            0,
        )
        .await
        .map_err(|e| InternalError::Internal(e.to_string()))?;
    let library_ids = state
        .services
        .library_repo
        .find_all()
        .await
        .map_err(internal)?
        .into_iter()
        .map(|library| library.id);
    let watcher = WatcherStatus::from_snapshot(state.services.watch_status.snapshot(), library_ids);

    Ok(Json(AdminStatusResponse {
        uptime_secs: state.uptime_secs(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        counts: AdminStatusCounts {
            users,
            libraries,
            files,
        },
        enrichment: EnrichmentQueueCounts::from(enrichment),
        recent_scans: recent_scans.into_iter().map(RecentScanDto::from).collect(),
        watcher,
    }))
}

// ── Anonymous library report (admin only, issue #93) ───────────────────────

/// The anonymous library report this server would send now, byte for byte,
/// and whether and where it would go. Sends nothing: this is how an admin
/// decides whether to set `BEAM_TELEMETRY_URL`, and checks what it sends once
/// set.
#[kynos::get(
    "/admin/telemetry/library",
    tag = Admin,
    operation_id = "previewLibraryTelemetry"
)]
pub async fn preview_library_telemetry(
    _auth: AdminAuth,
    Inject(state): Inject<AppState>,
) -> Result<Json<LibraryTelemetryPreview>, InternalError> {
    let telemetry = &state.services.telemetry;
    let LibraryReportPreview {
        destination_origin,
        last_sent_at,
        next_send_at,
        report,
        content_type,
        payload,
    } = telemetry
        .preview()
        .await
        .map_err(|e| InternalError::Internal(e.to_string()))?;
    Ok(Json(LibraryTelemetryPreview {
        destination_configured: telemetry.destination_configured(),
        destination_origin,
        last_sent_at,
        next_send_at,
        report,
        content_type: content_type.to_owned(),
        // The encoder writes JSON, which is UTF-8; lossless in practice, and
        // never a panic if that ever stopped being true.
        payload: String::from_utf8_lossy(&payload).into_owned(),
    }))
}

// ── Playback telemetry report (admin only, issue #143) ─────────────────────

/// Which UTC days the playback telemetry report covers.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct PlaybackTelemetryQuery {
    /// First day, inclusive (default: 29 days before `to`).
    pub from: Option<chrono::NaiveDate>,
    /// Last day, inclusive (default: today, UTC). At most 366 days after
    /// `from`.
    pub to: Option<chrono::NaiveDate>,
}

impl From<ReportError> for PlaybackTelemetryReportError {
    fn from(err: ReportError) -> Self {
        match err {
            ReportError::InvalidDateRange(message) => Self::InvalidDateRange(message),
            ReportError::Db(_) => Self::Internal(err.to_string()),
        }
    }
}

/// Playback telemetry counts, summed over a range of days.
///
/// The starts, start failures, rebuffers and source switches clients
/// reported. Operator-local: none of it is part of the anonymous library
/// report, and no row names a user, file or title. Readable whether or not
/// collection is enabled.
#[kynos::get(
    "/admin/telemetry/playback",
    tag = Admin,
    operation_id = "getPlaybackTelemetry"
)]
pub async fn get_playback_telemetry(
    _auth: AdminAuth,
    Query(query): Query<PlaybackTelemetryQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<PlaybackTelemetryReport>, PlaybackTelemetryReportError> {
    let report = state
        .services
        .playback_telemetry
        .report(query.from, query.to)
        .await?;
    Ok(Json(report))
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod admin_tests;

#[cfg(test)]
#[path = "admin_telemetry_tests.rs"]
mod admin_telemetry_tests;
