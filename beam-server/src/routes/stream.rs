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
use kynos::extract::media::MediaType;
use kynos::http::etag::ETag;
use kynos::prelude::*;
use kynos::response::range::served::{Conditions, Delivery, Served};
use kynos::response::range::source::ByteSource;
use kynos::response::{IntoResponse, Responses};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::error;

use crate::routes::api_error::{DeliveryError, SessionAuth};
use crate::routes::tags::Playback;
use crate::state::AppState;

/// What both delivery endpoints capture.
#[derive(Debug, Schema, PathParams)]
pub struct FilePath {
    /// File ID.
    pub file_id: String,
}

/// One indexed file on disk, read a span at a time.
///
/// A trait rather than a path is what lets the tests keep a fake filesystem:
/// `ByteSource` has two methods and neither mentions `std::fs`, so an in-memory
/// source stands in without special runtime infrastructure. Kynos ships
/// `InMemory(Bytes)` for exactly that.
pub struct FileByteSource {
    path: PathBuf,
    length: u64,
}

impl FileByteSource {
    /// Reads the file's metadata without reading a byte of its contents.
    async fn open(path: PathBuf) -> Result<(Self, SystemTime, u64), DeliveryError> {
        let metadata = tokio::fs::metadata(&path).await.map_err(|err| {
            error!(?path, ?err, "failed to read source file metadata");
            DeliveryError::NotFound("Source video file not found".into())
        })?;

        let length = metadata.len();
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        Ok((Self { path, length }, modified, length))
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
        let mut file = tokio::fs::File::open(&self.path).await?;
        file.seek(std::io::SeekFrom::Start(first)).await?;

        let span = usize::try_from(last - first + 1).unwrap_or(0);
        let mut buffer = vec![0u8; span];
        file.read_exact(&mut buffer).await?;

        Ok(Bytes::from(buffer))
    }
}

/// The media type the range engine is parameterised by.
///
/// Beam serves whatever the indexer detected, which is decided per file and
/// cannot be a `const`. [`MediaDelivery`] therefore overrides the header at
/// request time and describes the range of types honestly.
struct AnyMedia;

impl MediaType for AnyMedia {
    const MEDIA_TYPE: &'static str = "application/octet-stream";
}

/// A ranged delivery whose `Content-Type` is the file's, not the type
/// parameter's.
///
/// `Delivery<M>` carries its media type at the type level so it can describe
/// itself, which is right for an operation serving one representation and wrong
/// for a media server: Beam's content type comes from the file being read. This
/// wrapper keeps Kynos's range engine and replaces only that header, then
/// describes the media-type ranges Beam actually answers with.
///
/// Nothing here is an escape hatch. `IntoResponse` and `Responses` are the
/// public traits every response type implements, and `delivery_responses` is
/// public precisely so a wrapper can build on it -- the description stays
/// accurate rather than being waived, and the `unchecked` feature stays off.
///
/// It is still a gap worth closing upstream: a ranged delivery whose media type
/// is chosen at request time is a normal thing for a file server to want, and
/// Kynos has no way to say it. Recorded in
/// `docs/architecture/kynos-migration-readiness.md`.
pub struct MediaDelivery {
    inner: Delivery<AnyMedia>,
    content_type: String,
}

impl MediaDelivery {
    const fn new(inner: Delivery<AnyMedia>, content_type: String) -> Self {
        Self {
            inner,
            content_type,
        }
    }
}

impl IntoResponse for MediaDelivery {
    fn into_response(self) -> kynos::http::Response {
        let Self {
            inner,
            content_type,
        } = self;

        let status = inner.status();
        let mut response = inner.into_response();

        // Only the two statuses that actually carry the media bytes get the
        // media's content type. Everything else keeps whatever the range engine
        // labelled it.
        //
        // The check used to be `!= 304`, which was right about 304 -- a
        // response with no representation carries no content type, RFC 9110
        // section 15.4.5 -- and wrong about everything else the engine can
        // produce. A `Range` that cannot be satisfied answers 416 with an
        // RFC 9457 problem document, and that was being relabelled
        // `video/mp4`: a JSON body announced as video, which a client is
        // entitled to fail on. Naming the two statuses that mean "here is the
        // file" states the rule positively, so a status added later is
        // excluded by default rather than mislabelled by default.
        if matches!(
            status,
            kynos::http::StatusCode::OK | kynos::http::StatusCode::PARTIAL_CONTENT
        ) && let Ok(value) = content_type.parse()
        {
            response
                .headers_mut()
                .insert(kynos::http::header::CONTENT_TYPE, value);
        }

        response
    }
}

impl Responses for MediaDelivery {
    /// The same 200/206/304 shape `Delivery` describes, widened to the media
    /// types Beam can answer with.
    ///
    /// `video/*` and `audio/*` are media-type ranges, which OpenAPI permits as
    /// `content` keys; `application/octet-stream` is what an unrecognised
    /// container falls back to. Listing all three is the honest description of
    /// "whatever the indexer detected".
    fn responses(registry: &mut kynos::schema::registry::Registry) -> kynos::openapi::Responses {
        use kynos::openapi::{MediaType as OpenApiMediaType, RefOr, Schema, StatusPattern};

        let _ = registry;
        let mut responses = kynos::response::range::delivery_responses("video/*");

        for status in [200, 206] {
            if let Some(RefOr::Item(response)) = responses
                .responses
                .get_mut(&StatusPattern::Code(status).to_string())
            {
                for media_type in ["audio/*", "application/octet-stream"] {
                    response.content.insert(
                        media_type.to_owned(),
                        OpenApiMediaType::new(Schema::Object(Box::default())),
                    );
                }
            }
        }

        responses
    }
}

/// Resolve `file_id` to the file's on-disk path and detected content type.
///
/// The caller must be signed in via the `beam_session` cookie (ADR-0003) -- a
/// `<video>` element sends that automatically, so there is no separate
/// stream-token step. Authentication itself is `SessionAuth` in the handler
/// signature; this only resolves the file.
async fn locate_file(state: &AppState, file_id: &str) -> Result<(PathBuf, String), DeliveryError> {
    let file = match state
        .services
        .library
        .get_file_by_id(file_id.to_owned())
        .await
    {
        Ok(Some(file)) => file,
        Ok(None) => return Err(DeliveryError::NotFound("File not found".into())),
        Err(err) => {
            error!(?err, "failed to look up file");
            return Err(DeliveryError::Internal("Failed to look up file".into()));
        }
    };

    let path = PathBuf::from(&file.path);
    if !path.exists() {
        error!(?path, "source video file not found");
        return Err(DeliveryError::NotFound(
            "Source video file not found".into(),
        ));
    }

    let content_type = file
        .mime_type
        .clone()
        .unwrap_or_else(|| "application/octet-stream".to_owned());

    Ok((path, content_type))
}

/// A validator that changes whenever the bytes do.
///
/// Modification time and size, which is the shape nginx mints and is strong
/// enough for `If-Range` to mean something: a re-index that rewrites a file
/// moves its mtime, and a different file of the same size has a different one.
/// The previous `"{file_size}"` was neither -- every 4 GiB remux shared it, so
/// a resumed download could splice bytes from a different file.
fn validator(modified: SystemTime, length: u64) -> ETag {
    let stamp = modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    ETag::strong(format!("{stamp:x}-{length:x}"))
}

/// Builds the delivery both endpoints share.
async fn deliver(
    state: &AppState,
    file_id: &str,
    conditions: &Conditions,
    attachment: bool,
) -> Result<MediaDelivery, DeliveryError> {
    let (path, content_type) = locate_file(state, file_id).await?;
    let (source, modified, length) = FileByteSource::open(path.clone()).await?;

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
fn download_filename(path: &FsPath, file_id: &str) -> String {
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
    deliver(&state, &path.file_id, &conditions, false).await
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
    deliver(&state, &path.file_id, &conditions, false).await
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
    deliver(&state, &path.file_id, &conditions, true).await
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
    deliver(&state, &path.file_id, &conditions, true).await
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod stream_tests;
