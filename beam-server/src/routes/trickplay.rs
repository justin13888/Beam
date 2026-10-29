//! `/v1/files/{file_id}/trickplay/{sheet}` -- one of a file's scrub-preview
//! sheets (issue #265).
//!
//! The sheets are made in the background under Beam's data directory
//! (`services::trickplay`), never beside the library's files; this route only
//! reads them, beneath the trickplay folder with no link followed, as every
//! other file Beam serves is read. A source's `trickplay.sheet_urls` names
//! exactly the sheets this answers.

use kynos::http::etag::ETag;
use kynos::prelude::*;
use kynos::response::range::served::{Conditions, Served};
use tracing::{error, warn};
use uuid::Uuid;

use crate::routes::api_error::{ServerFault, SessionAuth, TrickplayDeliveryError};
use crate::routes::delivery::{AnyMedia, MediaRanges, RuntimeDelivery};
use crate::routes::stream::{FileByteSource, hides_file};
use crate::routes::tags::Playback;
use crate::services::trickplay::{LocatedSheet, SheetLookupError};
use crate::state::AppState;

/// What both trickplay operations capture.
#[derive(Debug, Schema, PathParams)]
pub struct TrickplayPath {
    /// The video file the sheet belongs to.
    pub file_id: Uuid,
    /// The sheet's position in the source's `trickplay.sheet_urls`, from 0.
    pub sheet: u32,
}

/// The one media type a sheet is served with.
pub struct JpegRanges;

impl MediaRanges for JpegRanges {
    const RANGES: &'static [&'static str] = &["image/jpeg"];
}

/// One trickplay sheet: a JPEG grid of tiles, laid out as the source's
/// `trickplay` says. Range-capable, like every delivery.
#[kynos::get(
    "/files/{file_id}/trickplay/{sheet}",
    tag = Playback,
    operation_id = "getTrickplaySheet"
)]
#[tracing::instrument(skip_all)]
pub async fn get_trickplay_sheet(
    auth: SessionAuth,
    Path(path): Path<TrickplayPath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<RuntimeDelivery<JpegRanges>, TrickplayDeliveryError> {
    deliver(&state, &auth, &path, &conditions).await
}

/// The same fields with no body, for a client checking a sheet's validator
/// before it fetches it.
#[kynos::head(
    "/files/{file_id}/trickplay/{sheet}",
    tag = Playback,
    operation_id = "headTrickplaySheet"
)]
#[tracing::instrument(skip_all)]
pub async fn head_trickplay_sheet(
    auth: SessionAuth,
    Path(path): Path<TrickplayPath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<RuntimeDelivery<JpegRanges>, TrickplayDeliveryError> {
    deliver(&state, &auth, &path, &conditions).await
}

fn file_not_found() -> TrickplayDeliveryError {
    TrickplayDeliveryError::FileNotFound("File not found".into())
}

fn sheet_not_found() -> TrickplayDeliveryError {
    TrickplayDeliveryError::TrickplayNotFound(
        "This file has no trickplay sheet with that number".into(),
    )
}

/// What both methods answer; a `HEAD` drops the body.
async fn deliver(
    state: &AppState,
    auth: &SessionAuth,
    path: &TrickplayPath,
    conditions: &Conditions,
) -> Result<RuntimeDelivery<JpegRanges>, TrickplayDeliveryError> {
    // Every caller logs its cause first (`hides_file` inside itself), so the
    // fault only summarises.
    let internal = |what: &'static str| TrickplayDeliveryError::Internal(ServerFault::logged(what));
    // The sheets of a file the caller may not see answer as those of a file
    // that does not exist (issue #263).
    match hides_file(state, auth, path.file_id).await {
        Ok(false) => {}
        Ok(true) => return Err(file_not_found()),
        Err(_) => return Err(internal("look up file")),
    }
    let file = state
        .services
        .file_repo
        .find_by_id(path.file_id)
        .await
        .map_err(|err| {
            error!(?err, "trickplay file lookup failed");
            internal("look up file")
        })?
        .ok_or_else(file_not_found)?;
    let LocatedSheet {
        root,
        path: sheet_path,
        etag,
    } = state
        .services
        .trickplay
        .locate_sheet(&file, path.sheet)
        .await
        .map_err(|err| match err {
            SheetLookupError::NotFound => sheet_not_found(),
            SheetLookupError::Db(err) => {
                error!(?err, "trickplay record lookup failed");
                internal("look up trickplay")
            }
        })?;
    // Recorded but not on disk: the data directory was cleared, or a sweep
    // removed a version this request had just looked up. Either way there is
    // no sheet to send, and the next pass makes it again.
    let (source, modified, _length) = FileByteSource::open(root, sheet_path.clone())
        .await
        .map_err(|err| {
            warn!(path = ?sheet_path, ?err, "a recorded trickplay sheet is not on disk");
            sheet_not_found()
        })?;

    let delivery = Served::<_, AnyMedia>::new(source)
        .etag(ETag::strong(etag))
        .last_modified(modified)
        .cache_control("private, max-age=3600")
        .deliver(conditions)
        .await
        .map_err(|err| {
            error!(?err, "failed to read a trickplay sheet");
            internal("read trickplay sheet")
        })?;

    Ok(RuntimeDelivery::new(delivery, "image/jpeg".to_owned()))
}

#[cfg(test)]
#[path = "trickplay_tests.rs"]
mod trickplay_tests;
