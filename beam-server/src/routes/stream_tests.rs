/// Subcutaneous HTTP tests for the stream REST routes.
///
/// These tests spin up the full Salvo service with in-memory implementations for all
/// external dependencies — no Redis, no PostgreSQL, no real ffmpeg invocation required.
#[cfg(test)]
mod tests {
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
    use salvo::test::{ResponseExt, TestClient};
    use tempfile::TempDir;

    use crate::models::{FileContentType, FileIndexStatus, LibraryFile};
    use crate::routes::{download_file, stream_file};
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
            unimplemented!("not called in stream route tests")
        }

        async fn get_continue_watching(
            &self,
            _user_id: uuid::Uuid,
            _limit: u32,
        ) -> Result<Vec<ContinueWatchingItem>, PlaybackError> {
            unimplemented!("not called in stream route tests")
        }
    }

    // ─── Constants ────────────────────────────────────────────────────────────

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

        async fn get_media_sources(
            &self,
            _media_id: &str,
        ) -> Result<Vec<crate::models::MediaSource>, MetadataError> {
            unimplemented!("not called in stream route tests")
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

    // ─── Test fixture ─────────────────────────────────────────────────────────

    struct TestFixture {
        state: AppState,
        session_store: Arc<InMemorySessionStore>,
        user_repo: Arc<InMemoryUserRepository>,
    }

    fn make_test_state(files: Vec<LibraryFile>) -> TestFixture {
        let session_store = Arc::new(InMemorySessionStore::default());
        let user_repo = Arc::new(InMemoryUserRepository::default());

        let notification = Arc::new(InMemoryNotificationService::new());
        let admin_log: Arc<dyn AdminLogService> = Arc::new(LocalAdminLogService::new(Arc::new(
            InMemoryAdminLogRepository::default(),
        )));

        let services = AppServices {
            hash: Arc::new(StubHashService),
            library: Arc::new(StubLibraryService::new(files)),
            metadata: Arc::new(StubMetadataService),
            notification,
            admin_log,
            user_repo: user_repo.clone(),
            playback: Arc::new(StubPlaybackService),
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

        TestFixture {
            state,
            session_store,
            user_repo,
        }
    }

    /// Seeds a user + session directly (bypassing the OIDC login flow, which
    /// isn't under test here) and returns a `Cookie` header value.
    async fn seed_session_cookie(fixture: &TestFixture) -> String {
        let user = fixture
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

        let token = fixture
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

    fn build_service(fixture: &TestFixture) -> Service {
        // Use a minimal router containing only the stream endpoints under test.
        let router = Router::new()
            .hoop(affix_state::inject(fixture.state.clone()))
            .push(
                Router::with_path("v1").push(
                    Router::with_path("files/{file_id}")
                        .push(Router::with_path("stream").get(stream_file))
                        .push(Router::with_path("download").get(download_file)),
                ),
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
            hash: "0".to_string(),
            mime_type: Some("video/mp4".to_string()),
            duration_secs: Some(60.0),
            container_format: Some("mp4".to_string()),
            status: FileIndexStatus::Known,
            content_type: FileContentType::Movie,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    // ─── Tests: GET /v1/files/:file_id/stream ──────────────────────────────────

    /// The handler must serve the source file's bytes directly -- no
    /// transcoding, no remuxing, no cache copy -- and must reflect the
    /// file's actual mime type in the response, not a hardcoded "video/mp4".
    #[tokio::test]
    async fn test_stream_file_serves_source_file_directly() {
        let source_dir = TempDir::new().unwrap();
        let source_file = source_dir.path().join("video.mkv");
        let source_bytes = b"REAL SOURCE FILE BYTES, SERVED AS-IS";
        std::fs::write(&source_file, source_bytes).unwrap();

        let mut file = make_library_file(TEST_FILE_ID, source_file.to_str().unwrap());
        file.mime_type = Some("video/x-matroska".to_string());
        let fixture = make_test_state(vec![file]);
        let service = build_service(&fixture);
        let cookie = seed_session_cookie(&fixture).await;

        let mut res = TestClient::get(format!("http://localhost/v1/files/{}/stream", TEST_FILE_ID))
            .add_header("Cookie", cookie, true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));
        assert_eq!(
            res.headers()
                .get("Content-Type")
                .and_then(|v| v.to_str().ok()),
            Some("video/x-matroska"),
            "Content-Type must reflect the file's actual mime type, not an assumed video/mp4"
        );
        assert!(
            res.headers().get("Content-Disposition").is_none(),
            "stream endpoint must not set Content-Disposition (renders inline)"
        );
        let body = res.take_bytes(None).await.expect("collect body");
        assert_eq!(
            &body[..],
            &source_bytes[..],
            "response body must be the untouched source file bytes"
        );
    }

    /// When the file has no known mime type, fall back to a generic binary
    /// content type rather than assuming a container format.
    #[tokio::test]
    async fn test_stream_file_falls_back_to_octet_stream_without_mime_type() {
        let source_dir = TempDir::new().unwrap();
        let source_file = source_dir.path().join("video.unknown");
        std::fs::write(&source_file, b"DATA").unwrap();

        let mut file = make_library_file(TEST_FILE_ID, source_file.to_str().unwrap());
        file.mime_type = None;
        let fixture = make_test_state(vec![file]);
        let service = build_service(&fixture);
        let cookie = seed_session_cookie(&fixture).await;

        let res = TestClient::get(format!("http://localhost/v1/files/{}/stream", TEST_FILE_ID))
            .add_header("Cookie", cookie, true)
            .send(&service)
            .await;

        assert_eq!(
            res.headers()
                .get("Content-Type")
                .and_then(|v| v.to_str().ok()),
            Some("application/octet-stream")
        );
    }

    /// When the file is in the library but the source path does not exist on disk,
    /// the handler must return 404.
    #[tokio::test]
    async fn test_stream_file_source_file_not_found() {
        let fixture = make_test_state(vec![make_library_file(
            TEST_FILE_ID,
            "/tmp/__nonexistent_source_video_xyz_beam_test__.mkv",
        )]);
        let service = build_service(&fixture);
        let cookie = seed_session_cookie(&fixture).await;

        let res = TestClient::get(format!("http://localhost/v1/files/{}/stream", TEST_FILE_ID))
            .add_header("Cookie", cookie, true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
    }

    /// When the file ID is not present in the library service, return 404.
    #[tokio::test]
    async fn test_stream_file_file_id_not_in_library() {
        let fixture = make_test_state(vec![]); // empty library
        let service = build_service(&fixture);
        let cookie = seed_session_cookie(&fixture).await;

        let res = TestClient::get(format!("http://localhost/v1/files/{}/stream", TEST_FILE_ID))
            .add_header("Cookie", cookie, true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
    }

    /// No session cookie at all must return 401.
    #[tokio::test]
    async fn test_stream_file_missing_session_cookie() {
        let fixture = make_test_state(vec![]);
        let service = build_service(&fixture);

        let res = TestClient::get(format!("http://localhost/v1/files/{}/stream", TEST_FILE_ID))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    /// A cookie value that doesn't match any known session must return 401.
    #[tokio::test]
    async fn test_stream_file_unknown_session_cookie() {
        let fixture = make_test_state(vec![]);
        let service = build_service(&fixture);

        let res = TestClient::get(format!("http://localhost/v1/files/{}/stream", TEST_FILE_ID))
            .add_header("Cookie", "beam_session=not-a-real-session", true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    /// A request without a Range header must return 200 and include
    /// `Accept-Ranges: bytes`.
    #[tokio::test]
    async fn test_stream_file_no_range_header_returns_200() {
        let source_dir = TempDir::new().unwrap();
        let source_file = source_dir.path().join("video.mkv");
        std::fs::write(&source_file, b"FAKE SOURCE DATA FOR RANGE TEST").unwrap();

        let fixture = make_test_state(vec![make_library_file(
            TEST_FILE_ID,
            source_file.to_str().unwrap(),
        )]);
        let service = build_service(&fixture);
        let cookie = seed_session_cookie(&fixture).await;

        let res = TestClient::get(format!("http://localhost/v1/files/{}/stream", TEST_FILE_ID))
            .add_header("Cookie", cookie, true)
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

    /// A `Range: bytes=0-99` request against a 200-byte source file must return
    /// 206 with the correct `Content-Range` and `Content-Length` headers.
    #[tokio::test]
    async fn test_stream_file_range_header_returns_206() {
        let source_dir = TempDir::new().unwrap();
        let source_file = source_dir.path().join("video.mkv");
        let data = vec![0u8; 200];
        std::fs::write(&source_file, &data).unwrap();

        let fixture = make_test_state(vec![make_library_file(
            TEST_FILE_ID,
            source_file.to_str().unwrap(),
        )]);

        let service = build_service(&fixture);
        let cookie = seed_session_cookie(&fixture).await;

        let res = TestClient::get(format!("http://localhost/v1/files/{}/stream", TEST_FILE_ID))
            .add_header("Cookie", cookie, true)
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

    // ─── Tests: GET /v1/files/:file_id/download ───────────────────────────────

    /// The download endpoint must serve the same untouched source bytes as
    /// stream, but with a `Content-Disposition: attachment` header carrying
    /// the file's original on-disk name so browsers save it instead of trying
    /// to play it inline.
    #[tokio::test]
    async fn test_download_file_sets_attachment_disposition_with_filename() {
        let source_dir = TempDir::new().unwrap();
        let source_file = source_dir.path().join("My Movie (2024).mkv");
        let source_bytes = b"REAL SOURCE FILE BYTES";
        std::fs::write(&source_file, source_bytes).unwrap();

        let fixture = make_test_state(vec![make_library_file(
            TEST_FILE_ID,
            source_file.to_str().unwrap(),
        )]);
        let service = build_service(&fixture);
        let cookie = seed_session_cookie(&fixture).await;

        let mut res = TestClient::get(format!(
            "http://localhost/v1/files/{}/download",
            TEST_FILE_ID
        ))
        .add_header("Cookie", cookie, true)
        .send(&service)
        .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));
        assert_eq!(
            res.headers()
                .get("Content-Disposition")
                .and_then(|v| v.to_str().ok()),
            Some("attachment; filename=\"My Movie (2024).mkv\"")
        );
        let body = res.take_bytes(None).await.expect("collect body");
        assert_eq!(&body[..], &source_bytes[..]);
    }

    /// Download supports Range requests too, so an interrupted download can
    /// resume rather than restarting from byte zero.
    #[tokio::test]
    async fn test_download_file_supports_range_requests() {
        let source_dir = TempDir::new().unwrap();
        let source_file = source_dir.path().join("video.mkv");
        let data = vec![0u8; 200];
        std::fs::write(&source_file, &data).unwrap();

        let fixture = make_test_state(vec![make_library_file(
            TEST_FILE_ID,
            source_file.to_str().unwrap(),
        )]);
        let service = build_service(&fixture);
        let cookie = seed_session_cookie(&fixture).await;

        let res = TestClient::get(format!(
            "http://localhost/v1/files/{}/download",
            TEST_FILE_ID
        ))
        .add_header("Cookie", cookie, true)
        .add_header("Range", "bytes=100-199", true)
        .send(&service)
        .await;

        assert_eq!(res.status_code, Some(StatusCode::PARTIAL_CONTENT));
        assert_eq!(
            res.headers()
                .get("Content-Range")
                .and_then(|v| v.to_str().ok()),
            Some("bytes 100-199/200")
        );
    }

    /// No session cookie must return 401 for downloads too.
    #[tokio::test]
    async fn test_download_file_missing_session_cookie() {
        let fixture = make_test_state(vec![]);
        let service = build_service(&fixture);

        let res = TestClient::get(format!(
            "http://localhost/v1/files/{}/download",
            TEST_FILE_ID
        ))
        .send(&service)
        .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }
}
