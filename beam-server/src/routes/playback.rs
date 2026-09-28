//! Watched state (FR-507, FR-508, FR-510, FR-511, FR-701; issue #188):
//! `/v1/files/{file_id}/progress`, `/v1/media/{id}/progress`,
//! `/v1/media/{id}/watched`, `/v1/continue-watching` and `/v1/history`. The
//! user is always the session's, never the request body's -- one user can
//! never read or overwrite another's state.

use std::num::NonZeroU32;

use kynos::prelude::*;
use kynos::response::status::NoContent;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::playback::{
    ContinueWatchingConnection, HistoryConnection, ReportProgressRequest, UserTitleState,
};
use crate::models::playback_telemetry::PlaybackTelemetryBatch;
use crate::models::{FieldError, PageInfo};
use crate::routes::api_error::{
    ContinueWatchingError, HistoryError, InternalError, MediaRefLookupError,
    PlaybackTelemetryError, ProgressError, SessionAuth, TitleProgressError,
};
use crate::routes::tags::Playback;
use crate::services::history_cursor;
use crate::services::playback::{
    ContinueWatchingPage, PlaybackError, PlaybackReadError, TitleStateError,
};
use crate::services::playback_telemetry::IngestError;
use crate::state::AppState;

impl From<PlaybackError> for ProgressError {
    fn from(err: PlaybackError) -> Self {
        match err {
            PlaybackError::FileNotFound(detail) => Self::FileNotFound(detail),
            PlaybackError::Invalid(faults) => Self::ValidationFailed {
                errors: faults
                    .into_iter()
                    .map(|fault| FieldError {
                        pointer: fault.pointer.to_string(),
                        detail: fault.detail,
                    })
                    .collect(),
            },
            PlaybackError::Db(_) => Self::Internal(err.to_string()),
        }
    }
}

impl From<TitleStateError> for TitleProgressError {
    fn from(err: TitleStateError) -> Self {
        match err {
            TitleStateError::NotFound(_) => Self::MediaNotFound(err.to_string()),
            TitleStateError::NotPlayable(_) => Self::ProgressNotAvailableForShow(err.to_string()),
            TitleStateError::Db(_) => Self::Internal(err.to_string()),
        }
    }
}

impl From<TitleStateError> for MediaRefLookupError {
    /// Marks and dismissals accept every kind of id, so `NotPlayable` never
    /// reaches here; were it to, the id named a title, so it is no 404.
    fn from(err: TitleStateError) -> Self {
        match err {
            TitleStateError::NotFound(_) => Self::MediaNotFound(err.to_string()),
            TitleStateError::NotPlayable(_) | TitleStateError::Db(_) => {
                Self::Internal(err.to_string())
            }
        }
    }
}

impl From<PlaybackReadError> for InternalError {
    fn from(err: PlaybackReadError) -> Self {
        let PlaybackReadError::Db(_) = &err;
        Self::Internal(err.to_string())
    }
}

impl From<PlaybackReadError> for ContinueWatchingError {
    fn from(err: PlaybackReadError) -> Self {
        Self::Internal(err.to_string())
    }
}

impl From<PlaybackReadError> for HistoryError {
    fn from(err: PlaybackReadError) -> Self {
        Self::Internal(err.to_string())
    }
}

impl From<InternalError> for ContinueWatchingError {
    fn from(err: InternalError) -> Self {
        let InternalError::Internal(detail) = err;
        Self::Internal(detail)
    }
}

impl From<InternalError> for HistoryError {
    fn from(err: InternalError) -> Self {
        let InternalError::Internal(detail) = err;
        Self::Internal(detail)
    }
}

impl From<InternalError> for TitleProgressError {
    fn from(err: InternalError) -> Self {
        let InternalError::Internal(detail) = err;
        Self::Internal(detail)
    }
}

impl From<InternalError> for MediaRefLookupError {
    fn from(err: InternalError) -> Self {
        let InternalError::Internal(detail) = err;
        Self::Internal(detail)
    }
}

/// The largest continue-watching shelf, and the one a request naming none
/// gets.
const CONTINUE_WATCHING_MAX: u32 = 50;
const CONTINUE_WATCHING_DEFAULT: u32 = 20;
/// The largest history page, and the one a request naming none gets.
const HISTORY_MAX: u32 = 100;
const HISTORY_DEFAULT: u32 = 50;

/// What `/v1/files/{file_id}/progress` captures.
#[derive(Debug, Schema, PathParams)]
pub struct FilePath {
    /// File id (UUID).
    pub file_id: Uuid,
}

/// What `/v1/media/{id}/progress`, `/v1/media/{id}/watched` and
/// `/v1/continue-watching/{id}` capture.
#[derive(Debug, Schema, PathParams)]
pub struct TitlePath {
    /// A movie, show, season or episode id.
    pub id: Uuid,
}

/// How `GET /v1/continue-watching` is bounded.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct ContinueWatchingQuery {
    /// At most this many titles (1-50, default 20).
    //
    // Bounds not documented while kynos drops `#[schema]` on an `Option`
    // query field; tracked with #223, as for `BrowseQuery::first`.
    #[schema(minimum = 1, maximum = 50)]
    pub first: Option<u32>,
}

/// How `GET /v1/history` is paged.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct HistoryQuery {
    /// This many titles (1-100, default 50).
    #[schema(minimum = 1, maximum = 100)]
    pub first: Option<u32>,
    /// An opaque cursor -- a page's `end_cursor` -- to page on from.
    pub after: Option<String>,
}

/// Resolves the session's user id.
///
/// Kept fallible rather than defaulting: every watched-state row is keyed by
/// this value, so resolving a malformed id to the nil UUID would pool every
/// affected user's history and progress into one shared, unowned account.
pub(crate) fn parse_user_id(user_id: &str) -> Result<Uuid, InternalError> {
    Uuid::parse_str(user_id)
        .map_err(|_| InternalError::Internal("invalid user id in session".to_owned()))
}

/// `first`, defaulted and held to `1..=max`.
fn page_size(first: Option<u32>, default: u32, max: u32) -> Result<NonZeroU32, String> {
    let size = first.unwrap_or(default);
    NonZeroU32::new(size)
        .filter(|size| size.get() <= max)
        .ok_or_else(|| format!("a page holds 1 to {max} titles, not {size}"))
}

/// Record how far through a file the caller has watched.
///
/// The position is kept for the file's movie or episode, not the file, so
/// any source of the title resumes from it. Reaching 95% of the duration
/// marks the title played and returns the position to the start; rewinding
/// afterwards never unplays it. A position past the end by more than 2
/// seconds or 1% -- whichever is larger -- is refused; one within that is
/// taken as the end.
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
) -> Result<Json<UserTitleState>, ProgressError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    let ReportProgressRequest {
        position_secs,
        duration_secs,
    } = body;

    let progress = state
        .services
        .playback
        .report_progress(user_id, path.file_id, position_secs, duration_secs)
        .await?;

    Ok(Json(progress))
}

/// The caller's state for a movie or an episode: where to resume, and
/// whether it is played. A title never played answers all zeros.
#[kynos::get(
    "/media/{id}/progress",
    tag = Playback,
    operation_id = "getTitleProgress"
)]
pub async fn get_title_progress(
    auth: SessionAuth,
    Path(path): Path<TitlePath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<UserTitleState>, TitleProgressError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    let progress = state
        .services
        .playback
        .get_title_progress(user_id, path.id)
        .await?;
    Ok(Json(progress))
}

/// Forget where the caller stopped in a movie or an episode. A title the
/// caller has played stays played; one never finished is forgotten.
#[kynos::delete(
    "/media/{id}/progress",
    tag = Playback,
    operation_id = "clearTitleProgress"
)]
pub async fn clear_title_progress(
    auth: SessionAuth,
    Path(path): Path<TitlePath>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, TitleProgressError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    state
        .services
        .playback
        .clear_progress(user_id, path.id)
        .await?;
    Ok(NoContent)
}

/// Mark a movie, an episode, a season or a whole show watched.
///
/// A season or a show marks each of its episodes; an episode added later
/// is unwatched. Marking a title already watched changes nothing.
#[kynos::put(
    "/media/{id}/watched",
    tag = Playback,
    operation_id = "markWatched"
)]
pub async fn mark_watched(
    auth: SessionAuth,
    Path(path): Path<TitlePath>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, MediaRefLookupError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    state
        .services
        .playback
        .set_watched(user_id, path.id, true)
        .await?;
    Ok(NoContent)
}

/// Mark a movie, an episode, a season or a whole show unwatched: its played
/// state, play count and resume position are all forgotten.
#[kynos::delete(
    "/media/{id}/watched",
    tag = Playback,
    operation_id = "markUnwatched"
)]
pub async fn mark_unwatched(
    auth: SessionAuth,
    Path(path): Path<TitlePath>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, MediaRefLookupError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    state
        .services
        .playback
        .set_watched(user_id, path.id, false)
        .await?;
    Ok(NoContent)
}

/// What the caller is part-way through: one row per movie to resume, and
/// one per show -- the episode to resume, or the next one to start --
/// most recently played first.
///
/// A show the caller has caught up on is not listed, nor a title removed
/// with `DELETE /v1/continue-watching/{id}` until it is played again.
#[kynos::get(
    "/continue-watching",
    tag = Playback,
    operation_id = "getContinueWatching"
)]
pub async fn get_continue_watching(
    auth: SessionAuth,
    Query(query): Query<ContinueWatchingQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<ContinueWatchingConnection>, ContinueWatchingError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    let ContinueWatchingQuery { first } = query;
    let first = page_size(first, CONTINUE_WATCHING_DEFAULT, CONTINUE_WATCHING_MAX)
        .map_err(ContinueWatchingError::InvalidPagination)?;

    let ContinueWatchingPage {
        items,
        has_next_page,
    } = state
        .services
        .playback
        .get_continue_watching(user_id, first)
        .await?;

    Ok(Json(ContinueWatchingConnection {
        items,
        page_info: PageInfo {
            has_next_page,
            has_previous_page: false,
            start_cursor: None,
            end_cursor: None,
        },
    }))
}

/// Remove a title from the caller's continue-watching until they next play
/// it. An episode or a season id removes its show. Its progress is kept.
#[kynos::delete(
    "/continue-watching/{id}",
    tag = Playback,
    operation_id = "dismissContinueWatching"
)]
pub async fn dismiss_continue_watching(
    auth: SessionAuth,
    Path(path): Path<TitlePath>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, MediaRefLookupError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    state.services.playback.dismiss(user_id, path.id).await?;
    Ok(NoContent)
}

/// Every movie and episode the caller has played or marked watched, most
/// recently first, a page at a time.
///
/// `total` counts every title in the history; a title whose files are all
/// gone is counted but not listed, so a page can hold fewer than `first`.
#[kynos::get("/history", tag = Playback, operation_id = "getHistory")]
pub async fn get_history(
    auth: SessionAuth,
    Query(query): Query<HistoryQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<HistoryConnection>, HistoryError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    let HistoryQuery { first, after } = query;
    let first =
        page_size(first, HISTORY_DEFAULT, HISTORY_MAX).map_err(HistoryError::InvalidPagination)?;
    let after = after
        .as_deref()
        .map(history_cursor::decode)
        .transpose()
        .map_err(|err| HistoryError::InvalidCursor(err.to_string()))?;
    let has_previous_page = after.is_some();

    let page = state
        .services
        .playback
        .get_history(user_id, after, first)
        .await?;

    Ok(Json(HistoryConnection {
        items: page.items,
        page_info: PageInfo {
            has_next_page: page.has_next_page,
            has_previous_page,
            start_cursor: page.start.as_ref().map(history_cursor::encode),
            end_cursor: page.end.as_ref().map(history_cursor::encode),
        },
        total: page.total,
    }))
}

impl From<IngestError> for PlaybackTelemetryError {
    fn from(err: IngestError) -> Self {
        match err {
            IngestError::Disabled => Self::Disabled(err.to_string()),
            IngestError::Invalid(errors) => Self::ValidationFailed { errors },
            IngestError::Db(_) => Self::Internal(err.to_string()),
        }
    }
}

/// Report playback starts, failures, rebuffers and source switches.
///
/// Each event is counted per UTC day under coarse dimensions of the file it
/// names -- container, codecs, resolution and bitrate class -- after which
/// the file id is discarded; who reported is never recorded. Answers 409
/// when the operator has not enabled playback telemetry: stop reporting.
#[kynos::post(
    "/telemetry/playback",
    tag = Playback,
    operation_id = "reportPlaybackTelemetry"
)]
pub async fn report_playback_telemetry(
    // Authenticates, and nothing more: the session's identity is deliberately
    // not passed on, so no count can be tied to a user (ADR-0019).
    _auth: SessionAuth,
    Inject(state): Inject<AppState>,
    Json(body): Json<PlaybackTelemetryBatch>,
) -> Result<NoContent, PlaybackTelemetryError> {
    state.services.playback_telemetry.ingest(body).await?;
    Ok(NoContent)
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod playback_tests;

#[cfg(test)]
#[path = "playback_telemetry_tests.rs"]
mod playback_telemetry_tests;

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
        // Every watched-state row is keyed by this value. Defaulting to the nil
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
