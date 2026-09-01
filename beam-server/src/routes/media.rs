//! `/v1/media` -- browse, detail, and sources endpoints. Domain REST API
//! conventions established here (RFC 9457 problem bodies, cookie-session auth
//! via `SessionAuth`, cursor pagination reused from the existing Relay-style
//! `search_media`) are meant to be followed by every subsequent `/v1` route.

use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::models::search::{MediaConnection, MediaSortField, MediaTypeFilter, SortOrder};
use crate::models::{MediaMetadata, MediaSource};
use crate::routes::api_error::{LookupError, MutationError, SessionAuth};
use crate::routes::tags::Media;
use crate::services::metadata::{MediaSearchFilters, MetadataError};
use crate::state::AppState;

/// Everything `GET /v1/media` accepts.
///
/// A struct rather than thirteen `parameters(...)` entries beside thirteen
/// `req.query::<T>(name)` calls. The pair used to be maintained by hand, so a
/// renamed parameter changed one and not the other; here the field *is* the
/// parameter and the type *is* the schema.
#[derive(Debug, Default, Serialize, Deserialize, Schema, QueryParams)]
pub struct BrowseQuery {
    /// Number of items to return from the start.
    pub first: Option<u32>,
    /// Cursor to start after.
    pub after: Option<String>,
    /// Number of items to return from the end.
    pub last: Option<u32>,
    /// Cursor to start before.
    pub before: Option<String>,
    /// Sort field.
    pub sort_by: Option<MediaSortField>,
    /// Sort order.
    pub sort_order: Option<SortOrder>,
    /// Filter by media type.
    pub media_type: Option<MediaTypeFilter>,
    /// Filter by genre.
    pub genre: Option<String>,
    /// Filter by year (exact match).
    pub year: Option<u32>,
    /// Filter by year range (start).
    pub year_from: Option<u32>,
    /// Filter by year range (end).
    pub year_to: Option<u32>,
    /// Search query for title.
    pub query: Option<String>,
    /// Filter by minimum rating (0-100).
    #[schema(maximum = 100)]
    pub min_rating: Option<u32>,
}

/// What `/v1/media/{id}` and `/v1/media/{id}/sources` capture.
#[derive(Debug, Schema, PathParams)]
pub struct MediaPath {
    /// Media id (movie or show UUID).
    pub id: String,
}

/// Browse/search the media library with cursor-based pagination, sorting, and
/// filtering.
#[kynos::get("/media", tag = Media, operation_id = "browseMedia")]
pub async fn browse_media(
    _auth: SessionAuth,
    Query(params): Query<BrowseQuery>,
    Inject(state): Inject<AppState>,
) -> Json<MediaConnection> {
    let BrowseQuery {
        first,
        after,
        last,
        before,
        sort_by,
        sort_order,
        media_type,
        genre,
        year,
        year_from,
        year_to,
        query,
        min_rating,
    } = params;

    let filters = MediaSearchFilters {
        media_type,
        genre,
        year,
        year_from,
        year_to,
        query,
        min_rating,
    };

    let result = state
        .services
        .metadata
        .search_media(
            first,
            after,
            last,
            before,
            sort_by.unwrap_or_default(),
            sort_order.unwrap_or_default(),
            filters,
        )
        .await;

    Json(result)
}

/// Fetch a single media item's full metadata by id.
#[kynos::get("/media/{id}", tag = Media, operation_id = "getMediaDetail")]
pub async fn get_media_detail(
    _auth: SessionAuth,
    Path(path): Path<MediaPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<MediaMetadata>, LookupError> {
    match state.services.metadata.get_media_metadata(&path.id).await {
        Some(metadata) => Ok(Json(metadata)),
        None => Err(LookupError::NotFound(format!(
            "media {} not found",
            path.id
        ))),
    }
}

/// List the playable/downloadable source files for a playable media id.
///
/// Accepts a movie id or an episode id (both are "playable" ids). A show id is
/// rejected with 400 -- shows have no files of their own, so callers request
/// sources for the show's individual episode ids instead. An episode with no
/// files yet returns an empty array (a valid, "not yet playable" response).
#[kynos::get("/media/{id}/sources", tag = Media, operation_id = "getMediaSources")]
pub async fn get_media_sources(
    _auth: SessionAuth,
    Path(path): Path<MediaPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<Vec<MediaSource>>, MutationError> {
    match state.services.metadata.get_media_sources(&path.id).await {
        Ok(sources) => Ok(Json(sources)),
        Err(MetadataError::MediaNotFound) => Err(MutationError::NotFound(format!(
            "media {} not found",
            path.id
        ))),
        Err(MetadataError::Unsupported(msg)) => Err(MutationError::BadRequest(msg)),
        Err(MetadataError::InternalError(msg)) => Err(MutationError::Internal(msg)),
    }
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod media_tests;
