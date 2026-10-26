//! `/v1/collections` -- provider collections of movies (issue #262), such
//! as a franchise: the listing, by name, and one collection's movies. Follows
//! the `/v1` conventions `media` set out: RFC 9457 problem bodies,
//! cookie-session auth, and an `{items, page_info, total}` connection paged
//! by `first`/`after`.

use kynos::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::{CollectionConnection, CollectionDetail};
use crate::routes::api_error::{CollectionListError, CollectionLookupError, SessionAuth};
use crate::routes::playback::parse_user_id;
use crate::routes::tags::Media;
use crate::services::collections::CollectionError;
use crate::state::AppState;

/// How `GET /v1/collections` is paged.
#[derive(Debug, Default, Serialize, Deserialize, Schema, QueryParams)]
pub struct CollectionQuery {
    /// This many collections (1-100, default 20), after `after` or from the
    /// start.
    #[schema(minimum = 1, maximum = 100)]
    pub first: Option<u32>,
    /// An opaque cursor -- a page's `end_cursor` -- to page on from.
    pub after: Option<String>,
}

/// What `/v1/collections/{id}` captures.
#[derive(Debug, Schema, PathParams)]
pub struct CollectionPath {
    /// Collection id.
    pub id: Uuid,
}

/// The collections the library's movies are in, by name, ignoring case.
///
/// Collections come from provider enrichment: TMDB groups a franchise's
/// movies into one. A collection none of the library's movies is in is not
/// listed.
#[kynos::get("/collections", tag = Media, operation_id = "listCollections")]
pub async fn list_collections(
    _auth: SessionAuth,
    Query(params): Query<CollectionQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<CollectionConnection>, CollectionListError> {
    let CollectionQuery { first, after } = params;
    match state.services.collections.list(first, after).await {
        Ok(connection) => Ok(Json(connection)),
        Err(CollectionError::InvalidCursor(detail)) => {
            Err(CollectionListError::InvalidCursor(detail))
        }
        Err(CollectionError::InvalidPagination(detail)) => {
            Err(CollectionListError::InvalidPagination(detail))
        }
        Err(CollectionError::Internal(detail)) => Err(CollectionListError::Internal(detail)),
    }
}

/// One collection and every movie of the library in it, in release order,
/// with the caller's state for each.
#[kynos::get("/collections/{id}", tag = Media, operation_id = "getCollection")]
pub async fn get_collection(
    auth: SessionAuth,
    Path(path): Path<CollectionPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<CollectionDetail>, CollectionLookupError> {
    let user_id = parse_user_id(&auth.0.user_id)
        .map_err(|err| CollectionLookupError::Internal(err.to_string()))?;
    // A failed read is a 500: answering 404 would tell the client the
    // collection is gone when the server only failed to read it.
    match state.services.collections.detail(user_id, path.id).await {
        Ok(Some(detail)) => Ok(Json(detail)),
        Ok(None) => Err(CollectionLookupError::CollectionNotFound(format!(
            "collection {} not found",
            path.id
        ))),
        Err(err) => Err(CollectionLookupError::Internal(err.to_string())),
    }
}

#[cfg(test)]
#[path = "collections_tests.rs"]
mod collections_tests;
