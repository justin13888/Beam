//! Sidecar subtitle delivery (issue #189).
//!
//! A subtitle file beside a video is served two ways: as stored, and -- for
//! SubRip and WebVTT -- as WebVTT, which is all a browser's `<track>` reads.
//! Both are read-only: Beam never writes into a library root, so a WebVTT
//! rendition is produced per request and never saved beside the original.
//! Subtitles embedded in a video are never extracted (ADR-0004); a client
//! reads those from the stream it is already playing.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use thiserror::Error;
use tracing::error;
use uuid::Uuid;

use beam_domain::models::sidecar::{SidecarSubtitle, SubtitleFormat};
use beam_domain::repositories::{FileRepository, LibraryRepository, SidecarSubtitleRepository};
use beam_domain::utils::subtitle::{normalize_webvtt, srt_to_webvtt};
use beam_index::library_file::{open_regular_file, relative_to};

use crate::services::sources::{SUBTITLE_CONVERT_MAX_BYTES, has_webvtt_rendition};

/// Why a subtitle could not be served.
#[derive(Debug, Error)]
pub enum SubtitleError {
    /// No present video file has this id.
    #[error("file not found")]
    FileNotFound,
    /// The video file has no subtitle file with this id.
    #[error("subtitle not found")]
    SubtitleNotFound,
    /// The index has the subtitle; the path it names is not on disk.
    #[error("subtitle file missing from disk")]
    SourceFileMissing,
    /// The subtitle is not offered as WebVTT: an ASS or SSA file, or one too
    /// large to convert.
    #[error("no WebVTT rendition of this subtitle")]
    RenditionUnavailable,
    #[error("{0}")]
    Internal(String),
}

/// A subtitle file found for delivery.
#[derive(Debug, Clone, PartialEq)]
pub struct LocatedSubtitle {
    pub path: PathBuf,
    /// Root of the library the video was indexed under, which the subtitle
    /// file is opened beneath (FR-212).
    pub library_root: PathBuf,
    pub format: SubtitleFormat,
}

/// A subtitle rewritten as WebVTT, with what a validator is minted from.
#[derive(Debug, Clone, PartialEq)]
pub struct WebVttRendition {
    pub text: String,
    /// The subtitle file's modification time and length, as read: the
    /// rendition changes exactly when they do.
    pub modified: SystemTime,
    pub source_length: u64,
}

#[async_trait::async_trait]
pub trait SubtitleService: Send + Sync + std::fmt::Debug {
    /// The subtitle file `subtitle_id` beside the video file `file_id`.
    ///
    /// Fails with [`SubtitleError::FileNotFound`] when no present file has
    /// that id -- a subtitle of a missing video is not served, whatever
    /// its own file's state -- and [`SubtitleError::SubtitleNotFound`] when
    /// the file has no subtitle with that id, which is also the answer for
    /// another file's subtitle.
    async fn locate(
        &self,
        file_id: Uuid,
        subtitle_id: Uuid,
    ) -> Result<LocatedSubtitle, SubtitleError>;

    /// The subtitle as WebVTT: SubRip converted, WebVTT normalised. Anything
    /// else, or a file over the conversion ceiling, is
    /// [`SubtitleError::RenditionUnavailable`].
    async fn webvtt(
        &self,
        file_id: Uuid,
        subtitle_id: Uuid,
    ) -> Result<WebVttRendition, SubtitleError>;
}

/// Serves the sidecars the indexer recorded, reading them from disk.
#[derive(Debug)]
pub struct DbSubtitleService {
    libraries: Arc<dyn LibraryRepository>,
    files: Arc<dyn FileRepository>,
    sidecars: Arc<dyn SidecarSubtitleRepository>,
}

impl DbSubtitleService {
    pub fn new(
        libraries: Arc<dyn LibraryRepository>,
        files: Arc<dyn FileRepository>,
        sidecars: Arc<dyn SidecarSubtitleRepository>,
    ) -> Self {
        Self {
            libraries,
            files,
            sidecars,
        }
    }

    /// The subtitle `subtitle_id` of the video file `file_id`, with the root
    /// of the library the video is in.
    async fn find(
        &self,
        file_id: Uuid,
        subtitle_id: Uuid,
    ) -> Result<(SidecarSubtitle, PathBuf), SubtitleError> {
        let internal = |err: sea_orm::DbErr| {
            error!(%file_id, %subtitle_id, ?err, "failed to look up a subtitle");
            SubtitleError::Internal("Failed to look up subtitle".to_owned())
        };
        // A visible read: a video file gone from disk takes its subtitles'
        // delivery with it, though their rows stay until the next scan.
        let Some(file) = self.files.find_by_id(file_id).await.map_err(internal)? else {
            return Err(SubtitleError::FileNotFound);
        };
        // A library's files go with it; were the video's gone, there would
        // be no root to open its subtitles beneath.
        let Some(library) = self
            .libraries
            .find_by_id(file.library_id)
            .await
            .map_err(internal)?
        else {
            return Err(SubtitleError::FileNotFound);
        };
        let sidecar = self
            .sidecars
            .find_by_file_id(file_id)
            .await
            .map_err(internal)?
            .into_iter()
            .find(|sidecar| sidecar.id == subtitle_id)
            .ok_or(SubtitleError::SubtitleNotFound)?;
        Ok((sidecar, library.root_path))
    }
}

#[async_trait::async_trait]
impl SubtitleService for DbSubtitleService {
    async fn locate(
        &self,
        file_id: Uuid,
        subtitle_id: Uuid,
    ) -> Result<LocatedSubtitle, SubtitleError> {
        let (sidecar, library_root) = self.find(file_id, subtitle_id).await?;
        Ok(LocatedSubtitle {
            path: sidecar.path,
            library_root,
            format: sidecar.info.format,
        })
    }

    async fn webvtt(
        &self,
        file_id: Uuid,
        subtitle_id: Uuid,
    ) -> Result<WebVttRendition, SubtitleError> {
        let (sidecar, library_root) = self.find(file_id, subtitle_id).await?;
        let format = sidecar.info.format;
        // Decided on the recorded size first, as the sources route decided
        // whether to offer the rendition at all, and again by `render_file`
        // on the file as it is now.
        if !has_webvtt_rendition(format, sidecar.size_bytes) {
            return Err(SubtitleError::RenditionUnavailable);
        }
        let render: fn(&[u8]) -> String = match format {
            SubtitleFormat::Srt => |bytes| srt_to_webvtt(bytes).vtt,
            SubtitleFormat::Vtt => normalize_webvtt,
            SubtitleFormat::Ass | SubtitleFormat::Ssa => {
                return Err(SubtitleError::RenditionUnavailable);
            }
        };
        let path = sidecar.path;
        tokio::task::spawn_blocking(move || render_file(&library_root, &path, format, render))
            .await
            .map_err(|err| SubtitleError::Internal(format!("subtitle conversion failed: {err}")))?
    }
}

/// Read the subtitle file at `path`, in the library rooted at
/// `library_root`, and render it, all from one handle.
///
/// Opened as every read of a library file is -- beneath its root with no
/// symbolic link followed, the file's own or a folder's above it, and only a
/// regular file ([`open_regular_file`]) -- so a sidecar, or a folder above
/// it, replaced by a link since the scan, or a sidecar replaced by a FIFO or
/// a device, is [`SubtitleError::SourceFileMissing`], never another file's
/// contents. The
/// size is checked again on the handle, as the file may have grown since the
/// scan, and the read is bounded by the ceiling whatever the handle claims.
fn render_file(
    library_root: &Path,
    path: &Path,
    format: SubtitleFormat,
    render: fn(&[u8]) -> String,
) -> Result<WebVttRendition, SubtitleError> {
    let missing = |err: std::io::Error| {
        error!(?path, ?err, "failed to read a subtitle file");
        SubtitleError::SourceFileMissing
    };
    let (file, metadata) = relative_to(library_root, path)
        .and_then(|relative| open_regular_file(library_root, relative))
        .map_err(missing)?;
    if !has_webvtt_rendition(format, metadata.len()) {
        return Err(SubtitleError::RenditionUnavailable);
    }
    let mut bytes = Vec::new();
    file.take(SUBTITLE_CONVERT_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(missing)?;
    let source_length = bytes.len() as u64;
    if !has_webvtt_rendition(format, source_length) {
        return Err(SubtitleError::RenditionUnavailable);
    }
    Ok(WebVttRendition {
        text: render(&bytes),
        modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        source_length,
    })
}
