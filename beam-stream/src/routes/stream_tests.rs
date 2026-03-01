/// Subcutaneous HTTP tests for the stream REST routes.
///
/// These tests spin up the full Salvo service with in-memory implementations for all
/// external dependencies — no Redis, no PostgreSQL, no real ffmpeg invocation required.
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use beam_auth::utils::{
        repository::in_memory::InMemoryUserRepository,
        service::{AuthService, LocalAuthService},
        session_store::in_memory::InMemorySessionStore,
    };
    use salvo::prelude::*;
    use salvo::test::{ResponseExt, TestClient};
    use serde_json::Value;
    use tempfile::TempDir;

    use crate::models::{FileContentType, FileIndexStatus, LibraryFile};
    use crate::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
    use crate::routes::{get_stream_token, stream_mp4};
    use crate::services::admin_log::{AdminLogService, LocalAdminLogService};
    use crate::services::hash::HashService;
    use crate::services::library::{LibraryError, LibraryService};
    use crate::services::metadata::{
        MediaConnection, MediaFilter, MediaSearchFilters, MediaSortField, MetadataError,
        MetadataService, PageInfo, SortOrder,
    };
    use crate::services::notification::InMemoryNotificationService;
    use crate::services::transcode::TranscodeService;
    use crate::state::{AppServices, AppState};

    // ─── Constants ────────────────────────────────────────────────────────────

    const TEST_JWT_SECRET: &str = "test-jwt-secret-for-stream-route-tests";
    const TEST_FILE_ID: &str = "11111111-1111-1111-1111-111111111111";

    // ─── Stub service implementations ─────────────────────────────────────────

    #[derive(Debug)]
    struct StubHashService;

    #[async_trait::async_trait]
    impl HashService for StubHashService {
        fn hash_sync(&self, _path: &std::path::Path) -> std::io::Result<u64> {
            unimplemented!("not called in stream route tests")
        }
        async fn hash_async(&self, _path: PathBuf) -> std::io::Result<u64> {
            unimplemented!("not called in stream route tests")
        }
    }

    #[derive(Debug)]
    struct StubMetadataService;

    #[async_trait::async_trait]
    impl MetadataService for StubMetadataService {
        async fn get_media_metadata(
            &self,
            _media_id: &str,
        ) -> Option<crate::models::MediaMetadata> {
            None
        }

        async fn search_media(
            &self,
            _first: Option<u32>,
            _after: Option<String>,
            _last: Option<u32>,
            _before: Option<String>,
            _sort_by: MediaSortField,
            _sort_order: SortOrder,
            _filters: MediaSearchFilters,
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

        async fn refresh_metadata(&self, _filter: MediaFilter) -> Result<(), MetadataError> {
            Ok(())
        }
    }

    /// Stub library service backed by a fixed list of files.
    ///
    /// Only `get_file_by_id` is exercised by the stream routes; all other methods
    /// are left `unimplemented!`.
    #[derive(Debug, Clone)]
    struct StubLibraryService {
        files: Vec<LibraryFile>,
    }

    impl StubLibraryService {
        fn new(files: Vec<LibraryFile>) -> Self {
            Self { files }
        }
    }

    #[async_trait::async_trait]
    impl LibraryService for StubLibraryService {
        async fn get_libraries(
            &self,
            _user_id: String,
        ) -> Result<Vec<crate::models::Library>, LibraryError> {
            unimplemented!("not called in stream route tests")
        }
        async fn get_library_by_id(
            &self,
            _library_id: String,
        ) -> Result<Option<crate::models::Library>, LibraryError> {
            unimplemented!("not called in stream route tests")
        }
        async fn get_library_files(
            &self,
            _library_id: String,
        ) -> Result<Vec<LibraryFile>, LibraryError> {
            unimplemented!("not called in stream route tests")
        }
        async fn create_library(
            &self,
            _name: String,
            _root_path: String,
        ) -> Result<crate::models::Library, LibraryError> {
            unimplemented!("not called in stream route tests")
        }
        async fn scan_library(&self, _library_id: String) -> Result<u32, LibraryError> {
            unimplemented!("not called in stream route tests")
        }
        async fn delete_library(&self, _library_id: String) -> Result<bool, LibraryError> {
            unimplemented!("not called in stream route tests")
        }
        async fn get_file_by_id(
            &self,
            file_id: String,
        ) -> Result<Option<LibraryFile>, LibraryError> {
            Ok(self.files.iter().find(|f| f.id == file_id).cloned())
        }
    }

    /// Stub transcode service: writes fake bytes to the output path instead of
    /// running ffmpeg, and counts how many times it is invoked.
    #[derive(Debug)]
    struct StubTranscodeService {
        call_count: Arc<AtomicUsize>,
    }

    impl StubTranscodeService {
        fn new(call_count: Arc<AtomicUsize>) -> Self {
            Self { call_count }
        }
    }

    #[async_trait::async_trait]
    impl TranscodeService for StubTranscodeService {
        async fn generate_mp4_cache(
            &self,
            _source_path: &std::path::Path,
            output_path: &std::path::Path,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            // Write fake bytes so `serve_mp4_file` can read the resulting file.
            std::fs::write(output_path, b"FAKE_MP4_DATA_FOR_TESTING")?;
            Ok(())
        }
    }

    // ─── Test fixture ─────────────────────────────────────────────────────────

    struct TestFixture {
        state: AppState,
        auth: Arc<LocalAuthService>,
        transcode_call_count: Arc<AtomicUsize>,
        /// Keeps the cache TempDir alive for the duration of the test.
        _cache_dir: TempDir,
    }

    fn make_test_state(files: Vec<LibraryFile>) -> TestFixture {
        let cache_dir = TempDir::new().expect("create cache tmpdir");

        let session_store = Arc::new(InMemorySessionStore::default());
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let auth = Arc::new(LocalAuthService::new(
            user_repo.clone(),
            session_store,
            TEST_JWT_SECRET.to_string(),
        ));

        let notification = Arc::new(InMemoryNotificationService::new());
        let admin_log: Arc<dyn AdminLogService> = Arc::new(LocalAdminLogService::new(Arc::new(
            InMemoryAdminLogRepository::default(),
        )));

        let transcode_call_count = Arc::new(AtomicUsize::new(0));

        let services = AppServices {
            auth: auth.clone(),
            hash: Arc::new(StubHashService),
            library: Arc::new(StubLibraryService::new(files)),
            metadata: Arc::new(StubMetadataService),
            transcode: Arc::new(StubTranscodeService::new(transcode_call_count.clone())),
            notification,
            admin_log,
            user_repo: user_repo.clone(),
        };

        let config = crate::config::ServerConfig {
            bind_address: "0.0.0.0:8000".to_string(),
            server_url: "http://localhost:8000".to_string(),
            enable_metrics: false,
            video_dir: PathBuf::from("/tmp"),
            cache_dir: cache_dir.path().to_path_buf(),
            database_url: "postgres://unused:unused@localhost/unused".to_string(),
            jwt_secret: TEST_JWT_SECRET.to_string(),
            redis_url: "redis://localhost".to_string(),
            beam_index_url: "http://localhost:50051".to_string(),
        };

        let state = AppState::new(config, services);

        TestFixture {
            state,
            auth,
            transcode_call_count,
            _cache_dir: cache_dir,
        }
    }

    /// Registers a test user and returns `(jwt_token, user_id)`.
    async fn register_and_get_token(auth: &LocalAuthService) -> (String, String) {
        let resp = auth
            .register(
                "testuser",
                "test@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .expect("registration should succeed");
        (resp.token, resp.user.id)
    }

    fn build_service(fixture: &TestFixture) -> Service {
        // Use a minimal router containing only the stream endpoints under test.
        // This avoids pulling in the full GraphQL schema and any unrelated middleware.
        let router = Router::new()
            .hoop(affix_state::inject(fixture.state.clone()))
            .push(
                Router::with_path("v1")
                    .push(Router::with_path("stream/{id}/token").post(get_stream_token))
                    .push(Router::with_path("stream/mp4/{id}").get(stream_mp4)),
            );
        Service::new(router)
    }

    /// Constructs a minimal `LibraryFile` fixture for a given `(id, path)` pair.
    fn make_library_file(id: &str, path: &str) -> LibraryFile {
        LibraryFile {
            id: id.to_string(),
            library_id: "00000000-0000-0000-0000-000000000001".to_string(),
            path: path.to_string(),
            size_bytes: 1024,
            mime_type: Some("video/mp4".to_string()),
            duration_secs: Some(60.0),
            container_format: Some("mp4".to_string()),
            status: FileIndexStatus::Known,
            content_type: FileContentType::Movie,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    // ─── Tests: POST /v1/stream/:id/token ─────────────────────────────────────

    /// A valid Bearer JWT should yield 200 and a JSON body containing `"token"`.
    #[tokio::test]
    async fn test_get_stream_token_valid_jwt() {
        let fixture = make_test_state(vec![make_library_file(TEST_FILE_ID, "/tmp/video.mkv")]);
        let service = build_service(&fixture);
        let (jwt, _user_id) = register_and_get_token(&fixture.auth).await;

        let mut res =
            TestClient::post(format!("http://localhost/v1/stream/{}/token", TEST_FILE_ID))
                .bearer_auth(&jwt)
                .send(&service)
                .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));
        let body: Value = res.take_json().await.expect("valid JSON body");
        assert!(
            body.get("token").and_then(Value::as_str).is_some(),
            "Expected 'token' field in response body, got: {body}"
        );
    }

    /// A request without an Authorization header must return 401.
    #[tokio::test]
    async fn test_get_stream_token_missing_authorization() {
        let fixture = make_test_state(vec![make_library_file(TEST_FILE_ID, "/tmp/video.mkv")]);
        let service = build_service(&fixture);

        let res = TestClient::post(format!("http://localhost/v1/stream/{}/token", TEST_FILE_ID))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    /// An Authorization header without the `Bearer ` prefix must return 401.
    #[tokio::test]
    async fn test_get_stream_token_malformed_authorization() {
        let fixture = make_test_state(vec![make_library_file(TEST_FILE_ID, "/tmp/video.mkv")]);
        let service = build_service(&fixture);

        let res = TestClient::post(format!("http://localhost/v1/stream/{}/token", TEST_FILE_ID))
            .add_header("Authorization", "NotBearer some-token", true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    /// An invalid/tampered JWT must return 401.
    #[tokio::test]
    async fn test_get_stream_token_invalid_jwt() {
        let fixture = make_test_state(vec![make_library_file(TEST_FILE_ID, "/tmp/video.mkv")]);
        let service = build_service(&fixture);

        let res = TestClient::post(format!("http://localhost/v1/stream/{}/token", TEST_FILE_ID))
            .bearer_auth("not.a.valid.jwt.token")
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    // ─── Tests: GET /v1/stream/mp4/:id?token=… ────────────────────────────────

    /// Cache miss: transcode service is invoked and the response is 200/206 with
    /// Content-Type: video/mp4.
    #[tokio::test]
    async fn test_stream_mp4_cache_miss_triggers_transcode() {
        let source_dir = TempDir::new().unwrap();
        let source_file = source_dir.path().join("video.mkv");
        std::fs::write(&source_file, b"FAKE SOURCE DATA").unwrap();

        let fixture = make_test_state(vec![make_library_file(
            TEST_FILE_ID,
            source_file.to_str().unwrap(),
        )]);
        let service = build_service(&fixture);

        let stream_token = fixture
            .state
            .services
            .auth
            .create_stream_token("dummy-user", TEST_FILE_ID)
            .expect("create_stream_token should succeed");

        let res = TestClient::get(format!(
            "http://localhost/v1/stream/mp4/{}?token={}",
            TEST_FILE_ID, stream_token
        ))
        .send(&service)
        .await;

        let status = res.status_code.unwrap();
        assert!(
            status == StatusCode::OK || status == StatusCode::PARTIAL_CONTENT,
            "Expected 200 or 206, got: {status}"
        );
        assert_eq!(
            res.headers()
                .get("Content-Type")
                .and_then(|v| v.to_str().ok()),
            Some("video/mp4"),
            "Expected Content-Type: video/mp4"
        );
        assert_eq!(
            fixture.transcode_call_count.load(Ordering::SeqCst),
            1,
            "Expected transcode service to be called exactly once (cache miss)"
        );
    }

    /// Cache hit: the transcode service must NOT be invoked when the cache file
    /// already exists.
    #[tokio::test]
    async fn test_stream_mp4_cache_hit_skips_transcode() {
        let source_dir = TempDir::new().unwrap();
        let source_file = source_dir.path().join("video.mkv");
        std::fs::write(&source_file, b"FAKE SOURCE DATA").unwrap();

        let fixture = make_test_state(vec![make_library_file(
            TEST_FILE_ID,
            source_file.to_str().unwrap(),
        )]);

        // Pre-populate the cache file so the handler skips transcoding.
        let cache_file = fixture
            .state
            .config
            .cache_dir
            .join(format!("{}.mp4", TEST_FILE_ID));
        std::fs::write(&cache_file, b"CACHED MP4 CONTENT").unwrap();

        let service = build_service(&fixture);

        let stream_token = fixture
            .state
            .services
            .auth
            .create_stream_token("dummy-user", TEST_FILE_ID)
            .expect("create_stream_token should succeed");

        let res = TestClient::get(format!(
            "http://localhost/v1/stream/mp4/{}?token={}",
            TEST_FILE_ID, stream_token
        ))
        .send(&service)
        .await;

        let status = res.status_code.unwrap();
        assert!(
            status == StatusCode::OK || status == StatusCode::PARTIAL_CONTENT,
            "Expected 200 or 206, got: {status}"
        );
        assert_eq!(
            fixture.transcode_call_count.load(Ordering::SeqCst),
            0,
            "Expected transcode service to NOT be called (cache hit)"
        );
    }

    /// When the file is in the library but the source path does not exist on disk,
    /// the handler must return 404.
    #[tokio::test]
    async fn test_stream_mp4_source_file_not_found() {
        let fixture = make_test_state(vec![make_library_file(
            TEST_FILE_ID,
            "/tmp/__nonexistent_source_video_xyz_beam_test__.mkv",
        )]);
        let service = build_service(&fixture);

        let stream_token = fixture
            .state
            .services
            .auth
            .create_stream_token("dummy-user", TEST_FILE_ID)
            .expect("create_stream_token should succeed");

        let res = TestClient::get(format!(
            "http://localhost/v1/stream/mp4/{}?token={}",
            TEST_FILE_ID, stream_token
        ))
        .send(&service)
        .await;

        assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
    }

    /// When the file ID is not present in the library service, return 404.
    #[tokio::test]
    async fn test_stream_mp4_file_id_not_in_library() {
        let fixture = make_test_state(vec![]); // empty library
        let service = build_service(&fixture);

        let stream_token = fixture
            .state
            .services
            .auth
            .create_stream_token("dummy-user", TEST_FILE_ID)
            .expect("create_stream_token should succeed");

        let res = TestClient::get(format!(
            "http://localhost/v1/stream/mp4/{}?token={}",
            TEST_FILE_ID, stream_token
        ))
        .send(&service)
        .await;

        assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
    }

    /// An invalid/tampered stream token must return 401.
    #[tokio::test]
    async fn test_stream_mp4_invalid_stream_token() {
        let fixture = make_test_state(vec![]);
        let service = build_service(&fixture);

        let res = TestClient::get(format!(
            "http://localhost/v1/stream/mp4/{}?token=not.a.valid.token",
            TEST_FILE_ID
        ))
        .send(&service)
        .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    /// A stream token issued for a *different* file ID than the path param must
    /// return 401.
    #[tokio::test]
    async fn test_stream_mp4_token_wrong_file_id() {
        let different_file_id = "22222222-2222-2222-2222-222222222222";

        let fixture = make_test_state(vec![]);
        let service = build_service(&fixture);

        // Token is for `different_file_id`, but the path requests `TEST_FILE_ID`.
        let stream_token = fixture
            .state
            .services
            .auth
            .create_stream_token("dummy-user", different_file_id)
            .expect("create_stream_token should succeed");

        let res = TestClient::get(format!(
            "http://localhost/v1/stream/mp4/{}?token={}",
            TEST_FILE_ID, stream_token
        ))
        .send(&service)
        .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    /// A request without a Range header must return 200 and include
    /// `Accept-Ranges: bytes`.
    #[tokio::test]
    async fn test_stream_mp4_no_range_header_returns_200() {
        let source_dir = TempDir::new().unwrap();
        let source_file = source_dir.path().join("video.mkv");
        std::fs::write(&source_file, b"FAKE SOURCE DATA FOR RANGE TEST").unwrap();

        let fixture = make_test_state(vec![make_library_file(
            TEST_FILE_ID,
            source_file.to_str().unwrap(),
        )]);
        let service = build_service(&fixture);

        let stream_token = fixture
            .state
            .services
            .auth
            .create_stream_token("dummy-user", TEST_FILE_ID)
            .expect("create_stream_token should succeed");

        let res = TestClient::get(format!(
            "http://localhost/v1/stream/mp4/{}?token={}",
            TEST_FILE_ID, stream_token
        ))
        .send(&service)
        .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));
        assert_eq!(
            res.headers()
                .get("Accept-Ranges")
                .and_then(|v| v.to_str().ok()),
            Some("bytes"),
            "Expected Accept-Ranges: bytes header"
        );
    }

    /// A `Range: bytes=0-99` request against a 200-byte cache file must return
    /// 206 with the correct `Content-Range` and `Content-Length` headers.
    #[tokio::test]
    async fn test_stream_mp4_range_header_returns_206() {
        let source_dir = TempDir::new().unwrap();
        let source_file = source_dir.path().join("video.mkv");
        let data = vec![0u8; 200];
        std::fs::write(&source_file, &data).unwrap();

        let fixture = make_test_state(vec![make_library_file(
            TEST_FILE_ID,
            source_file.to_str().unwrap(),
        )]);

        // Pre-create the 200-byte cache file so no transcoding is needed.
        let cache_file = fixture
            .state
            .config
            .cache_dir
            .join(format!("{}.mp4", TEST_FILE_ID));
        std::fs::write(&cache_file, &data).unwrap();

        let service = build_service(&fixture);

        let stream_token = fixture
            .state
            .services
            .auth
            .create_stream_token("dummy-user", TEST_FILE_ID)
            .expect("create_stream_token should succeed");

        let res = TestClient::get(format!(
            "http://localhost/v1/stream/mp4/{}?token={}",
            TEST_FILE_ID, stream_token
        ))
        .add_header("Range", "bytes=0-99", true)
        .send(&service)
        .await;

        assert_eq!(res.status_code, Some(StatusCode::PARTIAL_CONTENT));

        assert_eq!(
            res.headers()
                .get("Content-Range")
                .and_then(|v| v.to_str().ok()),
            Some("bytes 0-99/200"),
            "Unexpected Content-Range value"
        );
        assert_eq!(
            res.headers()
                .get("Content-Length")
                .and_then(|v| v.to_str().ok()),
            Some("100"),
            "Expected Content-Length of 100"
        );
    }
}
