//! The watchlist and favourites (issue #262): `/v1/watchlist`,
//! `/v1/watchlist/{id}`, `/v1/favourites` and `/v1/favourites/{id}`. The user
//! is always the session's, never the request's -- one user can never read
//! or change another's lists.
//!
//! Two lists of one shape, each its own operations rather than one route
//! taking the list as a parameter, so a generated client calls
//! `addToWatchlist` rather than passing a string it could misspell.

use kynos::prelude::*;
use kynos::response::status::NoContent;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use beam_domain::models::user_list::UserList;

use crate::models::UserListConnection;
use crate::routes::api_error::{
    InternalError, ListMembershipError, ListPageError, ServerFault, SessionAuth,
};
use crate::routes::playback::{caller_access, hides, media_not_found};
use crate::routes::tags::Playback;
use crate::services::lists::ListError;
use crate::state::AppState;

/// What `/v1/watchlist/{id}` and `/v1/favourites/{id}` capture.
#[derive(Debug, Schema, PathParams)]
pub struct ListTitlePath {
    /// A movie or show id.
    pub id: Uuid,
}

/// How `GET /v1/watchlist` and `GET /v1/favourites` are paged.
#[derive(Debug, Default, Serialize, Deserialize, Schema, QueryParams)]
pub struct ListQuery {
    /// This many titles (1-100, default 50).
    #[schema(minimum = 1, maximum = 100)]
    pub first: Option<u32>,
    /// An opaque cursor -- a page's `end_cursor` -- to page on from.
    pub after: Option<String>,
}

impl From<InternalError> for ListMembershipError {
    fn from(err: InternalError) -> Self {
        let InternalError::Internal(detail) = err;
        Self::Internal(detail)
    }
}

impl From<InternalError> for ListPageError {
    fn from(err: InternalError) -> Self {
        let InternalError::Internal(detail) = err;
        Self::Internal(detail)
    }
}

impl From<ListError> for ListMembershipError {
    /// Adding and removing take no cursor and no page size, so neither
    /// pagination error reaches here; were one to, it is no fault of the id.
    fn from(err: ListError) -> Self {
        match err {
            ListError::MediaNotFound(detail) => Self::MediaNotFound(detail),
            ListError::NotATitle(detail) => Self::NotATitle(detail),
            ListError::InvalidCursor(_)
            | ListError::InvalidPagination(_)
            | ListError::Internal(_) => Self::Internal(ServerFault::new(err)),
        }
    }
}

impl From<ListError> for ListPageError {
    /// A page names no title, so neither title error reaches here.
    fn from(err: ListError) -> Self {
        match err {
            ListError::InvalidCursor(detail) => Self::InvalidCursor(detail),
            ListError::InvalidPagination(detail) => Self::InvalidPagination(detail),
            ListError::MediaNotFound(_) | ListError::NotATitle(_) | ListError::Internal(_) => {
                Self::Internal(ServerFault::new(err))
            }
        }
    }
}

/// The session's user, once `id` is known not to name something their
/// access policy hides (issue #328): a hidden title, or a season or episode
/// of one, answers exactly as an id naming nothing.
async fn visible_to_caller(
    auth: &SessionAuth,
    state: &AppState,
    id: Uuid,
) -> Result<Uuid, ListMembershipError> {
    let (user_id, policy) = caller_access(state, auth).await?;
    if hides(state, &policy, id).await? {
        return Err(ListMembershipError::MediaNotFound(media_not_found(id)));
    }
    Ok(user_id)
}

async fn add(
    auth: &SessionAuth,
    state: &AppState,
    list: UserList,
    id: Uuid,
) -> Result<NoContent, ListMembershipError> {
    let user_id = visible_to_caller(auth, state, id).await?;
    state.services.lists.add(user_id, list, id).await?;
    Ok(NoContent)
}

async fn remove(
    auth: &SessionAuth,
    state: &AppState,
    list: UserList,
    id: Uuid,
) -> Result<NoContent, ListMembershipError> {
    let user_id = visible_to_caller(auth, state, id).await?;
    state.services.lists.remove(user_id, list, id).await?;
    Ok(NoContent)
}

async fn page(
    auth: &SessionAuth,
    state: &AppState,
    list: UserList,
    query: ListQuery,
) -> Result<Json<UserListConnection>, ListPageError> {
    let (user_id, policy) = caller_access(state, auth).await?;
    let ListQuery { first, after } = query;
    let connection = state
        .services
        .lists
        .page(user_id, &policy, list, first, after)
        .await?;
    Ok(Json(connection))
}

/// Put a movie or a show on the caller's watchlist.
///
/// One already there keeps its place. A season or an episode is refused:
/// put its show on the list instead. A title the caller's access policy
/// hides answers `404`, as an id naming nothing does.
#[kynos::put("/watchlist/{id}", tag = Playback, operation_id = "addToWatchlist")]
pub async fn add_to_watchlist(
    auth: SessionAuth,
    Path(path): Path<ListTitlePath>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, ListMembershipError> {
    add(&auth, &state, UserList::Watchlist, path.id).await
}

/// Take a movie or a show off the caller's watchlist. One not on it changes
/// nothing.
///
/// A title the caller's access policy hides answers `404`, as an id naming
/// nothing does.
#[kynos::delete(
    "/watchlist/{id}",
    tag = Playback,
    operation_id = "removeFromWatchlist"
)]
pub async fn remove_from_watchlist(
    auth: SessionAuth,
    Path(path): Path<ListTitlePath>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, ListMembershipError> {
    remove(&auth, &state, UserList::Watchlist, path.id).await
}

/// The caller's watchlist, most recently added first, a page at a time.
///
/// Titles the caller's access policy hides are left out of every page and
/// of `total`; they stay on the list, and appear again once the policy
/// admits them.
#[kynos::get("/watchlist", tag = Playback, operation_id = "getWatchlist")]
pub async fn get_watchlist(
    auth: SessionAuth,
    Query(query): Query<ListQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<UserListConnection>, ListPageError> {
    page(&auth, &state, UserList::Watchlist, query).await
}

/// Mark a movie or a show as one of the caller's favourites.
///
/// One already a favourite keeps its place. A season or an episode is
/// refused: favourite its show instead. A title the caller's access policy
/// hides answers `404`, as an id naming nothing does.
#[kynos::put("/favourites/{id}", tag = Playback, operation_id = "addToFavourites")]
pub async fn add_to_favourites(
    auth: SessionAuth,
    Path(path): Path<ListTitlePath>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, ListMembershipError> {
    add(&auth, &state, UserList::Favourites, path.id).await
}

/// Take a movie or a show out of the caller's favourites. One not among
/// them changes nothing.
///
/// A title the caller's access policy hides answers `404`, as an id naming
/// nothing does.
#[kynos::delete(
    "/favourites/{id}",
    tag = Playback,
    operation_id = "removeFromFavourites"
)]
pub async fn remove_from_favourites(
    auth: SessionAuth,
    Path(path): Path<ListTitlePath>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, ListMembershipError> {
    remove(&auth, &state, UserList::Favourites, path.id).await
}

/// The caller's favourites, most recently added first, a page at a time.
///
/// Titles the caller's access policy hides are left out of every page and
/// of `total`; they stay among the favourites, and appear again once the
/// policy admits them.
#[kynos::get("/favourites", tag = Playback, operation_id = "getFavourites")]
pub async fn get_favourites(
    auth: SessionAuth,
    Query(query): Query<ListQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<UserListConnection>, ListPageError> {
    page(&auth, &state, UserList::Favourites, query).await
}

#[cfg(test)]
#[path = "lists_tests.rs"]
mod lists_tests;
