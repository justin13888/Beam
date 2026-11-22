//! `/v1/admin/files/{file_id}/markers`: an administrator's control of a
//! file's skip markers (issue #332). Every operation is admin-only
//! (`AdminAuth`).
//!
//! A file's markers are what its sources carry as `markers`: for each kind,
//! the one Beam keeps -- an administrator's, or one detected in the audio a
//! season's episodes share -- else the one a chapter titled for it names.
//! Setting a kind replaces a detected marker and overrides a chapter's;
//! clearing it stops Beam keeping one, so the chapters decide it again. Every
//! operation answers with the file's markers as they are after it.

use kynos::prelude::*;
use uuid::Uuid;

use beam_domain::models::chapter::{
    MarkerKind, MarkerOrigin, PlaybackMarker as DomainMarker, ResolvedMarker,
};

use crate::models::{
    FieldError, FileMarker, FileMarkerList, PlaybackMarkerKind, PlaybackMarkerOrigin,
    SetFileMarkerRequest,
};
use crate::routes::admin::record_change;
use crate::routes::api_error::{ERROR_BASE, INTERNAL_DETAIL, ServerFault};
use crate::routes::session_auth::AdminAuth;
use crate::routes::tags::Admin;
use crate::services::markers::MarkerError;
use crate::services::sources::wire_marker;
use crate::state::AppState;

/// What `/v1/admin/files/{file_id}/markers` captures.
#[derive(Debug, Schema, PathParams)]
pub struct FileMarkersPath {
    /// The video file.
    pub file_id: Uuid,
}

/// What `/v1/admin/files/{file_id}/markers/{kind}` captures.
#[derive(Debug, Schema, PathParams)]
pub struct FileMarkerPath {
    /// The video file.
    pub file_id: Uuid,
    /// Which of its markers.
    pub kind: PlaybackMarkerKind,
}

/// What every marker operation can fail with. A malformed id or kind is the
/// `Path` extractor's 400.
#[derive(Debug, thiserror::Error, ApiError)]
#[problem(base = ERROR_BASE)]
pub enum FileMarkerError {
    #[error("{0}")]
    #[problem(status = 404, title = "File not found")]
    FileNotFound(String),

    /// The span does not end after it starts, or ends past the file's end.
    #[error("the request body breaks {} rule(s)", errors.len())]
    #[problem(status = 422, title = "Validation failed")]
    ValidationFailed {
        #[problem(extension)]
        errors: Vec<FieldError>,
    },

    #[error("{}", INTERNAL_DETAIL)]
    #[problem(status = 500, title = "Internal server error")]
    Internal(ServerFault),
}

impl From<MarkerError> for FileMarkerError {
    fn from(error: MarkerError) -> Self {
        match error {
            MarkerError::FileNotFound => FileMarkerError::FileNotFound("File not found".into()),
            MarkerError::InvalidSpan(detail) => FileMarkerError::ValidationFailed {
                errors: vec![FieldError {
                    pointer: "/end_ms".to_owned(),
                    detail,
                }],
            },
            MarkerError::Db(err) => FileMarkerError::Internal(ServerFault::new(err)),
        }
    }
}

fn domain_kind(kind: PlaybackMarkerKind) -> MarkerKind {
    match kind {
        PlaybackMarkerKind::Recap => MarkerKind::Recap,
        PlaybackMarkerKind::Intro => MarkerKind::Intro,
        PlaybackMarkerKind::Credits => MarkerKind::Credits,
        PlaybackMarkerKind::Preview => MarkerKind::Preview,
    }
}

/// The kind as the wire spells it, for a record's message.
fn kind_name(kind: PlaybackMarkerKind) -> &'static str {
    match kind {
        PlaybackMarkerKind::Recap => "recap",
        PlaybackMarkerKind::Intro => "intro",
        PlaybackMarkerKind::Credits => "credits",
        PlaybackMarkerKind::Preview => "preview",
    }
}

fn list(file_id: Uuid, markers: Vec<ResolvedMarker>) -> FileMarkerList {
    FileMarkerList {
        file_id,
        items: markers
            .into_iter()
            .map(|resolved| {
                let ResolvedMarker { marker, origin } = resolved;
                let crate::models::PlaybackMarker {
                    kind,
                    start_ms,
                    end_ms,
                } = wire_marker(marker);
                FileMarker {
                    kind,
                    start_ms,
                    end_ms,
                    origin: match origin {
                        MarkerOrigin::Chapter => PlaybackMarkerOrigin::Chapter,
                        MarkerOrigin::Detected => PlaybackMarkerOrigin::Detected,
                        MarkerOrigin::Admin => PlaybackMarkerOrigin::Admin,
                    },
                }
            })
            .collect(),
    }
}

/// A file's markers, and where each comes from.
#[kynos::get(
    "/admin/files/{file_id}/markers",
    tag = Admin,
    operation_id = "getFileMarkers"
)]
pub async fn get_file_markers(
    _auth: AdminAuth,
    Path(path): Path<FileMarkersPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<FileMarkerList>, FileMarkerError> {
    let markers = state.services.markers.markers(path.file_id).await?;
    Ok(Json(list(path.file_id, markers)))
}

/// Set or correct one of a file's markers. It replaces a detected marker of
/// its kind, overrides a chapter's, and is never replaced by detection.
#[kynos::put(
    "/admin/files/{file_id}/markers/{kind}",
    tag = Admin,
    operation_id = "setFileMarker"
)]
pub async fn set_file_marker(
    auth: AdminAuth,
    Path(path): Path<FileMarkerPath>,
    Inject(state): Inject<AppState>,
    Json(body): Json<SetFileMarkerRequest>,
) -> Result<Json<FileMarkerList>, FileMarkerError> {
    let FileMarkerPath { file_id, kind } = path;
    let SetFileMarkerRequest { start_ms, end_ms } = body;
    let markers = state
        .services
        .markers
        .set(
            file_id,
            DomainMarker {
                kind: domain_kind(kind),
                start_ms,
                end_ms,
            },
        )
        .await?;
    record_change(
        &state,
        &auth,
        format!("Set the {} marker of file {file_id}", kind_name(kind)),
        serde_json::json!({
            "file_id": file_id,
            "kind": kind,
            "start_ms": start_ms,
            "end_ms": end_ms,
        }),
    )
    .await;
    Ok(Json(list(file_id, markers)))
}

/// Clear one of a file's markers, whether an administrator set it or Beam
/// detected it, so the file's chapters decide that kind again. Clearing a
/// kind Beam keeps none of changes nothing.
#[kynos::delete(
    "/admin/files/{file_id}/markers/{kind}",
    tag = Admin,
    operation_id = "clearFileMarker"
)]
pub async fn clear_file_marker(
    auth: AdminAuth,
    Path(path): Path<FileMarkerPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<FileMarkerList>, FileMarkerError> {
    let FileMarkerPath { file_id, kind } = path;
    let markers = state
        .services
        .markers
        .clear(file_id, domain_kind(kind))
        .await?;
    // Recorded even where Beam kept no marker of the kind: the store does not
    // say whether the clear removed one, and the request is what was made.
    record_change(
        &state,
        &auth,
        format!("Cleared the {} marker of file {file_id}", kind_name(kind)),
        serde_json::json!({ "file_id": file_id, "kind": kind }),
    )
    .await;
    Ok(Json(list(file_id, markers)))
}

#[cfg(test)]
#[path = "markers_tests.rs"]
mod markers_tests;
