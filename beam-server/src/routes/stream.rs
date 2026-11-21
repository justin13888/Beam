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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use bytes::Bytes;
use kynos::http::etag::ETag;
use kynos::http::media::MediaRanges;
use kynos::prelude::*;
use kynos::response::range::served::{Conditions, Delivery, Served};
use kynos::response::range::source::ByteSource;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::error;
use uuid::Uuid;

use beam_index::library_file::{open_regular_file, relative_to};

use crate::routes::access::caller_access;
use crate::routes::api_error::{DeliveryError, ServerFault};
use crate::routes::session_auth::SessionAuth;
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

/// Which delivery endpoint a body is served through: the `endpoint` label of
/// the streaming metrics ([#458](https://github.com/justin13888/beam/issues/458)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryEndpoint {
    /// `/v1/files/{file_id}/stream`, played in place.
    Stream,
    /// `/v1/files/{file_id}/download`, saved as an attachment.
    Download,
}

impl DeliveryEndpoint {
    /// The `endpoint` label value.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Stream => "stream",
            Self::Download => "download",
        }
    }

    /// The endpoint whose body-carrying operation is `operation_id`.
    ///
    /// Read by the HTTP observer, which learns of a dropped body only through
    /// the route it was served on. The `HEAD` operations send no body, so they
    /// have none to abandon.
    pub(crate) fn of_operation(operation_id: &str) -> Option<Self> {
        match operation_id {
            "streamFile" => Some(Self::Stream),
            "downloadFile" => Some(Self::Download),
            _ => None,
        }
    }
}

/// A [`FileByteSource`] that reports what it delivers.
///
/// Kynos's range body reads its source one span at a time as the client
/// takes the bytes, and holds the source until the body is finished or
/// dropped. So every span read is counted into `beam_stream_bytes_total` as
/// it goes, and the first read marks the body active in `beam_streams_active`
/// until the source is dropped with it. A `HEAD`, a `304` or a refused range
/// reads nothing and is never active.
///
/// The bytes are those handed to the body: a client that leaves mid-span is
/// counted for the span it was being sent.
pub struct MeteredSource {
    inner: FileByteSource,
    endpoint: DeliveryEndpoint,
    /// Whether a span has been read, and so the body counted active.
    active: AtomicBool,
}

impl MeteredSource {
    fn new(inner: FileByteSource, endpoint: DeliveryEndpoint) -> Self {
        Self {
            inner,
            endpoint,
            active: AtomicBool::new(false),
        }
    }
}

impl ByteSource for MeteredSource {
    type Error = std::io::Error;

    async fn complete_length(&self) -> Result<u64, Self::Error> {
        self.inner.complete_length().await
    }

    async fn read_span(&self, first: u64, last: u64) -> Result<Bytes, Self::Error> {
        if !self.active.swap(true, Ordering::Relaxed) {
            metrics::gauge!("beam_streams_active", "endpoint" => self.endpoint.label())
                .increment(1.0);
        }
        let span = self.inner.read_span(first, last).await?;
        metrics::counter!("beam_stream_bytes_total", "endpoint" => self.endpoint.label())
            .increment(span.len() as u64);
        Ok(span)
    }
}

impl Drop for MeteredSource {
    fn drop(&mut self) {
        if *self.active.get_mut() {
            metrics::gauge!("beam_streams_active", "endpoint" => self.endpoint.label())
                .decrement(1.0);
        }
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
pub type MediaDelivery = Delivery<SourceFileRanges>;

/// Resolve `file_id` to the file's on-disk path, its library's root and its
/// detected content type.
///
/// The caller must be signed in via the `beam_session` cookie (ADR-0003) -- a
/// `<video>` element sends that automatically, so there is no separate
/// stream-token step. Authentication itself is `SessionAuth` in the handler
/// signature; this only resolves the file.
async fn locate_file(state: &AppState, file_id: Uuid) -> Result<LocatedFile, DeliveryError> {
    match state.services.library.get_file_by_id(file_id).await {
        Ok(Some(file)) => Ok(file),
        Ok(None) => Err(DeliveryError::FileNotFound("File not found".into())),
        Err(err) => {
            error!(?err, "failed to look up file");
            Err(DeliveryError::Internal(ServerFault::logged(
                "Failed to look up file",
            )))
        }
    }
}

/// The file `file_id` names, if the caller may have it.
///
/// A grant resolved within the last few seconds is served from the access
/// service's stream grants (issue #462), so a player's run of Range requests
/// costs the database one policy read, one access check and one file lookup
/// rather than one of each per request. The session is not part of that:
/// `SessionAuth` has already resolved and touched it for this request.
async fn granted_file(
    state: &AppState,
    auth: &SessionAuth,
    file_id: Uuid,
) -> Result<LocatedFile, DeliveryError> {
    let user_id = auth.0.user_id;
    let stream_grants = state.services.access.stream_grants();
    let ticket = match stream_grants.map(|grants| grants.lookup(user_id, file_id)) {
        Some(Ok(file)) => return Ok(file),
        Some(Err(ticket)) => Some(ticket),
        None => None,
    };

    // A file of a title the caller may not see is one that does not exist
    // (issue #263).
    if hides_file(state, auth, file_id).await? {
        return Err(DeliveryError::FileNotFound("File not found".into()));
    }
    let file = locate_file(state, file_id).await?;

    if let (Some(grants), Some(ticket)) = (stream_grants, ticket) {
        grants.remember(ticket, user_id, file_id, file.clone());
    }
    Ok(file)
}

/// Whether the caller's access policy hides the file `file_id`. Shared with
/// subtitle delivery, which serves the sidecars of a file.
pub(crate) async fn hides_file(
    state: &AppState,
    auth: &SessionAuth,
    file_id: Uuid,
) -> Result<bool, DeliveryError> {
    // `caller_access` logged its cause where it built the fault, so it moves
    // across as is; the file lookup's own failure is logged here.
    let (_, policy) = caller_access(state, auth).await?;
    state
        .services
        .access
        .hides_file(&policy, file_id)
        .await
        .map_err(|err| DeliveryError::Internal(ServerFault::new(err)))
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
    auth: &SessionAuth,
    file_id: Uuid,
    conditions: &Conditions,
    attachment: bool,
) -> Result<MediaDelivery, DeliveryError> {
    let LocatedFile {
        id: _,
        path,
        library_root,
        mime_type,
    } = granted_file(state, auth, file_id).await?;
    let content_type = mime_type.unwrap_or_else(|| "application/octet-stream".to_owned());
    // Missing, or no longer a regular file reached without a link: either
    // way the file indexed is not there to serve.
    let (source, modified, length) = FileByteSource::open(library_root, path.clone())
        .await
        .map_err(|err| {
            error!(?path, ?err, "failed to open source file");
            DeliveryError::SourceFileMissing("Source video file not found".into())
        })?;

    let endpoint = if attachment {
        DeliveryEndpoint::Download
    } else {
        DeliveryEndpoint::Stream
    };
    let mut served = Served::<_, SourceFileRanges>::new(MeteredSource::new(source, endpoint))
        .content_type(&content_type)
        .etag(validator(modified, length))
        .last_modified(modified)
        .cache_control("private, max-age=3600");

    if attachment {
        // Kynos owns the RFC 6266 encoding, so the hand-written quote escaping
        // this replaces is gone along with it.
        served = served.attachment(download_filename(&path, file_id));
    }

    served.deliver(conditions).await.map_err(|err| {
        error!(?err, "failed to read source file span");
        DeliveryError::Internal(ServerFault::logged("Failed to read source file"))
    })
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
    auth: SessionAuth,
    Path(path): Path<FilePath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<MediaDelivery, DeliveryError> {
    deliver(&state, &auth, path.file_id, &conditions, false).await
}

/// The same fields with no body, for a player sizing the stream before it
/// starts.
#[kynos::head("/files/{file_id}/stream", tag = Playback, operation_id = "headStreamFile")]
#[tracing::instrument(skip_all)]
pub async fn head_stream_file(
    auth: SessionAuth,
    Path(path): Path<FilePath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<MediaDelivery, DeliveryError> {
    deliver(&state, &auth, path.file_id, &conditions, false).await
}

/// Download the full source file as an attachment. Same auth and Range
/// support as [`stream_file`] (so a paused/interrupted download can resume),
/// but sets `Content-Disposition: attachment` with the original filename so
/// the browser saves it rather than attempting inline playback.
#[kynos::get("/files/{file_id}/download", tag = Playback, operation_id = "downloadFile")]
#[tracing::instrument(skip_all)]
pub async fn download_file(
    auth: SessionAuth,
    Path(path): Path<FilePath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<MediaDelivery, DeliveryError> {
    deliver(&state, &auth, path.file_id, &conditions, true).await
}

/// The same fields with no body, for a client sizing the download first.
#[kynos::head("/files/{file_id}/download", tag = Playback, operation_id = "headDownloadFile")]
#[tracing::instrument(skip_all)]
pub async fn head_download_file(
    auth: SessionAuth,
    Path(path): Path<FilePath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<MediaDelivery, DeliveryError> {
    deliver(&state, &auth, path.file_id, &conditions, true).await
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod stream_tests;
