//! `/v1/files/{file_id}/progress`, `/v1/continue-watching` and `/v1/history`
//! (FR-507, FR-508). The reporting user is always derived from the session
//! cookie, never from the request body -- one user can never overwrite
//! another's progress.

use kynos::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::playback::{ContinueWatchingItem, HistoryItem, PlaybackProgressDto};
use crate::routes::api_error::{InternalError, MutationError, SessionAuth};
use crate::routes::tags::Playback;
use crate::services::playback::PlaybackError;
use crate::state::AppState;

impl From<PlaybackError> for MutationError {
    fn from(err: PlaybackError) -> Self {
        match err {
            PlaybackError::FileNotFound => Self::NotFound(err.to_string()),
            PlaybackError::Db(_) => Self::Internal(err.to_string()),
        }
    }
}

impl From<PlaybackError> for InternalError {
    /// `get_continue_watching` and `get_history` never look up one file, so
    /// `FileNotFound` is unreachable for them -- but it is still mapped rather
    /// than panicked on, because an unreachable arm that lies is worse than one
    /// that is merely unused.
    fn from(err: PlaybackError) -> Self {
        Self::Internal(err.to_string())
    }
}

#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct ReportProgressRequest {
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
}

/// A page of watch history. `total` counts every history row for the user
/// (completed and in-progress) so the client can paginate; `items` may hold
/// fewer than the requested `limit` when a row's underlying file was removed
/// by a rescan (those stale rows are skipped here but still counted in
/// `total`).
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct HistoryResponse {
    pub items: Vec<HistoryItem>,
    pub total: u64,
}

/// Upper bound on `limit` for `GET /v1/history`, mirroring the browse/search
/// pagers: keeps one page (and its per-row media resolution) bounded.
const HISTORY_MAX_LIMIT: u64 = 100;
const HISTORY_DEFAULT_LIMIT: u64 = 50;

/// What `/v1/files/{file_id}/progress` captures.
#[derive(Debug, Schema, PathParams)]
pub struct FilePath {
    /// File id (UUID).
    pub file_id: Uuid,
}

/// How `GET /v1/continue-watching` is bounded.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct ContinueWatchingQuery {
    /// Max items to return (default 20).
    pub limit: Option<u32>,
}

/// How `GET /v1/history` is paged.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct HistoryQuery {
    /// Max items to return (default 50, max 100).
    #[schema(minimum = 1, maximum = 100)]
    pub limit: Option<u64>,
    /// Number of items to skip (default 0).
    pub offset: Option<u64>,
}

/// Resolves the session's user id.
///
/// Kept fallible rather than defaulting: every playback row is keyed by this
/// value, so resolving a malformed id to the nil UUID would pool every affected
/// user's history and progress into one shared, unowned account.
fn parse_user_id(user_id: &str) -> Result<Uuid, InternalError> {
    Uuid::parse_str(user_id)
        .map_err(|_| InternalError::Internal("invalid user id in session".to_owned()))
}

/// Record how far through a file the caller has watched.
#[kynos::put(
    "/files/{file_id}/progress",
    tag = Playback,
    operation_id = "reportPlaybackProgress"
)]
pub async fn report_playback_progress(
    auth: SessionAuth,
    Path(path): Path<FilePath>,
    Inject(state): Inject<AppState>,
    Json(body): Json<ReportProgressRequest>,
) -> Result<Json<PlaybackProgressDto>, MutationError> {
    let user_id = parse_user_id(&auth.0.user_id)?;

    let progress = state
        .services
        .playback
        .report_progress(
            user_id,
            path.file_id,
            body.position_secs,
            body.duration_secs,
        )
        .await?;

    Ok(Json(progress))
}

/// The caller's partially-watched files, most recently updated first.
#[kynos::get(
    "/continue-watching",
    tag = Playback,
    operation_id = "getContinueWatching"
)]
pub async fn get_continue_watching(
    auth: SessionAuth,
    Query(query): Query<ContinueWatchingQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<Vec<ContinueWatchingItem>>, InternalError> {
    let user_id = parse_user_id(&auth.0.user_id)?;

    let items = state
        .services
        .playback
        .get_continue_watching(user_id, query.limit.unwrap_or(20))
        .await?;

    Ok(Json(items))
}

/// Chronological watch history (completed and in-progress), most-recently-
/// updated first.
///
/// `limit` defaults to 50 and is clamped to 1..=100; `offset` defaults to 0.
/// The response carries `total` (all history rows for the user) so a single
/// request paginates without a separate count endpoint. Note that `items.len()`
/// can be below `limit` when stale rows (files removed by a rescan) are
/// skipped, while `total` still counts them.
#[kynos::get("/history", tag = Playback, operation_id = "getHistory")]
pub async fn get_history(
    auth: SessionAuth,
    Query(query): Query<HistoryQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<HistoryResponse>, InternalError> {
    let user_id = parse_user_id(&auth.0.user_id)?;

    let limit = query
        .limit
        .unwrap_or(HISTORY_DEFAULT_LIMIT)
        .clamp(1, HISTORY_MAX_LIMIT);
    let offset = query.offset.unwrap_or(0);

    let (items, total) = state
        .services
        .playback
        .get_history(user_id, limit, offset)
        .await?;

    Ok(Json(HistoryResponse { items, total }))
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod playback_tests;

#[cfg(test)]
mod parse_user_id_tests {
    use super::*;

    #[test]
    fn a_well_formed_session_user_id_parses_to_itself() {
        let id = Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0);
        assert_eq!(parse_user_id(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn a_malformed_user_id_is_an_error_not_the_nil_uuid() {
        // Every playback row is keyed by this value. Defaulting to the nil
        // UUID on a parse failure would pool every affected user's history
        // and progress into one shared, unowned account.
        for malformed in [
            "",
            "not-a-uuid",
            "1234",
            "00000000-0000-0000-0000-00000000000",
        ] {
            let error = parse_user_id(malformed)
                .expect_err("a malformed session user id must not resolve to an account");
            let InternalError::Internal(_) = error;
        }
    }

    #[test]
    fn the_nil_uuid_is_only_produced_when_it_was_actually_asked_for() {
        assert_eq!(
            parse_user_id(&Uuid::nil().to_string()).unwrap(),
            Uuid::nil()
        );
    }
}
