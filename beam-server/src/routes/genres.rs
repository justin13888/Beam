//! `/v1/genres` -- the distinct genre catalog used to populate the Explore
//! page's genre filter. Follows the shared `/v1` conventions from
//! `api_error` (RFC 9457 problem bodies, cookie-session auth).

use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::routes::api_error::{InternalError, SessionAuth};
use crate::routes::tags::Media;
use crate::state::AppState;

/// The distinct genre names present in the library, sorted alphabetically.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct GenreListResponse {
    /// Genre display names, sorted alphabetically (case-insensitive).
    pub genres: Vec<String>,
}

/// List the distinct genres in the library, sorted for stable presentation.
///
/// Taking `SessionAuth` is what puts `BeamSession` in this operation's
/// `security` and its 401 in `responses`; the previous `require_auth(req,
/// state)` call did the same work where no describer could see it.
#[kynos::get("/genres", tag = Media, operation_id = "listGenres")]
pub async fn list_genres(
    _auth: SessionAuth,
    Inject(state): Inject<AppState>,
) -> Result<Json<GenreListResponse>, InternalError> {
    let mut genres: Vec<String> = state
        .services
        .genre_repo
        .find_all()
        .await
        .map_err(|e| InternalError::Internal(e.to_string()))?
        .into_iter()
        .map(|genre| genre.name)
        .collect();
    genres.sort_by_key(|name| name.to_lowercase());

    Ok(Json(GenreListResponse { genres }))
}

#[cfg(test)]
#[path = "genres_tests.rs"]
mod genres_tests;
