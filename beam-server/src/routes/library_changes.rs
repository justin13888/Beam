//! `GET /v1/libraries/changes` (issue #454): the libraries the filesystem
//! watcher changed since a client last asked, so an open browse or home
//! screen refetches when new content is indexed rather than on the next
//! manual refresh.
//!
//! A polled cursor rather than a stream: see `docs/architecture/api.md`,
//! "Library change feed".

use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use beam_index::services::notification::ChangedLibraries;

use crate::models::{LibraryChanges, decode_change_cursor, encode_change_cursor};
use crate::routes::access::caller_access;
use crate::routes::api_error::InternalError;
use crate::routes::session_auth::SessionAuth;
use crate::routes::tags::Admin;
use crate::state::AppState;

/// Where a poll of the library change feed continues from.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct LibraryChangesQuery {
    /// The `cursor` the previous poll returned. Absent on a client's first
    /// poll; absent, malformed or issued before the server restarted, the
    /// answer is a reset.
    pub since: Option<String>,
}

/// The libraries whose contents changed since `since`, among those the
/// caller can see.
///
/// Poll it while a library is on screen, passing back the `cursor` each
/// answer returns; refetch each library in `library_ids`, or every library
/// when `reset` is `true`. Library-level only: it never names a file.
#[kynos::get("/libraries/changes", tag = Admin, operation_id = "getLibraryChanges")]
pub async fn get_library_changes(
    auth: SessionAuth,
    Query(query): Query<LibraryChangesQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<LibraryChanges>, InternalError> {
    let LibraryChangesQuery { since } = query;
    let (_, policy) = caller_access(&state, &auth).await?;
    let since = since.as_deref().and_then(decode_change_cursor);
    let changes = state.services.notification.library_changes(since);
    let (reset, library_ids) = match changes.changed {
        ChangedLibraries::All => (true, Vec::new()),
        ChangedLibraries::Only(library_ids) => (
            false,
            library_ids
                .into_iter()
                .filter(|library_id| policy.admits_library(*library_id))
                .collect(),
        ),
    };
    Ok(Json(LibraryChanges {
        cursor: encode_change_cursor(changes.cursor),
        reset,
        library_ids,
    }))
}

#[cfg(test)]
#[path = "library_changes_tests.rs"]
mod library_changes_tests;
