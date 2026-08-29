//! `/v1/files/{file_id}/progress` and `/v1/continue-watching` (FR-507,
//! FR-508). The reporting user is always derived from the session cookie,
//! never from the request body -- one user can never overwrite another's
//! progress.

use salvo::oapi::ToSchema;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::routes::api_error::{ApiError, obtain_state, require_auth};
use crate::services::playback::{
    ContinueWatchingItem, HistoryItem, PlaybackError, PlaybackProgressDto,
};

impl From<PlaybackError> for ApiError {
    fn from(err: PlaybackError) -> Self {
        match err {
            PlaybackError::FileNotFound => ApiError::NotFound(err.to_string()),
            PlaybackError::Db(_) => ApiError::Internal(err.to_string()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ReportProgressRequest {
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
}

/// A page of watch history. `total` counts every history row for the user
/// (completed and in-progress) so the client can paginate; `items` may hold
/// fewer than the requested `limit` when a row's underlying file was removed
/// by a rescan (those stale rows are skipped here but still counted in
/// `total`).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HistoryResponse {
    pub items: Vec<HistoryItem>,
    pub total: u64,
}

/// Upper bound on `limit` for `GET /v1/history`, mirroring the browse/search
/// pagers: keeps one page (and its per-row media resolution) bounded.
const HISTORY_MAX_LIMIT: u64 = 100;
const HISTORY_DEFAULT_LIMIT: u64 = 50;

fn parse_user_id(user_id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(user_id)
        .map_err(|_| ApiError::Internal("invalid user id in session".to_string()))
}

#[endpoint(
    tags("playback"),
    parameters(("file_id" = String, description = "File id (UUID)")),
    request_body = ReportProgressRequest,
)]
pub async fn report_playback_progress(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<PlaybackProgressDto>, ApiError> {
    let state = obtain_state(depot)?;
    let user = require_auth(req, state).await?;
    let user_id = parse_user_id(&user.user_id)?;

    let file_id: String = req.param::<String>("file_id").unwrap_or_default();
    let file_id = Uuid::parse_str(&file_id)
        .map_err(|_| ApiError::BadRequest("invalid file id".to_string()))?;

    let body: ReportProgressRequest = req
        .parse_json()
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    let progress = state
        .services
        .playback
        .report_progress(user_id, file_id, body.position_secs, body.duration_secs)
        .await?;

    Ok(Json(progress))
}

#[endpoint(
    tags("playback"),
    parameters(("limit" = Option<u32>, Query, description = "Max items to return (default 20)")),
)]
pub async fn get_continue_watching(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<ContinueWatchingItem>>, ApiError> {
    let state = obtain_state(depot)?;
    let user = require_auth(req, state).await?;
    let user_id = parse_user_id(&user.user_id)?;

    let limit = req.query::<u32>("limit").unwrap_or(20);
    let items = state
        .services
        .playback
        .get_continue_watching(user_id, limit)
        .await?;
    Ok(Json(items))
}

/// `GET /v1/history?limit=&offset=` — chronological watch history (completed
/// and in-progress), most-recently-updated first. `limit` defaults to 50 and
/// is clamped to 1..=100; `offset` defaults to 0. The response carries `total`
/// (all history rows for the user) so a single request paginates without a
/// separate count endpoint. Note that `items.len()` can be below `limit` when
/// stale rows (files removed by a rescan) are skipped, while `total` still
/// counts them.
#[endpoint(
    tags("playback"),
    parameters(
        ("limit" = Option<u32>, Query, description = "Max items to return (default 50, max 100)"),
        ("offset" = Option<u32>, Query, description = "Number of items to skip (default 0)"),
    ),
)]
pub async fn get_history(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<HistoryResponse>, ApiError> {
    let state = obtain_state(depot)?;
    let user = require_auth(req, state).await?;
    let user_id = parse_user_id(&user.user_id)?;

    let limit = req
        .query::<u64>("limit")
        .unwrap_or(HISTORY_DEFAULT_LIMIT)
        .clamp(1, HISTORY_MAX_LIMIT);
    let offset = req.query::<u64>("offset").unwrap_or(0);

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
            assert!(
                matches!(error, ApiError::Internal(_)),
                "for {malformed:?}: a broken session is a server-side problem, not the caller's"
            );
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
