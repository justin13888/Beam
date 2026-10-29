//! `/v1/people` -- people search and a person's filmography (issue #261).
//! Follows the `/v1` conventions `media` set out: RFC 9457 problem bodies,
//! cookie-session auth, and an `{items, page_info, total}` connection paged
//! by `first`/`after`.

use kynos::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::{PersonConnection, PersonDetail};
use crate::routes::api_error::{PeopleSearchError, PersonLookupError, ServerFault, SessionAuth};
use crate::routes::playback::caller_access;
use crate::routes::tags::Media;
use crate::services::people::PeopleError;
use crate::state::AppState;

/// Everything `GET /v1/people` accepts.
#[derive(Debug, Default, Serialize, Deserialize, Schema, QueryParams)]
pub struct PeopleQuery {
    /// Search the people credited in the library: those whose name contains
    /// this text, ignoring case. Without it -- or with only spaces -- every
    /// credited person is listed.
    pub query: Option<String>,
    /// This many people (1-100, default 20), after `after` or from the start.
    #[schema(minimum = 1, maximum = 100)]
    pub first: Option<u32>,
    /// An opaque cursor -- a page's `end_cursor` -- to page on from.
    pub after: Option<String>,
}

/// What `/v1/people/{id}` captures.
#[derive(Debug, Schema, PathParams)]
pub struct PersonPath {
    /// Person id.
    pub id: Uuid,
}

/// Search the people credited on the library's titles, by name.
///
/// People come from provider enrichment: the cast and key crew of each
/// matched title. The list is ordered by name, ignoring case. A person
/// credited only on titles the caller's access policy hides is not listed.
#[kynos::get("/people", tag = Media, operation_id = "searchPeople")]
pub async fn search_people(
    auth: SessionAuth,
    Query(params): Query<PeopleQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<PersonConnection>, PeopleSearchError> {
    let PeopleQuery {
        query,
        first,
        after,
    } = params;
    let (_, policy) = caller_access(&state, &auth)
        .await
        .map_err(|err| PeopleSearchError::Internal(ServerFault::new(err)))?;
    match state
        .services
        .people
        .search(query, &policy, first, after)
        .await
    {
        Ok(connection) => Ok(Json(connection)),
        Err(PeopleError::InvalidCursor(detail)) => Err(PeopleSearchError::InvalidCursor(detail)),
        Err(PeopleError::InvalidPagination(detail)) => {
            Err(PeopleSearchError::InvalidPagination(detail))
        }
        Err(PeopleError::InvalidSearchQuery(detail)) => {
            Err(PeopleSearchError::InvalidSearchQuery(detail))
        }
        Err(PeopleError::Internal(detail)) => {
            Err(PeopleSearchError::Internal(ServerFault::new(detail)))
        }
    }
}

/// One person and their filmography: every title in the library they are in
/// the cast or crew of.
///
/// Titles the caller's access policy hides are left out, and a person
/// credited only on such titles answers `404`, as an unknown id does.
#[kynos::get("/people/{id}", tag = Media, operation_id = "getPerson")]
pub async fn get_person(
    auth: SessionAuth,
    Path(path): Path<PersonPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<PersonDetail>, PersonLookupError> {
    let (_, policy) = caller_access(&state, &auth)
        .await
        .map_err(|err| PersonLookupError::Internal(ServerFault::new(err)))?;
    // A failed read is a 500: answering 404 would tell the client the person
    // is gone when the server only failed to read them.
    match state.services.people.detail(path.id, &policy).await {
        Ok(Some(detail)) => Ok(Json(detail)),
        Ok(None) => Err(PersonLookupError::PersonNotFound(format!(
            "person {} not found",
            path.id
        ))),
        Err(err) => Err(PersonLookupError::Internal(ServerFault::new(err))),
    }
}

#[cfg(test)]
#[path = "people_tests.rs"]
mod people_tests;
