//! `/v1/files/{file_id}/stream` and `/v1/files/{file_id}/download` -- direct
//! byte delivery of an indexed source file. Beam never transcodes or remuxes
//! server-side (ADR-0004), so both endpoints serve the file exactly as indexed
//! and differ only in `Content-Disposition`.
//!
//! The Kynos migration replaced roughly three hundred lines of hand-rolled
//! range parsing and header assembly with `Served<S, M>` over a [`ByteSource`].
//! Three things follow from that:
//!
//! * `If-Range`, `If-None-Match` and `If-Modified-Since` are honoured, and a
//!   `304` is possible. None of them existed before, so a seek re-sent bytes
//!   the client already had.
//! * `HEAD` is a real operation. Kynos does not synthesise one from a `GET`,
//!   because the two are separate operations in the description.
//! * The `ETag` is no longer `"{file_size}"`. That collided for any two files
//!   of the same size, which made it unsafe to resume against -- exactly the
//!   guarantee `If-Range` exists to provide.

use std::path::{Path as FsPath, PathBuf};
use std::time::SystemTime;

use bytes::Bytes;
use kynos::http::etag::ETag;
use kynos::prelude::*;
use kynos::response::range::served::{Conditions, Served};
use kynos::response::range::source::ByteSource;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::error;
use uuid::Uuid;

use beam_index::library_file::{open_regular_file, relative_to};

use crate::routes::api_error::{DeliveryError, SessionAuth};
use crate::routes::delivery::{AnyMedia, MediaRanges, RuntimeDelivery};
use crate::routes::tags::Playback;
use crate::services::library::LocatedFile;
use crate::state::AppState;

/// What both delivery endpoints capture.
#[derive(Debug, Schema, PathParams)]
pub struct FilePath {
    /// File ID.
    pub file_id: Uuid,
}

/// One indexed file on disk, read a span at a time.
///
/// Kynos's `Served<S, M>` range engine reads through its [`ByteSource`]; this
/// is the one Beam serves library files with. It holds an open handle, never
/// a path, and the delivery tests drive it end to end over real files in a
/// `TempDir`.
pub struct FileByteSource {
    /// The one handle every span is read from, so the bytes served are those
    /// of the file that was opened and statted -- never of whatever the path
    /// names by the time a span is read.
    file: tokio::sync::Mutex<tokio::fs::File>,
    length: u64,
}

impl FileByteSource {
    /// Opens the file at `path` in the library rooted at `library_root`, and
    /// reads its metadata from the handle, without reading a byte of its
    /// contents.
    ///
    /// Opened as every read of a library file is: beneath its root with no
    /// symbolic link followed -- neither the file nor a folder above it --
    /// and only a regular file, so a link, FIFO or device put in the path
    /// since it was indexed fails here (FR-212, [`beam_index::library_file`]).
    /// Shared with subtitle delivery, which serves a sidecar file the same
    /// way; each caller says what a file it cannot open means to its client.
    pub(crate) async fn open(
        library_root: PathBuf,
        path: PathBuf,
    ) -> std::io::Result<(Self, SystemTime, u64)> {
        let (file, metadata) = tokio::task::spawn_blocking(move || {
            open_regular_file(&library_root, relative_to(&library_root, &path)?)
        })
        .await??;

        let length = metadata.len();
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        let source = Self {
            file: tokio::sync::Mutex::new(tokio::fs::File::from_std(file)),
            length,
        };
        Ok((source, modified, length))
    }
}

impl ByteSource for FileByteSource {
    type Error = std::io::Error;

    /// Asked once, before a byte is read, so an unsatisfiable range costs no
    /// read at all.
    async fn complete_length(&self) -> Result<u64, Self::Error> {
        Ok(self.length)
    }

    /// Reads exactly the span asked for. The whole file is never held: a client
    /// seeking to the two-hour mark of a 40 GiB remux costs one span.
    async fn read_span(&self, first: u64, last: u64) -> Result<Bytes, Self::Error> {
        let mut file = self.file.lock().await;
        file.seek(std::io::SeekFrom::Start(first)).await?;

        let span = usize::try_from(last - first + 1).unwrap_or(0);
        let mut buffer = vec![0u8; span];
        file.read_exact(&mut buffer).await?;

        Ok(Bytes::from(buffer))
    }
}

/// The media-type ranges a source-file delivery answers with.
///
/// Beam serves whatever the indexer detected, which is decided per file and
/// cannot be a `const`; `application/octet-stream` is what an unrecognised
/// container falls back to.
pub struct SourceFileRanges;

impl MediaRanges for SourceFileRanges {
    const RANGES: &'static [&'static str] = &["video/*", "audio/*", "application/octet-stream"];
}

/// A ranged delivery of one indexed source file.
pub type MediaDelivery = RuntimeDelivery<SourceFileRanges>;

/// Resolve `file_id` to the file's on-disk path, its library's root and its
/// detected content type.
///
/// The caller must be signed in via the `beam_session` cookie (ADR-0003) -- a
/// `<video>` element sends that automatically, so there is no separate
/// stream-token step. Authentication itself is `SessionAuth` in the handler
/// signature; this only resolves the file.
async fn locate_file(
    state: &AppState,
    file_id: Uuid,
) -> Result<(PathBuf, PathBuf, String), DeliveryError> {
    let file = match state.services.library.get_file_by_id(file_id).await {
        Ok(Some(file)) => file,
        Ok(None) => return Err(DeliveryError::FileNotFound("File not found".into())),
        Err(err) => {
            error!(?err, "failed to look up file");
            return Err(DeliveryError::Internal("Failed to look up file".into()));
        }
    };

    let LocatedFile {
        id: _,
        path,
        library_root,
        mime_type,
    } = file;

    let content_type = mime_type.unwrap_or_else(|| "application/octet-stream".to_owned());

    Ok((path, library_root, content_type))
}

/// A validator that changes whenever the bytes do.
///
/// Modification time and size, which is the shape nginx mints and is strong
/// enough for `If-Range` to mean something: a re-index that rewrites a file
/// moves its mtime, and a different file of the same size has a different one.
/// The previous `"{file_size}"` was neither -- every 4 GiB remux shared it, so
/// a resumed download could splice bytes from a different file.
fn validator(modified: SystemTime, length: u64) -> ETag {
    ETag::strong(validator_tag(modified, length))
}

/// The opaque part of [`validator`]: `{mtime}-{length}` in hex. Shared with
/// subtitle delivery, which derives a rendition's validator from the file
/// it was rendered from.
pub(crate) fn validator_tag(modified: SystemTime, length: u64) -> String {
    let stamp = modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{stamp:x}-{length:x}")
}

/// Builds the delivery both endpoints share.
async fn deliver(
    state: &AppState,
    file_id: Uuid,
    conditions: &Conditions,
    attachment: bool,
) -> Result<MediaDelivery, DeliveryError> {
    let (path, library_root, content_type) = locate_file(state, file_id).await?;
    // Missing, or no longer a regular file reached without a link: either
    // way the file indexed is not there to serve.
    let (source, modified, length) = FileByteSource::open(library_root, path.clone())
        .await
        .map_err(|err| {
            error!(?path, ?err, "failed to open source file");
            DeliveryError::SourceFileMissing("Source video file not found".into())
        })?;

    let mut served = Served::<_, AnyMedia>::new(source)
        .etag(validator(modified, length))
        .last_modified(modified)
        .cache_control("public, max-age=3600");

    if attachment {
        // Kynos owns the RFC 6266 encoding, so the hand-written quote escaping
        // this replaces is gone along with it.
        served = served.attachment(download_filename(&path, file_id));
    }

    let delivery = served.deliver(conditions).await.map_err(|err| {
        error!(?err, "failed to read source file span");
        DeliveryError::Internal("Failed to read source file".into())
    })?;

    Ok(MediaDelivery::new(delivery, content_type))
}

/// The name a download is saved under.
fn download_filename(path: &FsPath, file_id: Uuid) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{file_id}.bin"))
}

/// Direct-play stream via HTTP Range. Serves the source file's bytes exactly
/// as indexed on disk -- Beam never transcodes or remuxes media server-side
/// (see ADR-0004); the response `Content-Type` reflects the file's actual
/// detected MIME type rather than assuming MP4. Rendered inline (no
/// `Content-Disposition`) so a `<video>` element plays it in place.
#[kynos::get("/files/{file_id}/stream", tag = Playback, operation_id = "streamFile")]
#[tracing::instrument(skip_all)]
pub async fn stream_file(
    _auth: SessionAuth,
    Path(path): Path<FilePath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<MediaDelivery, DeliveryError> {
    deliver(&state, path.file_id, &conditions, false).await
}

/// The same fields with no body, for a player sizing the stream before it
/// starts.
#[kynos::head("/files/{file_id}/stream", tag = Playback, operation_id = "headStreamFile")]
#[tracing::instrument(skip_all)]
pub async fn head_stream_file(
    _auth: SessionAuth,
    Path(path): Path<FilePath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<MediaDelivery, DeliveryError> {
    deliver(&state, path.file_id, &conditions, false).await
}

/// Download the full source file as an attachment. Same auth and Range
/// support as [`stream_file`] (so a paused/interrupted download can resume),
/// but sets `Content-Disposition: attachment` with the original filename so
/// the browser saves it rather than attempting inline playback.
#[kynos::get("/files/{file_id}/download", tag = Playback, operation_id = "downloadFile")]
#[tracing::instrument(skip_all)]
pub async fn download_file(
    _auth: SessionAuth,
    Path(path): Path<FilePath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<MediaDelivery, DeliveryError> {
    deliver(&state, path.file_id, &conditions, true).await
}

/// The same fields with no body, for a client sizing the download first.
#[kynos::head("/files/{file_id}/download", tag = Playback, operation_id = "headDownloadFile")]
#[tracing::instrument(skip_all)]
pub async fn head_download_file(
    _auth: SessionAuth,
    Path(path): Path<FilePath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<MediaDelivery, DeliveryError> {
    deliver(&state, path.file_id, &conditions, true).await
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod stream_tests;
