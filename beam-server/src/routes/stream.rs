use crate::state::AppState;
use salvo::oapi::{ToResponses, ToSchema};
use salvo::prelude::*;
use serde::Serialize;
use std::path::PathBuf;
use tokio::fs::File;
use tracing::{debug, error};

#[derive(Serialize, ToSchema)]
pub struct StreamTokenResponse {
    pub token: String,
}

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

// ── Error enums ───────────────────────────────────────────────────────────────

#[derive(ToResponses)]
pub enum GetStreamTokenError {
    /// Unauthorized
    #[salvo(response(status_code = 401))]
    Unauthorized(String),
    /// Stream not found
    #[salvo(response(status_code = 404))]
    NotFound(String),
    /// Internal server error
    #[salvo(response(status_code = 500))]
    InternalError(String),
}

#[async_trait]
impl Writer for GetStreamTokenError {
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
            Self::InternalError(msg) => {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Text::Plain(msg));
            }
        }
    }
}

#[derive(Debug, ToResponses)]
pub enum StreamMp4Error {
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
impl Writer for StreamMp4Error {
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

/// Get a presigned token for streaming
#[endpoint(
    tags("stream"),
    parameters(
        ("id" = String, description = "Stream ID"),
        ("Authorization" = String, Header, description = "Bearer <user JWT>")
    ),
)]
pub async fn get_stream_token(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<StreamTokenResponse>, GetStreamTokenError> {
    let state = depot.obtain::<AppState>().unwrap();
    let id: String = req.param::<String>("id").unwrap_or_default();

    // Validate user auth
    let user_id = if let Some(auth_header) = req.headers().get("Authorization")
        && let Ok(auth_str) = auth_header.to_str()
        && auth_str.starts_with("Bearer ")
    {
        let token = &auth_str[7..];
        state
            .services
            .auth
            .verify_token(token)
            .await
            .map(|user| user.user_id)
            .map_err(|_| GetStreamTokenError::Unauthorized("Invalid or expired token".into()))?
    } else {
        return Err(GetStreamTokenError::Unauthorized(
            "Missing Authorization header".into(),
        ));
    };

    // Verify the file exists before issuing a token
    match state.services.library.get_file_by_id(id.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Err(GetStreamTokenError::NotFound("Stream not found".into()));
        }
        Err(_) => {
            return Err(GetStreamTokenError::InternalError(
                "Failed to look up stream".into(),
            ));
        }
    }

    // Create stream token
    state
        .services
        .auth
        .create_stream_token(&user_id, &id)
        .map(|token| Json(StreamTokenResponse { token }))
        .map_err(|_| GetStreamTokenError::InternalError("Failed to create stream token".into()))
}

/// Direct-play stream via HTTP Range. Serves the source file's bytes exactly
/// as indexed on disk -- Beam never transcodes or remuxes media server-side
/// (see ADR-0004); the response `Content-Type` reflects the file's actual
/// detected MIME type rather than assuming MP4.
///
/// The route path and stream-token auth scheme here are interim: the
/// forthcoming REST API pass (direct-play/download endpoints, cookie-based
/// auth) will replace both.
#[endpoint(
    tags("media"),
    parameters(
        ("id" = String, description = "Stream ID"),
        ("Authorization" = String, Header, description = "Bearer <stream token> (alternative to ?token=)"),
        ("token" = String, Query, description = "Stream token (alternative to the Authorization header, required for <video> playback)"),
    ),
)]
#[tracing::instrument(skip_all)]
pub async fn stream_mp4(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), StreamMp4Error> {
    let state = depot.obtain::<AppState>().unwrap();
    let id: String = req.param::<String>("id").unwrap_or_default();

    // Accept the stream token from either the Authorization header (for
    // programmatic clients) or the ?token= query param (the <video> element
    // can't set custom headers).
    let header_token = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_owned);
    let token = header_token
        .or_else(|| req.queries().get("token").map(|s| s.to_string()))
        .ok_or_else(|| StreamMp4Error::Unauthorized("Missing stream token".into()))?;

    // Validate stream token
    match state.services.auth.verify_stream_token(&token) {
        Ok(stream_id) => {
            if stream_id != id {
                return Err(StreamMp4Error::Unauthorized(
                    "Token does not match stream ID".into(),
                ));
            }
        }
        Err(_) => {
            return Err(StreamMp4Error::Unauthorized(
                "Invalid or expired stream token".into(),
            ));
        }
    }

    debug!("Streaming media with ID: {}", id);

    // Look up the file by ID to get its actual path
    let file = match state.services.library.get_file_by_id(id.clone()).await {
        Ok(Some(f)) => f,
        Ok(None) => {
            return Err(StreamMp4Error::NotFound("File not found".into()));
        }
        Err(_) => {
            return Err(StreamMp4Error::InternalError(
                "Failed to look up file".into(),
            ));
        }
    };

    let source_video_path = PathBuf::from(&file.path);

    if !source_video_path.exists() {
        error!("Source video file not found: {:?}", source_video_path);
        return Err(StreamMp4Error::NotFound(
            "Source video file not found".into(),
        ));
    }

    let content_type = file
        .mime_type
        .clone()
        .unwrap_or_else(|| "application/octet-stream".to_string());

    // Serve the source file directly with range request support -- no
    // transcoding, no remuxing, no cache copy.
    serve_file_range(&source_video_path, &content_type, req, res).await
}

/// Serve a file with HTTP range request support, using the given content type.
async fn serve_file_range(
    file_path: &PathBuf,
    content_type: &str,
    req: &Request,
    res: &mut Response,
) -> Result<(), StreamMp4Error> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    // Get file metadata
    let file_metadata = match tokio::fs::metadata(file_path).await {
        Ok(metadata) => metadata,
        Err(err) => {
            error!("Failed to get file metadata: {:?}", err);
            return Err(StreamMp4Error::InternalError(
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
                return Err(StreamMp4Error::BadRequest("Invalid range header".into()));
            }
        };

        match parse_byte_range(range_str, file_size) {
            Ok((start, end)) => (start, end, StatusCode::PARTIAL_CONTENT),
            Err(RangeError::RangeNotSatisfiable { .. }) => {
                return Err(StreamMp4Error::RangeNotSatisfiable(
                    "Range not satisfiable".into(),
                ));
            }
            Err(_) => {
                return Err(StreamMp4Error::BadRequest(
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
            return Err(StreamMp4Error::InternalError("Failed to open file".into()));
        }
    };

    // Seek to start position if needed
    if start > 0
        && let Err(err) = file.seek(std::io::SeekFrom::Start(start)).await
    {
        error!("Failed to seek in file: {:?}", err);
        return Err(StreamMp4Error::InternalError(
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
        serve_file_range(&file_path, "video/mp4", &req, &mut res)
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

    // ── stream_mp4 handler tests ──────────────────────────────────────────

    mod handler_tests {
        use std::path::PathBuf;
        use std::sync::Arc;

        use beam_auth::utils::{
            repository::in_memory::InMemoryUserRepository,
            service::{AuthService, LocalAuthService},
            session_store::in_memory::InMemorySessionStore,
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

        const TEST_JWT_SECRET: &str = "test-stream-jwt-secret";
        const TEST_FILE_ID: &str = "test-file-id-123";

        struct TestContext {
            service: Service,
            auth: Arc<LocalAuthService>,
        }

        fn build_test_service() -> TestContext {
            let session_store = Arc::new(InMemorySessionStore::default());
            let user_repo = Arc::new(InMemoryUserRepository::default());
            let auth = Arc::new(LocalAuthService::new(
                user_repo.clone(),
                session_store,
                TEST_JWT_SECRET.to_string(),
            ));

            let notification = Arc::new(InMemoryNotificationService::new());
            let admin_log: Arc<dyn AdminLogService> = Arc::new(LocalAdminLogService::new(
                Arc::new(InMemoryAdminLogRepository::default()),
            ));

            let services = AppServices {
                auth: auth.clone(),
                hash: Arc::new(StubHashService),
                library: Arc::new(NotFoundLibraryService),
                metadata: Arc::new(StubMetadataService),
                notification,
                admin_log,
                user_repo: user_repo.clone(),
            };

            let config = crate::config::ServerConfig {
                bind_address: "0.0.0.0:8000".to_string(),
                server_url: "http://localhost:8000".to_string(),
                enable_metrics: false,
                video_dir: PathBuf::from("/tmp"),
                cache_dir: PathBuf::from("/tmp"),
                database_url: "postgres://unused:unused@localhost/unused".to_string(),
                jwt_secret: TEST_JWT_SECRET.to_string(),
                hash_unknown_files: true,
                scan_interval_secs: 3600,
                watch_enabled: false,
                watch_debounce_ms: 2000,
                enrich_interval_secs: 300,
                tmdb_api_token: None,
                anilist_enabled: false,
            };

            let state = AppState::new(config, services);
            let router = Router::new()
                .hoop(salvo::affix_state::inject(state))
                .push(Router::with_path("stream/mp4/{id}").get(super::super::stream_mp4));

            TestContext {
                service: Service::new(router),
                auth,
            }
        }

        fn stream_url(id: &str) -> String {
            format!("http://localhost/stream/mp4/{id}")
        }

        // ── Tests ─────────────────────────────────────────────────────────

        /// No Authorization header → 401.
        #[tokio::test]
        async fn test_rejects_missing_auth_header() {
            let ctx = build_test_service();
            let response = TestClient::get(stream_url(TEST_FILE_ID))
                .send(&ctx.service)
                .await;
            assert_eq!(response.status_code, Some(StatusCode::UNAUTHORIZED));
        }

        /// A valid stream token supplied as a query parameter passes auth.
        /// The library stub returns Ok(None) so the handler proceeds past auth
        /// and returns 404 — proving the token was accepted (any failure to
        /// authenticate would surface as 401, not 404).
        #[tokio::test]
        async fn test_accepts_query_param_token() {
            let ctx = build_test_service();
            let token = ctx
                .auth
                .create_stream_token("user-1", TEST_FILE_ID)
                .expect("token creation should succeed");
            let url = format!("{}?token={}", stream_url(TEST_FILE_ID), token);
            let response = TestClient::get(url).send(&ctx.service).await;
            assert_eq!(response.status_code, Some(StatusCode::NOT_FOUND));
        }

        /// Bearer token for a different stream ID → 401 (token/id mismatch).
        #[tokio::test]
        async fn test_rejects_mismatched_stream_id() {
            let ctx = build_test_service();
            let token = ctx
                .auth
                .create_stream_token("user-1", "different-file-id")
                .expect("token creation should succeed");
            let response = TestClient::get(stream_url(TEST_FILE_ID))
                .bearer_auth(token)
                .send(&ctx.service)
                .await;
            assert_eq!(response.status_code, Some(StatusCode::UNAUTHORIZED));
        }

        /// Token signed with a different secret → 401.
        #[tokio::test]
        async fn test_rejects_tampered_token() {
            let ctx = build_test_service();
            let rogue_auth = LocalAuthService::new(
                Arc::new(InMemoryUserRepository::default()),
                Arc::new(InMemorySessionStore::default()),
                "different-secret".to_string(),
            );
            let token = rogue_auth
                .create_stream_token("user-1", TEST_FILE_ID)
                .expect("token creation should succeed");
            let response = TestClient::get(stream_url(TEST_FILE_ID))
                .bearer_auth(token)
                .send(&ctx.service)
                .await;
            assert_eq!(response.status_code, Some(StatusCode::UNAUTHORIZED));
        }

        /// A valid Bearer token passes auth; the file not found in the library
        /// returns 404 — confirming the handler advanced past token validation.
        #[tokio::test]
        async fn test_valid_bearer_token_passes_auth() {
            let ctx = build_test_service();
            let token = ctx
                .auth
                .create_stream_token("user-1", TEST_FILE_ID)
                .expect("token creation should succeed");
            let response = TestClient::get(stream_url(TEST_FILE_ID))
                .bearer_auth(token)
                .send(&ctx.service)
                .await;
            // Token is valid → auth passes; library returns None → 404 (not 401).
            assert_eq!(response.status_code, Some(StatusCode::NOT_FOUND));
        }
    }
}
