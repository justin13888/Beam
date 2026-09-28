//! `/v1/files/{file_id}/subtitles/{subtitle_id}` and its `/webvtt`
//! rendition -- a subtitle file beside a video, as stored and as WebVTT
//! (issue #189).
//!
//! Rewriting a text subtitle is not transcoding (ADR-0020): ADR-0004 keeps
//! Beam from re-encoding or remuxing media, and rewriting cue text is
//! neither. Both operations are read-only -- nothing is written beside the
//! library's files -- and read a subtitle file beneath its library root with
//! no symbolic link followed, at the file or any folder above it, and only as
//! a regular file, from one open handle.

use bytes::Bytes;
use kynos::http::etag::ETag;
use kynos::prelude::*;
use kynos::response::range::served::{Conditions, Served};
use kynos::response::range::source::InMemory;
use tracing::error;
use uuid::Uuid;

use beam_domain::models::sidecar::SubtitleFormat;

use crate::routes::api_error::{SessionAuth, SubtitleDeliveryError};
use crate::routes::delivery::{AnyMedia, MediaRanges, RuntimeDelivery};
use crate::routes::stream::{FileByteSource, validator_tag};
use crate::routes::tags::Playback;
use crate::services::subtitle::{LocatedSubtitle, SubtitleError, WebVttRendition};
use crate::state::AppState;

/// What both subtitle operations capture.
#[derive(Debug, Schema, PathParams)]
pub struct SubtitlePath {
    /// The video file the subtitle belongs to.
    pub file_id: Uuid,
    /// The subtitle's `sidecar_id` in its source's `subtitle_tracks`.
    pub subtitle_id: Uuid,
}

/// The media types a subtitle is served as stored with.
pub struct StoredSubtitleRanges;

impl MediaRanges for StoredSubtitleRanges {
    const RANGES: &'static [&'static str] = &[
        "text/vtt",
        "application/x-subrip",
        "text/x-ass",
        "text/x-ssa",
    ];
}

/// The one media type a WebVTT rendition is served with.
pub struct WebVttRanges;

impl MediaRanges for WebVttRanges {
    const RANGES: &'static [&'static str] = &["text/vtt"];
}

/// The `Content-Type` a subtitle file is served as stored with.
fn stored_content_type(format: SubtitleFormat) -> &'static str {
    match format {
        SubtitleFormat::Srt => "application/x-subrip",
        SubtitleFormat::Vtt => "text/vtt",
        SubtitleFormat::Ass => "text/x-ass",
        SubtitleFormat::Ssa => "text/x-ssa",
    }
}

/// What a rendition is served as: UTF-8 whatever the file was saved in.
const WEBVTT_CONTENT_TYPE: &str = "text/vtt; charset=utf-8";

/// The revision of the SubRip-to-WebVTT conversion, part of every
/// rendition's validator. A rendition is a function of the file *and* the
/// converter, so a strong validator (RFC 9110 8.8.1) must change when either
/// does: bump this with any change to `beam_domain::utils::subtitle` that
/// changes the bytes a file renders to, or a client holding the old
/// rendition is told `304` and keeps it.
///
/// Pinned by `the_converter_version_changes_with_its_output`, which fails when
/// the output changes and this has not.
const WEBVTT_CONVERTER_VERSION: u32 = 1;

impl From<SubtitleError> for SubtitleDeliveryError {
    fn from(err: SubtitleError) -> Self {
        match err {
            SubtitleError::FileNotFound => Self::FileNotFound("File not found".into()),
            SubtitleError::SubtitleNotFound => {
                Self::SubtitleNotFound("This file has no subtitle with that id".into())
            }
            SubtitleError::SourceFileMissing => {
                Self::SourceFileMissing("Subtitle file not found on disk".into())
            }
            SubtitleError::RenditionUnavailable => Self::RenditionUnavailable(
                "This subtitle is served only as stored; fetch its `url` instead".into(),
            ),
            SubtitleError::Internal(message) => Self::Internal(message),
        }
    }
}

/// A subtitle file beside a video, byte for byte as stored, with its
/// format's content type. Range-capable, like file delivery.
#[kynos::get(
    "/files/{file_id}/subtitles/{subtitle_id}",
    tag = Playback,
    operation_id = "getSubtitle"
)]
#[tracing::instrument(skip_all)]
pub async fn get_subtitle(
    _auth: SessionAuth,
    Path(path): Path<SubtitlePath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<RuntimeDelivery<StoredSubtitleRanges>, SubtitleDeliveryError> {
    let LocatedSubtitle {
        path: file_path,
        library_root,
        format,
    } = state
        .services
        .subtitles
        .locate(path.file_id, path.subtitle_id)
        .await?;
    let (source, modified, length) = FileByteSource::open(library_root, file_path.clone())
        .await
        .map_err(|err| {
            error!(path = ?file_path, ?err, "failed to read subtitle file metadata");
            SubtitleError::SourceFileMissing
        })?;

    let delivery = Served::<_, AnyMedia>::new(source)
        .etag(ETag::strong(validator_tag(modified, length)))
        .last_modified(modified)
        .cache_control("public, max-age=3600")
        .deliver(&conditions)
        .await
        .map_err(|err| {
            error!(?err, "failed to read subtitle file span");
            SubtitleDeliveryError::Internal("Failed to read subtitle file".into())
        })?;

    Ok(RuntimeDelivery::new(
        delivery,
        stored_content_type(format).to_owned(),
    ))
}

/// A SubRip or WebVTT subtitle file as WebVTT, which a browser's `<track>`
/// element reads: SubRip converted, WebVTT normalised to UTF-8. Offered
/// exactly where the track's `webvtt_url` is present; an ASS or SSA file, or
/// one over 8 MiB, is `#subtitle-rendition-unavailable`.
#[kynos::get(
    "/files/{file_id}/subtitles/{subtitle_id}/webvtt",
    tag = Playback,
    operation_id = "getSubtitleWebVtt"
)]
#[tracing::instrument(skip_all)]
pub async fn get_subtitle_webvtt(
    _auth: SessionAuth,
    Path(path): Path<SubtitlePath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<RuntimeDelivery<WebVttRanges>, SubtitleDeliveryError> {
    let WebVttRendition {
        text,
        modified,
        source_length,
    } = state
        .services
        .subtitles
        .webvtt(path.file_id, path.subtitle_id)
        .await?;

    // Derived from the file it was rendered from and from the converter that
    // rendered it, and marked so it never matches the file's own validator.
    let etag = ETag::strong(format!(
        "{}-vtt{WEBVTT_CONVERTER_VERSION}",
        validator_tag(modified, source_length)
    ));
    let delivery = match Served::<_, AnyMedia>::new(InMemory::new(Bytes::from(text)))
        .etag(etag)
        .last_modified(modified)
        .cache_control("public, max-age=3600")
        .deliver(&conditions)
        .await
    {
        Ok(delivery) => delivery,
        Err(never) => match never {},
    };

    Ok(RuntimeDelivery::new(
        delivery,
        WEBVTT_CONTENT_TYPE.to_owned(),
    ))
}

#[cfg(test)]
#[path = "subtitle_tests.rs"]
mod subtitle_tests;
