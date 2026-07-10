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
    AdminEventDto, AdminLogCountResponse, AdminLogEntryDto, CreateLibraryRequest, Library,
    LibraryFile, ScanLibraryResponse,
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

#[cfg(test)]
#[path = "admin_tests.rs"]
mod admin_tests;
