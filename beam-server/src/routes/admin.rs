//! `/v1/libraries` (any authenticated user -- browsing libraries is a normal
//! user action) and `/v1/admin/*` (admin-only: mutating a library, or seeing
//! operational logs/events). Tightens gap G8 from the pre-REST GraphQL
//! surface, where library create/scan/delete were only `AuthGuard`-gated
//! (any signed-in user, not just admins).

use std::convert::Infallible;

use async_stream::stream;
use salvo::prelude::*;
use tokio::sync::broadcast::error::RecvError;

use crate::models::{
    AdminEventDto, AdminLogCountResponse, AdminLogEntryDto, AdminStatusCounts, AdminStatusResponse,
    AdminUserDto, AdminUserListResponse, CreateLibraryRequest, EnrichmentQueueCounts, Library,
    LibraryFile, RecentScanDto, ScanLibraryResponse, UpdateAdminUserRequest,
};
use crate::routes::api_error::{ApiError, obtain_state, require_admin, require_auth};
use crate::services::library::LibraryError;
use crate::services::metadata::{MediaFilter, MetadataError};

impl From<MetadataError> for ApiError {
    fn from(err: MetadataError) -> Self {
        match err {
            MetadataError::MediaNotFound => ApiError::NotFound(err.to_string()),
            MetadataError::Unsupported(msg) => ApiError::BadRequest(msg),
            MetadataError::InternalError(msg) => ApiError::Internal(msg),
        }
    }
}

impl From<LibraryError> for ApiError {
    fn from(err: LibraryError) -> Self {
        match err {
            LibraryError::LibraryNotFound => ApiError::NotFound(err.to_string()),
            LibraryError::InvalidId => ApiError::BadRequest(err.to_string()),
            LibraryError::PathNotFound(_) | LibraryError::Validation(_) => {
                ApiError::BadRequest(err.to_string())
            }
            LibraryError::UserNotFound | LibraryError::Db(_) => ApiError::Internal(err.to_string()),
        }
    }
}

// ── Library reads (any authenticated user) ─────────────────────────────────

#[endpoint(tags("admin"))]
pub async fn list_libraries(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<Library>>, ApiError> {
    let state = obtain_state(depot)?;
    let user = require_auth(req, state).await?;
    let libraries = state.services.library.get_libraries(user.user_id).await?;
    Ok(Json(libraries))
}

#[endpoint(
    tags("admin"),
    parameters(
        ("id" = String, description = "Library id (UUID)"),
    ),
)]
pub async fn get_library(req: &mut Request, depot: &mut Depot) -> Result<Json<Library>, ApiError> {
    let state = obtain_state(depot)?;
    require_auth(req, state).await?;
    let id: String = req.param::<String>("id").unwrap_or_default();
    match state.services.library.get_library_by_id(id.clone()).await? {
        Some(library) => Ok(Json(library)),
        None => Err(ApiError::NotFound(format!("library {id} not found"))),
    }
}

#[endpoint(
    tags("admin"),
    parameters(
        ("id" = String, description = "Library id (UUID)"),
    ),
)]
pub async fn get_library_files(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<LibraryFile>>, ApiError> {
    let state = obtain_state(depot)?;
    require_auth(req, state).await?;
    let id: String = req.param::<String>("id").unwrap_or_default();
    let files = state.services.library.get_library_files(id).await?;
    Ok(Json(files))
}

// ── Library mutations (admin only) ──────────────────────────────────────────

#[endpoint(
    tags("admin"),
        request_body = CreateLibraryRequest,
)]
pub async fn create_library(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Library>, ApiError> {
    let state = obtain_state(depot)?;
    require_admin(req, state).await?;
    let body: CreateLibraryRequest = req
        .parse_json()
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let library = state
        .services
        .library
        .create_library(body.name, body.root_path)
        .await?;
    Ok(Json(library))
}

#[endpoint(
    tags("admin"),
    parameters(
        ("id" = String, description = "Library id (UUID)"),
    ),
)]
pub async fn scan_library(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ScanLibraryResponse>, ApiError> {
    let state = obtain_state(depot)?;
    require_admin(req, state).await?;
    let id: String = req.param::<String>("id").unwrap_or_default();
    let added = state.services.library.scan_library(id).await?;
    Ok(Json(ScanLibraryResponse { added }))
}

/// Force a specific movie/show to re-run metadata enrichment on the next
/// worker sweep (FR-603). Does not rematch against a different external
/// title -- see `MetadataService::refresh_metadata`'s `MediaFilter::ByMediaId`
/// semantics.
#[endpoint(
    tags("admin"),
    parameters(
        ("id" = String, description = "Media id (movie or show UUID)"),
    ),
)]
pub async fn refresh_media_metadata(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), ApiError> {
    let state = obtain_state(depot)?;
    require_admin(req, state).await?;
    let id: String = req.param::<String>("id").unwrap_or_default();
    state
        .services
        .metadata
        .refresh_metadata(MediaFilter::ByMediaId(id))
        .await?;
    res.status_code(StatusCode::NO_CONTENT);
    Ok(())
}

#[endpoint(
    tags("admin"),
    parameters(
        ("id" = String, description = "Library id (UUID)"),
    ),
)]
pub async fn delete_library(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), ApiError> {
    let state = obtain_state(depot)?;
    require_admin(req, state).await?;
    let id: String = req.param::<String>("id").unwrap_or_default();
    let deleted = state.services.library.delete_library(id.clone()).await?;
    if deleted {
        res.status_code(StatusCode::NO_CONTENT);
        Ok(())
    } else {
        Err(ApiError::NotFound(format!("library {id} not found")))
    }
}

// ── Admin logs (admin only) ─────────────────────────────────────────────────

#[endpoint(
    tags("admin"),
    parameters(
        ("limit" = Option<u32>, Query, description = "Max entries to return (default 50)"),
        ("offset" = Option<u32>, Query, description = "Number of entries to skip"),
    ),
)]
pub async fn get_admin_logs(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<AdminLogEntryDto>>, ApiError> {
    let state = obtain_state(depot)?;
    require_admin(req, state).await?;
    let limit = req.query::<u32>("limit").unwrap_or(50);
    let offset = req.query::<u32>("offset").unwrap_or(0);
    let logs = state
        .services
        .admin_log
        .get_logs(limit, offset)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(logs.into_iter().map(AdminLogEntryDto::from).collect()))
}

#[endpoint(tags("admin"))]
pub async fn get_admin_log_count(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AdminLogCountResponse>, ApiError> {
    let state = obtain_state(depot)?;
    require_admin(req, state).await?;
    let count = state
        .services
        .admin_log
        .count()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(AdminLogCountResponse { count }))
}

// ── Admin events: snapshot + SSE live stream (admin only) ───────────────────

#[endpoint(
    tags("admin"),
    parameters(
        ("limit" = Option<u32>, Query, description = "Max events to return (default 100, max 1000)"),
    ),
)]
pub async fn get_admin_events(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<AdminEventDto>>, ApiError> {
    let state = obtain_state(depot)?;
    require_admin(req, state).await?;
    let limit = (req.query::<u32>("limit").unwrap_or(100) as usize).min(1000);
    let events = state.services.notification.recent_events(limit);
    Ok(Json(events.into_iter().map(AdminEventDto::from).collect()))
}

/// Live stream of admin events over Server-Sent Events, replacing the old
/// GraphQL-subscription-over-websocket transport with a plain HTTP stream a
/// browser's `EventSource` can consume directly.
#[endpoint(tags("admin"))]
pub async fn stream_admin_events(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), ApiError> {
    let state = obtain_state(depot)?;
    require_admin(req, state).await?;

    let mut receiver = state.services.notification.subscribe();
    let event_stream = stream! {
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let dto = AdminEventDto::from(event);
                    match SseEvent::default().json(dto) {
                        Ok(sse_event) => yield Ok::<_, Infallible>(sse_event),
                        Err(e) => tracing::warn!(error = %e, "failed to serialize admin event for SSE"),
                    }
                }
                Err(RecvError::Closed) => break,
                Err(RecvError::Lagged(skipped)) => {
                    tracing::warn!("admin events SSE stream lagged, skipped {} events", skipped);
                }
            }
        }
    };
    salvo::sse::stream(res, event_stream);
    Ok(())
}

// ── Admin user management (admin only, issue #85) ───────────────────────────

/// Paginated list of every account, for the admin users tab. `is_admin` in
/// each row is informational/read-only: it is derived from the IdP-asserted
/// admin claim at every login, never editable locally.
#[endpoint(
    tags("admin"),
    parameters(
        ("limit" = Option<u64>, Query, description = "Max users to return (default 50, clamped to 1..=100)"),
        ("offset" = Option<u64>, Query, description = "Number of users to skip"),
    ),
)]
pub async fn list_admin_users(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AdminUserListResponse>, ApiError> {
    let state = obtain_state(depot)?;
    require_admin(req, state).await?;

    let limit = req.query::<u64>("limit").unwrap_or(50).clamp(1, 100);
    let offset = req.query::<u64>("offset").unwrap_or(0);

    let items = state
        .services
        .user_repo
        .list_page(limit, offset)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let total = state
        .services
        .user_repo
        .count()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

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
#[endpoint(
    tags("admin"),
    parameters(
        ("id" = String, description = "User id (UUID)"),
    ),
    request_body = UpdateAdminUserRequest,
)]
pub async fn update_admin_user(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), ApiError> {
    let state = obtain_state(depot)?;
    let caller = require_admin(req, state).await?;

    let id_raw: String = req.param::<String>("id").unwrap_or_default();
    let user_id = uuid::Uuid::parse_str(&id_raw)
        .map_err(|_| ApiError::BadRequest(format!("invalid user id: {id_raw}")))?;
    let body: UpdateAdminUserRequest = req
        .parse_json()
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    let target = state
        .services
        .user_repo
        .find_by_id(user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("user {id_raw} not found")))?;

    if body.disabled && target.id.to_string() == caller.user_id {
        return Err(ApiError::BadRequest(
            "cannot disable your own account".to_string(),
        ));
    }

    state
        .services
        .user_repo
        .set_disabled(target.id, body.disabled)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    if body.disabled {
        // Revoke every live session so the disable takes effect immediately,
        // not just at the next login attempt.
        state
            .services
            .session_store
            .delete_all_for_user(&target.id.to_string())
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    res.status_code(StatusCode::NO_CONTENT);
    Ok(())
}

// ── Admin system status (admin only, issue #85) ─────────────────────────────

/// How many of the newest `library_scan` admin log entries the status
/// endpoint returns as recent scan history.
const RECENT_SCANS_LIMIT: u32 = 10;

/// Operational snapshot for the admin system-status tab: process uptime and
/// version, entity counts, the metadata-enrichment queue state, and recent
/// library-scan history.
#[endpoint(tags("admin"))]
pub async fn get_admin_status(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AdminStatusResponse>, ApiError> {
    let state = obtain_state(depot)?;
    require_admin(req, state).await?;

    let internal = |e: sea_orm::DbErr| ApiError::Internal(e.to_string());

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
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(AdminStatusResponse {
        uptime_secs: state.uptime_secs(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        counts: AdminStatusCounts {
            users,
            libraries,
            files,
        },
        enrichment: EnrichmentQueueCounts::from(enrichment),
        recent_scans: recent_scans.into_iter().map(RecentScanDto::from).collect(),
    }))
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod admin_tests;
