use crate::routes::api_error::{ApiError, obtain_state, require_auth};
use salvo::oapi::ToResponses;
use salvo::prelude::*;
use std::path::PathBuf;
use tokio::fs::File;
use tracing::error;

#[derive(Debug, PartialEq)]
pub(crate) enum RangeError {
    MissingBytesPrefix,
    MalformedRange,
    NonNumericBound,
    RangeNotSatisfiable { start: u64, file_size: u64 },
}

/// Parse an HTTP Range header value against a known file size.
///
/// Returns `Ok((start, end))` where both are inclusive byte offsets,
/// or a `RangeError` describing the failure mode.
pub(crate) fn parse_byte_range(
    header_value: &str,
    file_size: u64,
) -> Result<(u64, u64), RangeError> {
    if file_size == 0 {
        return Err(RangeError::RangeNotSatisfiable {
            start: 0,
            file_size: 0,
        });
    }

    if !header_value.starts_with("bytes=") {
        return Err(RangeError::MissingBytesPrefix);
    }

    let range_part = &header_value[6..]; // strip "bytes="
    let dash_pos = range_part.find('-').ok_or(RangeError::MalformedRange)?;
    let start_str = &range_part[..dash_pos];
    let end_str = &range_part[dash_pos + 1..];

    if start_str.is_empty() && end_str.is_empty() {
        return Err(RangeError::MalformedRange);
    }

    let (start, end) = if start_str.is_empty() {
        // Suffix range: "bytes=-N" means the last N bytes
        let suffix = end_str
            .parse::<u64>()
            .map_err(|_| RangeError::NonNumericBound)?;
        let start = file_size.saturating_sub(suffix);
        (start, file_size - 1)
    } else {
        let start = start_str
            .parse::<u64>()
            .map_err(|_| RangeError::NonNumericBound)?;
        let end = if end_str.is_empty() {
            // Open-ended range: "bytes=N-"
            file_size - 1
        } else {
            let e = end_str
                .parse::<u64>()
                .map_err(|_| RangeError::NonNumericBound)?;
            std::cmp::min(e, file_size - 1)
        };
        (start, end)
    };

    if start > end || start >= file_size {
        return Err(RangeError::RangeNotSatisfiable { start, file_size });
    }

    Ok((start, end))
}

/// Escape a filename for use inside a `Content-Disposition` quoted-string
/// (RFC 6266 / RFC 2616 §2.2): backslash and double-quote are backslash-escaped,
/// and any control character (which would otherwise let a maliciously-named
/// source file inject extra header lines) is stripped outright.
fn sanitize_disposition_filename(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .flat_map(|c| match c {
            '"' | '\\' => vec!['\\', c],
            other => vec![other],
        })
        .collect()
}

// ── Error enums ───────────────────────────────────────────────────────────────

/// Errors shared by both file-delivery endpoints (`stream_file`, `download_file`).
#[derive(Debug, ToResponses)]
pub enum FileDeliveryError {
    /// Unauthorized
    #[salvo(response(status_code = 401))]
    Unauthorized(String),
    /// File not found
    #[salvo(response(status_code = 404))]
    NotFound(String),
    /// Bad request
    #[salvo(response(status_code = 400))]
    BadRequest(String),
    /// Range not satisfiable
    #[salvo(response(status_code = 416))]
    RangeNotSatisfiable(String),
    /// Internal server error
    #[salvo(response(status_code = 500))]
    InternalError(String),
}

#[async_trait]
impl Writer for FileDeliveryError {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Plain(msg));
            }
            Self::NotFound(msg) => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Text::Plain(msg));
            }
            Self::BadRequest(msg) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Text::Plain(msg));
            }
            Self::RangeNotSatisfiable(msg) => {
                res.status_code(StatusCode::RANGE_NOT_SATISFIABLE);
                res.render(Text::Plain(msg));
            }
            Self::InternalError(msg) => {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Text::Plain(msg));
            }
        }
    }
}

// ── Endpoints ─────────────────────────────────────────────────────────────────

/// Resolve `file_id` (path param) to the file's on-disk path and detected
/// content type, requiring the caller to be logged in via the
/// `beam_session` cookie (see ADR-0003) -- a `<video>` element sends that
/// cookie automatically, so there is no separate stream-token step anymore.
async fn authorize_and_locate_file(
    req: &Request,
    depot: &Depot,
    id: &str,
) -> Result<(PathBuf, String), FileDeliveryError> {
    let state = obtain_state(depot)
        .map_err(|_| FileDeliveryError::InternalError("Server state unavailable".into()))?;

    require_auth(req, state).await.map_err(|e| match e {
        ApiError::Unauthorized(msg) => FileDeliveryError::Unauthorized(msg),
        ApiError::BadRequest(msg) => FileDeliveryError::BadRequest(msg),
        ApiError::NotFound(msg) => FileDeliveryError::NotFound(msg),
        ApiError::Forbidden(msg) => FileDeliveryError::Unauthorized(msg),
        ApiError::Internal(msg) => FileDeliveryError::InternalError(msg),
    })?;

    let file = match state.services.library.get_file_by_id(id.to_string()).await {
        Ok(Some(f)) => f,
        Ok(None) => {
            return Err(FileDeliveryError::NotFound("File not found".into()));
        }
        Err(_) => {
            return Err(FileDeliveryError::InternalError(
                "Failed to look up file".into(),
            ));
        }
    };

    let source_video_path = PathBuf::from(&file.path);

    if !source_video_path.exists() {
        error!("Source video file not found: {:?}", source_video_path);
        return Err(FileDeliveryError::NotFound(
            "Source video file not found".into(),
        ));
    }

    let content_type = file
        .mime_type
        .clone()
        .unwrap_or_else(|| "application/octet-stream".to_string());

    Ok((source_video_path, content_type))
}

/// Direct-play stream via HTTP Range. Serves the source file's bytes exactly
/// as indexed on disk -- Beam never transcodes or remuxes media server-side
/// (see ADR-0004); the response `Content-Type` reflects the file's actual
/// detected MIME type rather than assuming MP4. Rendered inline (no
/// `Content-Disposition`) so a `<video>` element plays it in place.
#[endpoint(
    tags("media"),
    parameters(("file_id" = String, description = "File ID")),
)]
#[tracing::instrument(skip_all)]
pub async fn stream_file(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), FileDeliveryError> {
    let id: String = req.param::<String>("file_id").unwrap_or_default();
    let (path, content_type) = authorize_and_locate_file(req, depot, &id).await?;
    serve_file_range(&path, &content_type, None, req, res).await
}

/// Download the full source file as an attachment. Same auth and Range
/// support as [`stream_file`] (so a paused/interrupted download can resume),
/// but sets `Content-Disposition: attachment` with the original filename so
/// the browser saves it rather than attempting inline playback.
#[endpoint(
    tags("media"),
    parameters(("file_id" = String, description = "File ID")),
)]
#[tracing::instrument(skip_all)]
pub async fn download_file(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), FileDeliveryError> {
    let id: String = req.param::<String>("file_id").unwrap_or_default();
    let (path, content_type) = authorize_and_locate_file(req, depot, &id).await?;
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{id}.bin"));
    serve_file_range(&path, &content_type, Some(filename), req, res).await
}

/// Serve a file with HTTP range request support, using the given content type.
/// `attachment_filename` set to `Some(name)` sends
/// `Content-Disposition: attachment; filename="name"` (download); `None`
/// leaves the disposition unset, so browsers render/play the response inline.
async fn serve_file_range(
    file_path: &PathBuf,
    content_type: &str,
    attachment_filename: Option<String>,
    req: &Request,
    res: &mut Response,
) -> Result<(), FileDeliveryError> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    // Get file metadata
    let file_metadata = match tokio::fs::metadata(file_path).await {
        Ok(metadata) => metadata,
        Err(err) => {
            error!("Failed to get file metadata: {:?}", err);
            return Err(FileDeliveryError::InternalError(
                "Failed to get file metadata".into(),
            ));
        }
    };

    let file_size = file_metadata.len();

    // Handle range requests
    let range = req.headers().get("range");
    let (start, end, status_code) = if let Some(range_header) = range {
        let range_str = match range_header.to_str() {
            Ok(s) => s,
            Err(_) => {
                return Err(FileDeliveryError::BadRequest("Invalid range header".into()));
            }
        };

        match parse_byte_range(range_str, file_size) {
            Ok((start, end)) => (start, end, StatusCode::PARTIAL_CONTENT),
            Err(RangeError::RangeNotSatisfiable { .. }) => {
                return Err(FileDeliveryError::RangeNotSatisfiable(
                    "Range not satisfiable".into(),
                ));
            }
            Err(_) => {
                return Err(FileDeliveryError::BadRequest(
                    "Invalid range specification".into(),
                ));
            }
        }
    } else {
        (0, file_size - 1, StatusCode::OK)
    };

    // Open file and seek to start position
    let mut file = match File::open(file_path).await {
        Ok(f) => f,
        Err(err) => {
            error!("Failed to open file: {:?}", err);
            return Err(FileDeliveryError::InternalError(
                "Failed to open file".into(),
            ));
        }
    };

    // Seek to start position if needed
    if start > 0
        && let Err(err) = file.seek(std::io::SeekFrom::Start(start)).await
    {
        error!("Failed to seek in file: {:?}", err);
        return Err(FileDeliveryError::InternalError(
            "Failed to seek in file".into(),
        ));
    }

    let content_length = end - start + 1;

    // Build response
    res.status_code(status_code);
    res.headers_mut()
        .insert("Content-Type", content_type.parse().unwrap());
    res.headers_mut().insert(
        "Content-Length",
        content_length.to_string().parse().unwrap(),
    );
    res.headers_mut()
        .insert("Accept-Ranges", "bytes".parse().unwrap());

    if let Some(filename) = &attachment_filename {
        let escaped = sanitize_disposition_filename(filename);
        res.headers_mut().insert(
            "Content-Disposition",
            format!("attachment; filename=\"{escaped}\"")
                .parse()
                .unwrap(),
        );
    }

    // Add range headers for partial content
    if status_code == StatusCode::PARTIAL_CONTENT {
        res.headers_mut().insert(
            "Content-Range",
            format!("bytes {}-{}/{}", start, end, file_size)
                .parse()
                .unwrap(),
        );
    }

    // Add cache headers for better performance
    res.headers_mut()
        .insert("Cache-Control", "public, max-age=3600".parse().unwrap());
    res.headers_mut()
        .insert("ETag", format!("\"{}\"", file_size).parse().unwrap()); // Simple ETag based on file size

    // Stream the range lazily in chunks to avoid buffering the entire range in memory.
    let chunk_size = 128 * 1024usize;
    let stream = async_stream::stream! {
        let mut remaining = content_length as usize;
        while remaining > 0 {
            let to_read = chunk_size.min(remaining);
            let mut buf = vec![0u8; to_read];
            match file.read_exact(&mut buf).await {
                Ok(_) => {
                    remaining -= to_read;
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(buf));
                }
                Err(e) => {
                    yield Err(e);
                    break;
                }
            }
        }
    };
    res.body(salvo::http::body::ResBody::stream(stream));

    Ok(())
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod stream_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use salvo::test::ResponseExt;

    /// Verify that `serve_file_range` streams a requested range correctly and does not
    /// regress to a single-buffer approach. A 1 MB file is created and only the first
    /// 1 024 bytes are requested; the response body must be exactly 1 024 bytes.
    #[tokio::test]
    async fn test_serve_file_range_body_length() {
        use std::io::Write;

        // Write 1 MB of patterned data to a temp file.
        let mut tmp = tempfile::NamedTempFile::new().expect("create tempfile");
        let data: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024).collect();
        tmp.write_all(&data).expect("write tempfile");
        tmp.flush().expect("flush tempfile");

        let file_path = PathBuf::from(tmp.path());

        // Build a minimal Salvo request with a range header.
        let mut req = salvo::Request::new();
        req.headers_mut()
            .insert("range", "bytes=0-1023".parse().unwrap());

        let mut res = salvo::Response::new();
        serve_file_range(&file_path, "video/mp4", None, &req, &mut res)
            .await
            .expect("serve_file_range should succeed");

        assert_eq!(
            res.status_code,
            Some(salvo::http::StatusCode::PARTIAL_CONTENT)
        );

        let body = res.take_bytes(None).await.expect("collect body");
        assert_eq!(body.len(), 1024, "response body must be exactly 1024 bytes");
        assert_eq!(&body[..], &data[..1024], "response body content must match");
    }

    #[tokio::test]
    async fn test_serve_file_range_attachment_sets_content_disposition() {
        use std::io::Write;

        let mut tmp = tempfile::NamedTempFile::new().expect("create tempfile");
        tmp.write_all(b"hello world").expect("write tempfile");
        tmp.flush().expect("flush tempfile");
        let file_path = PathBuf::from(tmp.path());

        let req = salvo::Request::new();
        let mut res = salvo::Response::new();
        serve_file_range(
            &file_path,
            "video/mp4",
            Some("My Movie (2024).mkv".to_string()),
            &req,
            &mut res,
        )
        .await
        .expect("serve_file_range should succeed");

        assert_eq!(
            res.headers()
                .get("Content-Disposition")
                .and_then(|v| v.to_str().ok()),
            Some("attachment; filename=\"My Movie (2024).mkv\"")
        );
    }

    #[test]
    fn test_sanitize_disposition_filename_escapes_quotes_and_backslashes() {
        assert_eq!(
            sanitize_disposition_filename(r#"weird"name\here.mkv"#),
            r#"weird\"name\\here.mkv"#
        );
    }

    #[test]
    fn test_sanitize_disposition_filename_strips_control_characters() {
        assert_eq!(
            sanitize_disposition_filename("evil\r\nSet-Cookie: pwned=1"),
            "evilSet-Cookie: pwned=1"
        );
    }

    // ── parse_byte_range unit tests ───────────────────────────────────────

    #[test]
    fn test_basic_range() {
        assert_eq!(parse_byte_range("bytes=0-499", 1000), Ok((0, 499)));
    }

    #[test]
    fn test_range_end_at_last_byte() {
        assert_eq!(parse_byte_range("bytes=0-999", 1000), Ok((0, 999)));
    }

    #[test]
    fn test_open_ended_range() {
        assert_eq!(parse_byte_range("bytes=1000-", 5000), Ok((1000, 4999)));
    }

    #[test]
    fn test_suffix_range() {
        assert_eq!(parse_byte_range("bytes=-500", 1000), Ok((500, 999)));
    }

    #[test]
    fn test_suffix_range_larger_than_file_clamps_to_start() {
        assert_eq!(parse_byte_range("bytes=-1500", 1000), Ok((0, 999)));
    }

    #[test]
    fn test_start_greater_than_end_is_not_satisfiable() {
        assert_eq!(
            parse_byte_range("bytes=500-400", 1000),
            Err(RangeError::RangeNotSatisfiable {
                start: 500,
                file_size: 1000
            })
        );
    }

    #[test]
    fn test_start_beyond_file_size_is_not_satisfiable() {
        assert_eq!(
            parse_byte_range("bytes=2000-2500", 1000),
            Err(RangeError::RangeNotSatisfiable {
                start: 2000,
                file_size: 1000
            })
        );
    }

    #[test]
    fn test_end_beyond_file_size_is_clamped() {
        assert_eq!(parse_byte_range("bytes=0-2000", 1000), Ok((0, 999)));
    }

    #[test]
    fn test_missing_bytes_prefix() {
        assert_eq!(
            parse_byte_range("invalid=0-100", 1000),
            Err(RangeError::MissingBytesPrefix)
        );
    }

    #[test]
    fn test_non_numeric_bounds() {
        assert_eq!(
            parse_byte_range("bytes=abc-def", 1000),
            Err(RangeError::NonNumericBound)
        );
    }

    #[test]
    fn test_no_dash_is_malformed() {
        assert_eq!(
            parse_byte_range("bytes=0", 1000),
            Err(RangeError::MalformedRange)
        );
    }

    #[test]
    fn test_empty_range_spec_is_malformed() {
        assert_eq!(
            parse_byte_range("bytes=", 1000),
            Err(RangeError::MalformedRange)
        );
    }

    #[test]
    fn test_zero_file_size_is_not_satisfiable() {
        assert_eq!(
            parse_byte_range("bytes=0-0", 0),
            Err(RangeError::RangeNotSatisfiable {
                start: 0,
                file_size: 0
            })
        );
    }

    // ── stream_file / download_file handler tests ─────────────────────────

    mod handler_tests {
        use std::path::PathBuf;
        use std::sync::Arc;

        use beam_auth::server::OidcRuntimeConfig;
        use beam_auth::utils::{
            models::CreateUser,
            oidc::NotConfiguredOidcClient,
            pending_auth_store::in_memory::InMemoryPendingAuthStore,
            repository::{UserRepository, in_memory::InMemoryUserRepository},
            session_store::{SessionData, SessionStore, in_memory::InMemorySessionStore},
        };
        use salvo::prelude::*;
        use salvo::test::TestClient;

        use crate::services::admin_log::{AdminLogService, LocalAdminLogService};
        use crate::services::hash::HashService;
        use crate::services::library::{LibraryError, LibraryService};
        use crate::services::metadata::{
            MediaConnection, MediaFilter, MediaSearchFilters, MediaSortField, MetadataError,
            MetadataService, PageInfo, SortOrder,
        };
        use crate::services::notification::InMemoryNotificationService;
        use crate::services::playback::{
            ContinueWatchingItem, PlaybackError, PlaybackProgressDto, PlaybackService,
        };
        use crate::state::{AppServices, AppState};
        use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;

        // ── Stubs ─────────────────────────────────────────────────────────

        #[derive(Debug)]
        struct StubHashService;

        #[async_trait::async_trait]
        impl HashService for StubHashService {
            fn hash_sync(&self, _: &std::path::Path) -> std::io::Result<u64> {
                unimplemented!("not called in stream handler tests")
            }
            async fn hash_async(&self, _: PathBuf) -> std::io::Result<u64> {
                unimplemented!("not called in stream handler tests")
            }
        }

        #[derive(Debug)]
        struct StubMetadataService;

        #[async_trait::async_trait]
        impl MetadataService for StubMetadataService {
            async fn get_media_metadata(&self, _: &str) -> Option<crate::models::MediaMetadata> {
                None
            }
            async fn search_media(
                &self,
                _: Option<u32>,
                _: Option<String>,
                _: Option<u32>,
                _: Option<String>,
                _: MediaSortField,
                _: SortOrder,
                _: MediaSearchFilters,
            ) -> MediaConnection {
                MediaConnection {
                    edges: vec![],
                    page_info: PageInfo {
                        has_next_page: false,
                        has_previous_page: false,
                        start_cursor: None,
                        end_cursor: None,
                    },
                }
            }
            async fn refresh_metadata(&self, _: MediaFilter) -> Result<(), MetadataError> {
                Ok(())
            }

            async fn get_media_sources(
                &self,
                _media_id: &str,
            ) -> Result<Vec<crate::models::MediaSource>, MetadataError> {
                unimplemented!("not called in stream route tests")
            }
        }

        #[derive(Debug)]
        struct StubPlaybackService;

        #[async_trait::async_trait]
        impl PlaybackService for StubPlaybackService {
            async fn report_progress(
                &self,
                _user_id: uuid::Uuid,
                _file_id: uuid::Uuid,
                _position_secs: f64,
                _duration_secs: Option<f64>,
            ) -> Result<PlaybackProgressDto, PlaybackError> {
                unimplemented!("not called in stream handler tests")
            }

            async fn get_continue_watching(
                &self,
                _user_id: uuid::Uuid,
                _limit: u32,
            ) -> Result<Vec<ContinueWatchingItem>, PlaybackError> {
                unimplemented!("not called in stream handler tests")
            }

            async fn get_history(
                &self,
                _user_id: uuid::Uuid,
                _limit: u64,
                _offset: u64,
            ) -> Result<(Vec<crate::services::playback::HistoryItem>, u64), PlaybackError>
            {
                unimplemented!("not called in stream handler tests")
            }
        }

        /// Library stub that always returns `Ok(None)` for file lookups so token
        /// validation is exercised without touching the filesystem.
        #[derive(Debug)]
        struct NotFoundLibraryService;

        #[async_trait::async_trait]
        impl LibraryService for NotFoundLibraryService {
            async fn get_libraries(
                &self,
                _: String,
            ) -> Result<Vec<crate::models::Library>, LibraryError> {
                unimplemented!()
            }
            async fn get_library_by_id(
                &self,
                _: String,
            ) -> Result<Option<crate::models::Library>, LibraryError> {
                unimplemented!()
            }
            async fn get_library_files(
                &self,
                _: String,
            ) -> Result<Vec<crate::models::LibraryFile>, LibraryError> {
                unimplemented!()
            }
            async fn create_library(
                &self,
                _: String,
                _: String,
            ) -> Result<crate::models::Library, LibraryError> {
                unimplemented!()
            }
            async fn scan_library(&self, _: String) -> Result<u32, LibraryError> {
                unimplemented!()
            }
            async fn delete_library(&self, _: String) -> Result<bool, LibraryError> {
                unimplemented!()
            }
            async fn get_file_by_id(
                &self,
                _: String,
            ) -> Result<Option<crate::models::LibraryFile>, LibraryError> {
                Ok(None)
            }
        }

        // ── Test helpers ──────────────────────────────────────────────────

        const TEST_FILE_ID: &str = "test-file-id-123";

        struct TestContext {
            service: Service,
            session_store: Arc<InMemorySessionStore>,
            user_repo: Arc<InMemoryUserRepository>,
        }

        fn build_test_service() -> TestContext {
            let session_store = Arc::new(InMemorySessionStore::default());
            let user_repo = Arc::new(InMemoryUserRepository::default());

            let notification = Arc::new(InMemoryNotificationService::new());
            let admin_log: Arc<dyn AdminLogService> = Arc::new(LocalAdminLogService::new(
                Arc::new(InMemoryAdminLogRepository::default()),
            ));

            let services = AppServices {
                hash: Arc::new(StubHashService),
                library: Arc::new(NotFoundLibraryService),
                metadata: Arc::new(StubMetadataService),
                notification,
                admin_log,
                user_repo: user_repo.clone(),
                playback: Arc::new(StubPlaybackService),
                genre_repo: Arc::new(
                    beam_domain::repositories::genre::in_memory::InMemoryGenreRepository::default(),
                ),
                session_store: session_store.clone(),
                oidc_client: Arc::new(NotConfiguredOidcClient::new("not used in these tests")),
                pending_auth_store: Arc::new(InMemoryPendingAuthStore::default()),
                oidc_config: OidcRuntimeConfig {
                    web_url: "http://localhost:5173".to_string(),
                    cookie_secure: false,
                    admin_emails_csv: String::new(),
                    session_idle_days: 14,
                    session_max_days: 60,
                },
            };

            let config = crate::config::ServerConfig {
                bind_address: "0.0.0.0:8000".to_string(),
                server_url: "http://localhost:8000".to_string(),
                enable_metrics: false,
                shutdown_timeout_secs: 30,
                video_dir: PathBuf::from("/tmp"),
                data_dir: PathBuf::from("/tmp"),
                database_url: "postgres://unused:unused@localhost/unused".to_string(),
                auto_migrate: true,
                db_max_connections: 20,
                db_min_connections: 5,
                hash_unknown_files: true,
                scan_interval_secs: 3600,
                watch_enabled: false,
                watch_debounce_ms: 2000,
                enrich_interval_secs: 300,
                enrich_batch_size: 25,
                enrich_min_confidence: 0.7,
                tmdb_api_token: None,
                anilist_enabled: false,
                metadata_language: None,
                oidc_issuer: None,
                oidc_client_id: None,
                oidc_client_secret: None,
                oidc_scopes: "openid profile email".to_string(),
                web_url: "http://localhost:5173".to_string(),
                extra_allowed_origins: None,
                admin_emails: None,
                cookie_secure: Some(false),
                session_idle_days: 14,
                session_max_days: 60,
                rate_limit_enabled: true,
                rate_limit_auth_per_minute: 10,
                rate_limit_search_per_minute: 60,
                rate_limit_trust_forwarded_for: false,
            };

            let state = AppState::new(
                config,
                services,
                Arc::new(crate::services::health::InMemoryDependencyProbe::healthy()),
            );
            let router = Router::new()
                .hoop(salvo::affix_state::inject(state))
                .push(Router::with_path("files/{file_id}/stream").get(super::super::stream_file));

            TestContext {
                service: Service::new(router),
                session_store,
                user_repo,
            }
        }

        /// Seeds a user + session directly (bypassing the OIDC login flow,
        /// which isn't under test here) and returns a `Cookie` header value.
        async fn seed_session_cookie(ctx: &TestContext) -> String {
            let user = ctx
                .user_repo
                .create(CreateUser {
                    oidc_issuer: "https://test.example".to_string(),
                    oidc_subject: "subj-1".to_string(),
                    email: Some("test@example.com".to_string()),
                    display_name: "Test User".to_string(),
                    avatar_url: None,
                    is_admin: false,
                })
                .await
                .expect("seed user should succeed");

            let token = ctx
                .session_store
                .create(
                    &SessionData {
                        user_id: user.id.to_string(),
                        device_hash: "test-device".to_string(),
                        ip: "127.0.0.1".to_string(),
                        created_at: chrono::Utc::now().timestamp(),
                        last_active: chrono::Utc::now().timestamp(),
                    },
                    86400,
                    86400,
                )
                .await
                .expect("seed session should succeed");

            format!("beam_session={token}")
        }

        fn stream_url(id: &str) -> String {
            format!("http://localhost/files/{id}/stream")
        }

        // ── Tests ─────────────────────────────────────────────────────────

        /// No session cookie → 401.
        #[tokio::test]
        async fn test_rejects_missing_session_cookie() {
            let ctx = build_test_service();
            let response = TestClient::get(stream_url(TEST_FILE_ID))
                .send(&ctx.service)
                .await;
            assert_eq!(response.status_code, Some(StatusCode::UNAUTHORIZED));
        }

        /// A garbage cookie value (no matching session) → 401.
        #[tokio::test]
        async fn test_rejects_unknown_session_cookie() {
            let ctx = build_test_service();
            let response = TestClient::get(stream_url(TEST_FILE_ID))
                .add_header("Cookie", "beam_session=not-a-real-session", true)
                .send(&ctx.service)
                .await;
            assert_eq!(response.status_code, Some(StatusCode::UNAUTHORIZED));
        }

        /// A valid session cookie passes auth; the file not found in the
        /// library returns 404 — confirming the handler advanced past the
        /// session check (any auth failure would be 401, not 404).
        #[tokio::test]
        async fn test_valid_session_cookie_passes_auth() {
            let ctx = build_test_service();
            let cookie = seed_session_cookie(&ctx).await;
            let response = TestClient::get(stream_url(TEST_FILE_ID))
                .add_header("Cookie", cookie, true)
                .send(&ctx.service)
                .await;
            assert_eq!(response.status_code, Some(StatusCode::NOT_FOUND));
        }
    }
}
