//! `/v1/media` -- browse, detail, and sources endpoints. Domain REST API
//! conventions established here (uniform JSON error body, cookie-based
//! `CurrentUser` extraction, cursor pagination reused from the existing
//! Relay-style `search_media`) are meant to be followed by every subsequent
//! `/v1` route.

use salvo::prelude::*;

use crate::models::{MediaMetadata, MediaSource};
use crate::routes::api_error::{ApiError, obtain_state, require_auth};
use crate::services::metadata::{
    MediaSearchFilters, MediaSortField, MediaTypeFilter, MetadataError, SortOrder,
};

/// Browse/search the media library with cursor-based pagination, sorting, and
/// filtering.
#[endpoint(
    tags("media"),
    parameters(
        ("first" = Option<u32>, Query, description = "Number of items to return from the start"),
        ("after" = Option<String>, Query, description = "Cursor to start after"),
        ("last" = Option<u32>, Query, description = "Number of items to return from the end"),
        ("before" = Option<String>, Query, description = "Cursor to start before"),
        ("sort_by" = Option<MediaSortField>, Query, description = "Sort field"),
        ("sort_order" = Option<SortOrder>, Query, description = "Sort order"),
        ("media_type" = Option<MediaTypeFilter>, Query, description = "Filter by media type"),
        ("genre" = Option<String>, Query, description = "Filter by genre"),
        ("year" = Option<u32>, Query, description = "Filter by year (exact match)"),
        ("year_from" = Option<u32>, Query, description = "Filter by year range (start)"),
        ("year_to" = Option<u32>, Query, description = "Filter by year range (end)"),
        ("query" = Option<String>, Query, description = "Search query for title"),
        ("min_rating" = Option<u32>, Query, description = "Filter by minimum rating (0-100)"),
    ),
)]
pub async fn browse_media(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<crate::services::metadata::MediaConnection>, ApiError> {
    let state = obtain_state(depot)?;
    require_auth(req, state).await?;

    let filters = MediaSearchFilters {
        media_type: req.query::<MediaTypeFilter>("media_type"),
        genre: req.query::<String>("genre"),
        year: req.query::<u32>("year"),
        year_from: req.query::<u32>("year_from"),
        year_to: req.query::<u32>("year_to"),
        query: req.query::<String>("query"),
        min_rating: req.query::<u32>("min_rating"),
    };

    let result = state
        .services
        .metadata
        .search_media(
            req.query::<u32>("first"),
            req.query::<String>("after"),
            req.query::<u32>("last"),
            req.query::<String>("before"),
            req.query::<MediaSortField>("sort_by").unwrap_or_default(),
            req.query::<SortOrder>("sort_order").unwrap_or_default(),
            filters,
        )
        .await;

    Ok(Json(result))
}

/// Fetch a single media item's full metadata by id.
#[endpoint(
    tags("media"),
    parameters(("id" = String, description = "Media id (movie or show UUID)")),
)]
pub async fn get_media_detail(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<MediaMetadata>, ApiError> {
    let state = obtain_state(depot)?;
    require_auth(req, state).await?;

    let id: String = req.param::<String>("id").unwrap_or_default();
    match state.services.metadata.get_media_metadata(&id).await {
        Some(metadata) => Ok(Json(metadata)),
        None => Err(ApiError::NotFound(format!("media {id} not found"))),
    }
}

/// List the playable/downloadable source files for a playable media id.
///
/// Accepts a movie id or an episode id (both are "playable" ids). A show id is
/// rejected with 400 -- shows have no files of their own, so callers request
/// sources for the show's individual episode ids instead. An episode with no
/// files yet returns an empty array (a valid, "not yet playable" response).
#[endpoint(
    tags("media"),
    parameters(("id" = String, description = "Playable media id: a movie UUID or an episode UUID")),
)]
pub async fn get_media_sources(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<MediaSource>>, ApiError> {
    let state = obtain_state(depot)?;
    require_auth(req, state).await?;

    let id: String = req.param::<String>("id").unwrap_or_default();
    match state.services.metadata.get_media_sources(&id).await {
        Ok(sources) => Ok(Json(sources)),
        Err(MetadataError::MediaNotFound) => {
            Err(ApiError::NotFound(format!("media {id} not found")))
        }
        Err(MetadataError::Unsupported(msg)) => Err(ApiError::BadRequest(msg)),
        Err(MetadataError::InternalError(msg)) => Err(ApiError::Internal(msg)),
    }
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod media_tests;
