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

use crate::models::enrichment::MetadataProviderStatus;
use crate::models::{
    AdminEvent, AdminEventConnection, AdminLogConnection, AdminLogEntry, AdminStatus,
    AdminStatusCounts, AdminUser, AdminUserConnection, CreateLibraryRequest, EnrichmentQueueCounts,
    FieldError, Library, LibraryConnection, LibraryFile, LibraryFileConnection,
    LibraryTelemetryPreview, PageInfo, PlaybackTelemetryReport, RecentScan, ScanJob,
    UpdateAdminUserRequest, UserAccess, WatcherStatus,
};
use crate::routes::api_error::{
    AdminAuth, AdminListError, AdminUserError, InternalError, LibraryCreateError,
    LibraryFilesError, LibraryRefError, LibraryScanError, LibraryScanReadError, MediaRefreshError,
    PlaybackTelemetryReportError, SessionAuth, UserAccessReadError, UserAccessUpdateError,
};
use crate::routes::playback::{caller_access, page_size};
use crate::routes::tags::Admin;
use crate::services::library::LibraryError;
use crate::services::library_files_cursor;
use crate::services::offset_cursor::{self, OffsetList};
use crate::services::playback_telemetry::ReportError;
use crate::services::telemetry::LibraryReportPreview;
use crate::state::AppState;
use beam_domain::models::access::{AccessPolicy, ContentRating, LibraryAccess};
use beam_index::services::enrichment::control::{ControlError, RefreshScope};

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

impl From<ControlError> for MediaRefreshError {
    fn from(err: ControlError) -> Self {
        match err {
            ControlError::MediaNotFound(_) => Self::MediaNotFound(err.to_string()),
            // Unreachable: a refresh of one title names no library, pins no
            // id, locks nothing and searches no provider.
            ControlError::LibraryNotFound(_)
            | ControlError::InvalidExternalRef(_)
            | ControlError::ProviderNotConfigured(_)
            | ControlError::ExternalRefTaken(_)
            | ControlError::FieldNotLockable(_)
            | ControlError::Provider(_)
            | ControlError::Db(_)
            | ControlError::Index(_) => Self::Internal(err.to_string()),
        }
    }
}

impl From<LibraryError> for LibraryRefError {
    fn from(err: LibraryError) -> Self {
        match err {
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

impl From<LibraryError> for LibraryFilesError {
    fn from(err: LibraryError) -> Self {
        match err {
            LibraryError::LibraryNotFound => Self::LibraryNotFound(err.to_string()),
            // Unreachable: reading a page of files validates no path and
            // starts no scan.
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
            // Unreachable: creation names no existing library and starts no
            // scan.
            LibraryError::LibraryNotFound | LibraryError::ScanInProgress | LibraryError::Db(_) => {
                Self::Internal(err.to_string())
            }
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
            // Unreachable: containment and overlap are decided at
            // registration.
            LibraryError::PathOutsideRoot(_)
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
            // Unreachable: reading a job validates no path and starts no
            // scan.
            LibraryError::PathNotFound(_)
            | LibraryError::PathOutsideRoot(_)
            | LibraryError::PathOverlapsLibrary
            | LibraryError::PathOverlapsDataDir
            | LibraryError::ScanInProgress
            | LibraryError::Db(_) => Self::Internal(err.to_string()),
        }
    }
}

/// What `/v1/libraries/{id}`, `/v1/admin/libraries/{id}` and their
/// subresources capture. A `Uuid`, per the wire conventions: a malformed id is
/// the `Path` extractor's 400.
#[derive(Debug, Schema, PathParams)]
pub struct LibraryPath {
    /// Library id (UUID).
    pub id: uuid::Uuid,
}

/// What `/v1/admin/media/{id}/refresh` captures.
#[derive(Debug, Schema, PathParams)]
pub struct MediaPath {
    /// Media id (movie or show UUID).
    pub id: uuid::Uuid,
}

/// What `/v1/admin/users/{id}` captures.
#[derive(Debug, Schema, PathParams)]
pub struct UserPath {
    /// User id (UUID).
    pub id: uuid::Uuid,
}

/// The largest admin log page, and the one a request naming none gets.
const LOGS_MAX: u32 = 200;
const LOGS_DEFAULT: u32 = 50;
/// The largest admin event snapshot, and the one a request naming none gets.
const EVENTS_MAX: u32 = 1000;
const EVENTS_DEFAULT: u32 = 100;
/// The largest admin user page, and the one a request naming none gets.
const USERS_MAX: u32 = 100;
const USERS_DEFAULT: u32 = 50;
/// The largest page of a library's files, and the one a request naming none
/// gets.
const FILES_MAX: u32 = 500;
const FILES_DEFAULT: u32 = 100;

/// How the admin log list is paged.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct LogsQuery {
    /// This many entries (1-200, default 50).
    #[schema(minimum = 1, maximum = 200)]
    pub first: Option<u32>,
    /// An opaque cursor -- a page's `end_cursor` -- to page on from.
    pub after: Option<String>,
}

/// How the admin event snapshot is bounded.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct EventsQuery {
    /// At most this many of the most recent events (1-1000, default 100).
    #[schema(minimum = 1, maximum = 1000)]
    pub first: Option<u32>,
}

/// How the admin user list is paged.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct UsersQuery {
    /// This many users (1-100, default 50).
    #[schema(minimum = 1, maximum = 100)]
    pub first: Option<u32>,
    /// An opaque cursor -- a page's `end_cursor` -- to page on from.
    pub after: Option<String>,
}

/// How a library's files are paged.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct FilesQuery {
    /// This many files (1-500, default 100).
    #[schema(minimum = 1, maximum = 500)]
    pub first: Option<u32>,
    /// An opaque cursor -- a page's `end_cursor` -- to page on from.
    pub after: Option<String>,
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
) -> Result<Json<LibraryConnection>, InternalError> {
    let (_, policy) = caller_access(&state, &auth).await?;
    let libraries = state
        .services
        .library
        .get_libraries(auth.0.user_id.clone())
        .await
        .map_err(|e| InternalError::Internal(e.to_string()))?
        .into_iter()
        .filter(|library| policy.admits_library(library.id))
        .collect();
    Ok(Json(LibraryConnection {
        items: libraries,
        page_info: PageInfo::complete(),
    }))
}

/// One library by id. A library the caller has not been granted answers as
/// one that does not exist.
#[kynos::get("/libraries/{id}", tag = Admin, operation_id = "getLibrary")]
pub async fn get_library(
    auth: SessionAuth,
    Path(path): Path<LibraryPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<Library>, LibraryRefError> {
    let (_, policy) = caller_access(&state, &auth).await?;
    let library = if policy.admits_library(path.id) {
        state.services.library.get_library_by_id(path.id).await?
    } else {
        None
    };
    match library {
        Some(library) => Ok(Json(library)),
        None => Err(LibraryRefError::LibraryNotFound(format!(
            "library {} not found",
            path.id
        ))),
    }
}

/// The files indexed under one library, a page at a time, by path.
///
/// Pass `page_info.end_cursor` as `after` for the next page; a cursor
/// another library's list issued is refused.
///
/// A library the caller has not been granted answers as one that does not
/// exist, and so does every library for a caller with a maximum content
/// rating: a file's path names its title whatever that title's rating.
#[kynos::get(
    "/libraries/{id}/files",
    tag = Admin,
    operation_id = "getLibraryFiles"
)]
pub async fn get_library_files(
    auth: SessionAuth,
    Path(path): Path<LibraryPath>,
    Query(query): Query<FilesQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<LibraryFileConnection>, LibraryFilesError> {
    let (_, policy) = caller_access(&state, &auth)
        .await
        .map_err(|err| LibraryFilesError::Internal(err.to_string()))?;
    if !policy.admits_library(path.id) || policy.max_rating.is_some() {
        return Err(LibraryFilesError::LibraryNotFound(format!(
            "library {} not found",
            path.id
        )));
    }
    let FilesQuery { first, after } = query;
    let first =
        page_size(first, FILES_DEFAULT, FILES_MAX).map_err(LibraryFilesError::InvalidPagination)?;
    let after = after
        .as_deref()
        .map(|cursor| library_files_cursor::decode(path.id, cursor))
        .transpose()
        .map_err(|err| LibraryFilesError::InvalidCursor(err.to_string()))?;
    let has_previous_page = after.is_some();

    let page = state
        .services
        .library
        .get_library_files(path.id, after, first)
        .await?;
    let cursor = |file: &LibraryFile| library_files_cursor::encode(path.id, &file.path, file.id);
    Ok(Json(LibraryFileConnection {
        page_info: PageInfo {
            has_next_page: page.has_next_page,
            has_previous_page,
            start_cursor: page.items.first().map(cursor),
            end_cursor: page.items.last().map(cursor),
        },
        items: page.items,
    }))
}

// ── Library mutations (admin only) ──────────────────────────────────────────

/// Register a new library root.
#[kynos::post("/admin/libraries", tag = Admin, operation_id = "createLibrary")]
pub async fn create_library(
    _auth: AdminAuth,
    Inject(state): Inject<AppState>,
    Json(body): Json<CreateLibraryRequest>,
) -> Result<Json<Library>, LibraryCreateError> {
    let errors = create_library_errors(&body);
    if !errors.is_empty() {
        return Err(LibraryCreateError::ValidationFailed { errors });
    }
    let CreateLibraryRequest { name, root_path } = body;
    let library = state
        .services
        .library
        .create_library(name, root_path)
        .await?;
    Ok(Json(library))
}

/// The longest library name, in characters.
const LIBRARY_NAME_MAX_CHARS: usize = 200;

/// Every rule `body` breaks, by field: a blank name, a name over
/// [`LIBRARY_NAME_MAX_CHARS`], a blank root path. Blank is empty or only
/// whitespace.
fn create_library_errors(body: &CreateLibraryRequest) -> Vec<FieldError> {
    let CreateLibraryRequest { name, root_path } = body;
    let mut errors = Vec::new();
    if name.trim().is_empty() {
        errors.push(FieldError {
            pointer: "/name".to_owned(),
            detail: "a library needs a name".to_owned(),
        });
    } else if name.chars().count() > LIBRARY_NAME_MAX_CHARS {
        errors.push(FieldError {
            pointer: "/name".to_owned(),
            detail: format!("a library name is at most {LIBRARY_NAME_MAX_CHARS} characters"),
        });
    }
    if root_path.trim().is_empty() {
        errors.push(FieldError {
            pointer: "/root_path".to_owned(),
            detail: "a library needs a root path".to_owned(),
        });
    }
    errors
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
    Path(path): Path<LibraryPath>,
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
    Path(path): Path<LibraryPath>,
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

/// Queue a specific movie/show for another enrichment pass, whatever its
/// status (FR-308, FR-603). It keeps its match, re-fetched fresh, unless
/// `rematch`, which discards it to be searched for again (by its pin, if it
/// has one); to match it to a title of your choosing, fix the match
/// (`POST /v1/admin/media/{id}/match`).
#[kynos::post(
    "/admin/media/{id}/refresh",
    tag = Admin,
    operation_id = "refreshMediaMetadata"
)]
pub async fn refresh_media_metadata(
    auth: AdminAuth,
    Path(path): Path<MediaPath>,
    Query(query): Query<crate::routes::enrichment::RefreshQuery>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, MediaRefreshError> {
    let crate::routes::enrichment::RefreshQuery { rematch } = query;
    state
        .services
        .enrichment_control
        .refresh(
            RefreshScope::Title(path.id),
            rematch.unwrap_or(false),
            &auth.0.user_id,
        )
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
    if state.services.library.delete_library(path.id).await? {
        Ok(NoContent)
    } else {
        Err(LibraryRefError::LibraryNotFound(format!(
            "library {} not found",
            path.id
        )))
    }
}

// ── Admin logs (admin only) ─────────────────────────────────────────────────

/// A page of the operational log, newest first.
///
/// Pass `page_info.end_cursor` as `after` for the next page. `total` counts
/// every entry; an entry written while paging shifts the pages after it by
/// one, since the log is read by position.
#[kynos::get("/admin/logs", tag = Admin, operation_id = "getAdminLogs")]
pub async fn get_admin_logs(
    _auth: AdminAuth,
    Query(query): Query<LogsQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<AdminLogConnection>, AdminListError> {
    let internal =
        |e: crate::services::admin_log::AdminLogError| AdminListError::Internal(e.to_string());
    let LogsQuery { first, after } = query;
    let first =
        page_size(first, LOGS_DEFAULT, LOGS_MAX).map_err(AdminListError::InvalidPagination)?;
    let offset = offset_cursor::start_after(OffsetList::AdminLogs, after.as_deref())
        .map_err(|err| AdminListError::InvalidCursor(err.to_string()))?;
    // The log service pages by a `u32` offset; one past it has no rows.
    let logs = match u32::try_from(offset) {
        Ok(offset) => state
            .services
            .admin_log
            .get_logs(first.get(), offset)
            .await
            .map_err(internal)?,
        Err(_) => Vec::new(),
    };
    let total = state.services.admin_log.count().await.map_err(internal)?;

    Ok(Json(AdminLogConnection {
        page_info: offset_cursor::page_info(OffsetList::AdminLogs, offset, logs.len(), total),
        items: logs.into_iter().map(AdminLogEntry::from).collect(),
        total,
    }))
}

// ── Admin events: snapshot + SSE live stream (admin only) ───────────────────

/// The most recent admin events, oldest first, as a snapshot.
///
/// At most `first` of them; `page_info.has_next_page` says whether older
/// events were left out.
#[kynos::get("/admin/events", tag = Admin, operation_id = "getAdminEvents")]
pub async fn get_admin_events(
    _auth: AdminAuth,
    Query(query): Query<EventsQuery>,
    Inject(state): Inject<AppState>,
) -> Json<AdminEventConnection> {
    let EventsQuery { first } = query;
    // Kynos holds `first` to its schema's 1-1000 before the handler runs.
    let first = first.unwrap_or(EVENTS_DEFAULT).clamp(1, EVENTS_MAX) as usize;
    // One more than the snapshot shows says whether older events exist.
    let mut events = state.services.notification.recent_events(first + 1);
    let has_next_page = events.len() > first;
    if has_next_page {
        // Oldest first: the one left out is the oldest.
        events.remove(0);
    }
    Json(AdminEventConnection {
        items: events.into_iter().map(AdminEvent::from).collect(),
        page_info: PageInfo {
            has_next_page,
            ..PageInfo::complete()
        },
    })
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
    Sse<impl futures_core::Stream<Item = Result<Event<AdminEvent>, Infallible>>>,
    StreamHeaders,
> {
    let mut receiver = state.services.notification.subscribe();

    let events = stream! {
        loop {
            match receiver.recv().await {
                Ok(event) => yield Ok(Event::new(AdminEvent::from(event))),
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
) -> Result<Json<AdminUserConnection>, AdminListError> {
    let internal = |e: sea_orm::DbErr| AdminListError::Internal(e.to_string());

    let UsersQuery { first, after } = query;
    let first =
        page_size(first, USERS_DEFAULT, USERS_MAX).map_err(AdminListError::InvalidPagination)?;
    let offset = offset_cursor::start_after(OffsetList::AdminUsers, after.as_deref())
        .map_err(|err| AdminListError::InvalidCursor(err.to_string()))?;

    let items = state
        .services
        .user_repo
        .list_page(u64::from(first.get()), offset)
        .await
        .map_err(internal)?;
    let total = state.services.user_repo.count().await.map_err(internal)?;

    Ok(Json(AdminUserConnection {
        page_info: offset_cursor::page_info(OffsetList::AdminUsers, offset, items.len(), total),
        items: items.into_iter().map(AdminUser::from).collect(),
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

// ── Per-user access (admin only, issue #263) ────────────────────────────────

/// Which libraries a user sees, and the highest content rating.
///
/// A user nobody restricted sees every library with no maximum rating.
#[kynos::get(
    "/admin/users/{id}/access",
    tag = Admin,
    operation_id = "getAdminUserAccess"
)]
pub async fn get_admin_user_access(
    _auth: AdminAuth,
    Path(path): Path<UserPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<UserAccess>, UserAccessReadError> {
    let internal = |e: sea_orm::DbErr| UserAccessReadError::Internal(e.to_string());

    let target = state
        .services
        .user_repo
        .find_by_id(path.id)
        .await
        .map_err(internal)?
        .ok_or_else(|| UserAccessReadError::UserNotFound(format!("user {} not found", path.id)))?;
    let policy = state
        .services
        .access
        .policy(target.id)
        .await
        .map_err(internal)?;
    Ok(Json(UserAccess::from(policy)))
}

/// Set which libraries a user sees, and the highest content rating, as one
/// whole: what is not in the body is not kept. Answers what is now stored.
///
/// Takes effect on the user's next request. A title the user may not see
/// answers every route as one that does not exist and is never listed, so
/// its existence does not leak. An administrator is held to their own
/// access as any user is; the `/v1/admin` operations are not.
#[kynos::put(
    "/admin/users/{id}/access",
    tag = Admin,
    operation_id = "setAdminUserAccess"
)]
pub async fn set_admin_user_access(
    _auth: AdminAuth,
    Path(path): Path<UserPath>,
    Inject(state): Inject<AppState>,
    Json(body): Json<UserAccess>,
) -> Result<Json<UserAccess>, UserAccessUpdateError> {
    let internal = |e: sea_orm::DbErr| UserAccessUpdateError::Internal(e.to_string());

    let target = state
        .services
        .user_repo
        .find_by_id(path.id)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            UserAccessUpdateError::UserNotFound(format!("user {} not found", path.id))
        })?;

    let UserAccess {
        all_libraries,
        library_ids,
        max_content_rating,
    } = body;
    let mut errors = Vec::new();
    if all_libraries && !library_ids.is_empty() {
        errors.push(FieldError {
            pointer: "/library_ids".to_owned(),
            detail: "must be empty when all_libraries is true".to_owned(),
        });
    }
    for (index, library_id) in library_ids.iter().enumerate() {
        if state
            .services
            .library_repo
            .find_by_id(*library_id)
            .await
            .map_err(internal)?
            .is_none()
        {
            errors.push(FieldError {
                pointer: format!("/library_ids/{index}"),
                detail: format!("no library has the id {library_id}"),
            });
        }
    }
    if !errors.is_empty() {
        return Err(UserAccessUpdateError::ValidationFailed { errors });
    }

    let policy = AccessPolicy {
        libraries: if all_libraries {
            LibraryAccess::All
        } else {
            LibraryAccess::Only(library_ids.into_iter().collect())
        },
        max_rating: max_content_rating.map(ContentRating::from),
    };
    state
        .services
        .access
        .replace(target.id, &policy)
        .await
        .map_err(internal)?;
    Ok(Json(UserAccess::from(policy)))
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
) -> Result<Json<AdminStatus>, InternalError> {
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
    let metadata_providers = crate::state::provider_statuses(
        &state.config,
        &state.services.enrichment_control.available_providers(),
    );

    Ok(Json(AdminStatus {
        uptime_secs: state.uptime_secs(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        counts: AdminStatusCounts {
            users,
            libraries,
            files,
        },
        enrichment: EnrichmentQueueCounts::from(enrichment),
        enrichment_providers: metadata_providers
            .iter()
            .filter_map(MetadataProviderStatus::frozen)
            .collect(),
        metadata_providers,
        recent_scans: recent_scans.into_iter().map(RecentScan::from).collect(),
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
