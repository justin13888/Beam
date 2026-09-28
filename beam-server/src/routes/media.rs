//! `/v1/media` -- browse, detail, and sources endpoints. Domain REST API
//! conventions established here (RFC 9457 problem bodies, cookie-session auth
//! via `SessionAuth`, cursor pagination over an `{items, page_info}`
//! connection) are meant to be followed by every subsequent `/v1` route.

use kynos::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::search::{MediaConnection, MediaSortField, MediaTypeFilter, SortOrder};
use crate::models::{EpisodeDetail, MediaMetadata, MediaSourceConnection, SeasonDetail};
use crate::routes::api_error::{
    MediaBrowseError, MediaLookupError, MediaRefLookupError, MediaSourcesError, SessionAuth,
};
use crate::routes::playback::parse_user_id;
use crate::routes::tags::Media;
use crate::services::metadata::{BrowseRequest, MediaSearchFilters, MetadataError};
use crate::state::AppState;

/// Everything `GET /v1/media` accepts.
///
/// A struct rather than thirteen `parameters(...)` entries beside thirteen
/// `req.query::<T>(name)` calls. The pair used to be maintained by hand, so a
/// renamed parameter changed one and not the other; here the field *is* the
/// parameter and the type *is* the schema.
#[derive(Debug, Default, Serialize, Deserialize, Schema, QueryParams)]
pub struct BrowseQuery {
    /// Page forwards: this many items (1-100, default 20), after `after` or
    /// from the start. Not combined with `last` or `before`.
    //
    // Kynos 0.3.0 neither documents nor enforces `#[schema]` bounds on an
    // `Option` field of a `QueryParams` struct (`HistoryQuery::limit` shows
    // the same) -- tracked with #223, the local issue for this class of
    // Kynos constraint gap; upstream issue pending. The service enforces 1-100
    // either way -- `#invalid-pagination` -- and the prose states it; the
    // bounds below reach the document once Kynos honours them.
    #[schema(minimum = 1, maximum = 100)]
    pub first: Option<u32>,
    /// An opaque cursor -- a page's `end_cursor` -- to page forwards from.
    pub after: Option<String>,
    /// Page backwards: this many items (1-100), before `before` or from the
    /// end. Not combined with `first` or `after`.
    #[schema(minimum = 1, maximum = 100)]
    pub last: Option<u32>,
    /// An opaque cursor -- a page's `start_cursor` -- to page backwards from.
    pub before: Option<String>,
    /// Sort field (default `title`). Titles with no value for the field sort
    /// last in either order; ties fall back to a stable per-title order.
    pub sort_by: Option<MediaSortField>,
    /// Sort order (default `asc`).
    pub sort_order: Option<SortOrder>,
    /// Filter by media type.
    pub media_type: Option<MediaTypeFilter>,
    /// Filter by genre, by name or slug: `Science Fiction` and
    /// `science-fiction` name the same genre.
    pub genre: Option<String>,
    /// Filter by year (exact match).
    pub year: Option<u32>,
    /// Filter by year range (start).
    pub year_from: Option<u32>,
    /// Filter by year range (end).
    pub year_to: Option<u32>,
    /// Search the titles: those resembling this text or containing it,
    /// ignoring case. Results keep the requested sort.
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

/// What `/v1/episodes/{id}` and `/v1/seasons/{id}` capture.
#[derive(Debug, Schema, PathParams)]
pub struct ChildPath {
    /// Episode or season id.
    pub id: Uuid,
}

/// Browse/search the media library with cursor-based pagination, sorting, and
/// filtering.
///
/// Only titles with at least one present file are listed. Each movie carries
/// the caller's state for it; a show carries none here.
#[kynos::get("/media", tag = Media, operation_id = "browseMedia")]
pub async fn browse_media(
    auth: SessionAuth,
    Query(params): Query<BrowseQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<MediaConnection>, MediaBrowseError> {
    let user_id = parse_user_id(&auth.0.user_id)
        .map_err(|err| MediaBrowseError::Internal(err.to_string()))?;
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
        .search_media(BrowseRequest {
            first,
            after,
            last,
            before,
            sort_by: sort_by.unwrap_or_default(),
            sort_order: sort_order.unwrap_or_default(),
            filters,
        })
        .await;

    match result {
        Ok(mut connection) => {
            state
                .services
                .playback
                .overlay_titles(user_id, &mut connection.items)
                .await
                .map_err(|err| MediaBrowseError::Internal(err.to_string()))?;
            Ok(Json(connection))
        }
        Err(MetadataError::InvalidCursor(detail)) => Err(MediaBrowseError::InvalidCursor(detail)),
        Err(MetadataError::InvalidPagination(detail)) => {
            Err(MediaBrowseError::InvalidPagination(detail))
        }
        Err(MetadataError::InvalidSearchQuery(detail)) => {
            Err(MediaBrowseError::InvalidSearchQuery(detail))
        }
        Err(
            err @ (MetadataError::InternalError(_)
            | MetadataError::InvalidId
            | MetadataError::MediaNotFound
            | MetadataError::Unsupported(_)),
        ) => Err(MediaBrowseError::Internal(err.to_string())),
    }
}

/// Fetch a single media item's full metadata by id, with the caller's state
/// for the movie, or for the show and each of its seasons and episodes.
#[kynos::get("/media/{id}", tag = Media, operation_id = "getMediaDetail")]
pub async fn get_media_detail(
    auth: SessionAuth,
    Path(path): Path<MediaPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<MediaMetadata>, MediaLookupError> {
    // Parsed here, so a malformed id is the 400 `/sources` and the refresh
    // route answer for the same path parameter, not a lookup miss.
    let Ok(id) = Uuid::parse_str(&path.id) else {
        return Err(MediaLookupError::InvalidMediaId(format!(
            "{} is not a valid media id",
            path.id
        )));
    };

    let user_id = parse_user_id(&auth.0.user_id)?;

    // A failed lookup is a 500: answering 404 would tell the client the
    // title is gone when the server only failed to read it.
    match state.services.metadata.get_media_metadata(id).await {
        Ok(Some(mut metadata)) => {
            state
                .services
                .playback
                .overlay_media(user_id, &mut metadata)
                .await
                .map_err(|err| MediaLookupError::Internal(err.to_string()))?;
            Ok(Json(metadata))
        }
        Ok(None) => Err(MediaLookupError::MediaNotFound(format!(
            "media {} not found",
            path.id
        ))),
        Err(err) => Err(MediaLookupError::Internal(err.to_string())),
    }
}

/// List the playable/downloadable source files for a playable media id.
///
/// Accepts a movie id or an episode id (both are "playable" ids). A show id is
/// rejected with 400 -- shows have no files of their own, so callers request
/// sources for the show's individual episode ids instead. An episode with no
/// files yet returns an empty list (a valid, "not yet playable" response).
/// The list is a connection, but never paged: `items` is
/// every source, primary first.
#[kynos::get("/media/{id}/sources", tag = Media, operation_id = "getMediaSources")]
pub async fn get_media_sources(
    _auth: SessionAuth,
    Path(path): Path<MediaPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<MediaSourceConnection>, MediaSourcesError> {
    match state.services.metadata.get_media_sources(&path.id).await {
        Ok(sources) => Ok(Json(MediaSourceConnection::complete(sources))),
        Err(MetadataError::InvalidId) => Err(MediaSourcesError::InvalidMediaId(format!(
            "media id {} is not a valid identifier",
            path.id
        ))),
        Err(MetadataError::MediaNotFound) => Err(MediaSourcesError::MediaNotFound(format!(
            "media {} not found",
            path.id
        ))),
        Err(MetadataError::Unsupported(msg)) => {
            Err(MediaSourcesError::SourcesNotAvailableForShow(msg))
        }
        // Sources are neither paged nor searched: a cursor, page or search
        // error cannot arise here.
        Err(
            MetadataError::InternalError(msg)
            | MetadataError::InvalidCursor(msg)
            | MetadataError::InvalidPagination(msg)
            | MetadataError::InvalidSearchQuery(msg),
        ) => Err(MediaSourcesError::Internal(msg)),
    }
}

/// One episode: its metadata and the caller's state for it, its season, its
/// show, and the episodes either side of it in the show.
#[kynos::get("/episodes/{id}", tag = Media, operation_id = "getEpisodeDetail")]
pub async fn get_episode_detail(
    auth: SessionAuth,
    Path(path): Path<ChildPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<EpisodeDetail>, MediaRefLookupError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    let mut detail = state
        .services
        .metadata
        .get_episode_detail(path.id)
        .await
        .map_err(|err| MediaRefLookupError::Internal(err.to_string()))?
        .ok_or_else(|| {
            MediaRefLookupError::MediaNotFound(format!("episode {} not found", path.id))
        })?;
    state
        .services
        .playback
        .overlay_episode(user_id, &mut detail)
        .await
        .map_err(|err| MediaRefLookupError::Internal(err.to_string()))?;
    Ok(Json(detail))
}

/// One season: its episodes with the caller's state for each and for the
/// season, and its show.
#[kynos::get("/seasons/{id}", tag = Media, operation_id = "getSeasonDetail")]
pub async fn get_season_detail(
    auth: SessionAuth,
    Path(path): Path<ChildPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<SeasonDetail>, MediaRefLookupError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    let mut detail = state
        .services
        .metadata
        .get_season_detail(path.id)
        .await
        .map_err(|err| MediaRefLookupError::Internal(err.to_string()))?
        .ok_or_else(|| {
            MediaRefLookupError::MediaNotFound(format!("season {} not found", path.id))
        })?;
    state
        .services
        .playback
        .overlay_season(user_id, &mut detail)
        .await
        .map_err(|err| MediaRefLookupError::Internal(err.to_string()))?;
    Ok(Json(detail))
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod media_tests;
