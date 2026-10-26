//! `/v1/genres` -- the distinct genre catalog used to populate the Explore
//! page's genre filter. Follows the shared `/v1` conventions from
//! `api_error` (RFC 9457 problem bodies, cookie-session auth).

use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::models::PageInfo;
use crate::routes::api_error::{InternalError, SessionAuth};
use crate::routes::playback::caller_access;
use crate::routes::tags::Media;
use crate::state::AppState;

/// The distinct genre names present in the library, sorted alphabetically.
///
/// A connection, as every `/v1` list is, though a catalog's genres are few
/// enough that it is never paged: `items` is the whole list, both of
/// `page_info`'s `has_*_page` flags are `false`, and it carries no cursors.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct GenreConnection {
    /// Genre display names, sorted alphabetically (case-insensitive).
    pub items: Vec<String>,
    /// Always a single, complete page.
    pub page_info: PageInfo,
}

/// List the distinct genres in the library, sorted for stable presentation.
///
/// Only the genres of titles the caller may see are listed: one carried only
/// by titles their access policy hides, or by no title any more, is not.
///
/// Taking `SessionAuth` is what puts `BeamSession` in this operation's
/// `security` and its 401 in `responses`; the previous `require_auth(req,
/// state)` call did the same work where no describer could see it.
#[kynos::get("/genres", tag = Media, operation_id = "listGenres")]
pub async fn list_genres(
    auth: SessionAuth,
    Inject(state): Inject<AppState>,
) -> Result<Json<GenreConnection>, InternalError> {
    let (_, policy) = caller_access(&state, &auth).await?;
    let mut genres: Vec<String> = state
        .services
        .genre_repo
        .find_carried(&policy)
        .await
        .map_err(|e| InternalError::Internal(e.to_string()))?
        .into_iter()
        .map(|genre| genre.name)
        .collect();
    genres.sort_by_key(|name| name.to_lowercase());

    Ok(Json(GenreConnection {
        items: genres,
        page_info: PageInfo::complete(),
    }))
}

#[cfg(test)]
#[path = "genres_tests.rs"]
mod genres_tests;
