//! `/v1/genres` -- the distinct genre catalog used to populate the Explore
//! page's genre filter. Follows the shared `/v1` conventions from
//! `api_error` (uniform JSON error body, cookie-session auth).

use salvo::oapi::ToSchema;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};

use crate::routes::api_error::{ApiError, obtain_state, require_auth};

/// The distinct genre names present in the library, sorted alphabetically.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GenreListResponse {
    /// Genre display names, sorted alphabetically (case-insensitive).
    pub genres: Vec<String>,
}

/// List the distinct genres in the library, sorted for stable presentation.
#[endpoint(tags("media"))]
pub async fn list_genres(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<GenreListResponse>, ApiError> {
    let state = obtain_state(depot)?;
    require_auth(req, state).await?;

    let mut genres: Vec<String> = state
        .services
        .genre_repo
        .find_all()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .into_iter()
        .map(|genre| genre.name)
        .collect();
    genres.sort_by_key(|name| name.to_lowercase());

    Ok(Json(GenreListResponse { genres }))
}

#[cfg(test)]
#[path = "genres_tests.rs"]
mod genres_tests;
