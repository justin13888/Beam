//! `/v1/files/{file_id}/progress` and `/v1/continue-watching` (FR-507,
//! FR-508). The reporting user is always derived from the Bearer JWT, never
//! from the request body -- one user can never overwrite another's progress.

use salvo::oapi::ToSchema;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::routes::api_error::{ApiError, require_auth};
use crate::services::playback::{ContinueWatchingItem, PlaybackError, PlaybackProgressDto};
use crate::state::AppState;

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

fn parse_user_id(user_id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(user_id)
        .map_err(|_| ApiError::Internal("invalid user id in session".to_string()))
}

#[endpoint(
    tags("playback"),
    parameters(
        ("file_id" = String, description = "File id (UUID)"),
        ("Authorization" = String, Header, description = "Bearer <user JWT>"),
    ),
    request_body = ReportProgressRequest,
)]
pub async fn report_playback_progress(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<PlaybackProgressDto>, ApiError> {
    let state = depot.obtain::<AppState>().unwrap();
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
    parameters(
        ("limit" = Option<u32>, Query, description = "Max items to return (default 20)"),
        ("Authorization" = String, Header, description = "Bearer <user JWT>"),
    ),
)]
pub async fn get_continue_watching(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<ContinueWatchingItem>>, ApiError> {
    let state = depot.obtain::<AppState>().unwrap();
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

#[cfg(test)]
#[path = "playback_tests.rs"]
mod playback_tests;
