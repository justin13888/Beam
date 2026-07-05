/// Subcutaneous HTTP tests for the `/v1/libraries` and `/v1/admin/*` REST
/// routes. These spin up the full Salvo service with in-memory
/// implementations for all external dependencies -- no Redis, no PostgreSQL
/// required. Library CRUD/scan runs against the real `LocalLibraryService`
/// (backed by in-memory repos), not a stub, so this also exercises that
/// service's actual logic through the new REST surface.
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use argon2::password_hash::{SaltString, rand_core::OsRng};
    use argon2::{Argon2, PasswordHasher};
    use beam_auth::utils::{
        models::CreateUser,
        repository::{UserRepository, in_memory::InMemoryUserRepository},
        service::{AuthService, LocalAuthService},
        session_store::in_memory::InMemorySessionStore,
    };
    use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
    use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
    use beam_index::services::index::MockIndexService;
    use salvo::prelude::*;
    use salvo::test::{ResponseExt, TestClient};

    use crate::models::{AdminLogEntryDto, CreateLibraryRequest, Library, ScanLibraryResponse};
    use crate::routes::{
        create_library, delete_library, get_admin_log_count, get_admin_logs, get_library,
        get_library_files, list_libraries, refresh_media_metadata, scan_library,
    };
    use crate::services::admin_log::{AdminLogService, LocalAdminLogService};
    use crate::services::hash::HashService;
    use crate::services::library::{InMemoryPathValidator, LibraryService, LocalLibraryService};
    use crate::services::metadata::{
        MediaConnection, MediaFilter, MediaSearchFilters, MediaSortField, MetadataError,
        MetadataService, PageInfo, SortOrder,
    };
    use crate::services::notification::InMemoryNotificationService;
    use crate::services::playback::{
        ContinueWatchingItem, PlaybackError, PlaybackProgressDto, PlaybackService,
    };
    use crate::state::{AppServices, AppState};

    const TEST_JWT_SECRET: &str = "test-jwt-secret-for-admin-route-tests";
    const ADMIN_PASSWORD: &str = "admin-password-123";

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
            unimplemented!("not called in admin route tests")
        }

        async fn get_continue_watching(
            &self,
            _user_id: uuid::Uuid,
            _limit: u32,
        ) -> Result<Vec<ContinueWatchingItem>, PlaybackError> {
            unimplemented!("not called in admin route tests")
        }
    }

    #[derive(Debug)]
    struct StubHashService;

    #[async_trait::async_trait]
    impl HashService for StubHashService {
        fn hash_sync(&self, _path: &std::path::Path) -> std::io::Result<u64> {
            unimplemented!("not called in admin route tests")
        }
        async fn hash_async(&self, _path: PathBuf) -> std::io::Result<u64> {
            unimplemented!("not called in admin route tests")
        }
    }

    #[derive(Debug, Default)]
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
            unimplemented!("not called in admin route tests")
        }
    }

    struct TestFixture {
        state: AppState,
        auth: Arc<LocalAuthService>,
        user_repo: Arc<InMemoryUserRepository>,
    }

    fn hash_password(password: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .unwrap()
            .to_string()
    }

    fn make_test_state() -> TestFixture {
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

        let library: Arc<dyn LibraryService> = Arc::new(LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(beam_domain::repositories::file::in_memory::InMemoryFileRepository::default()),
            PathBuf::from("/videos"),
            notification.clone(),
            Arc::new({
                let mut mock_index = MockIndexService::new();
                mock_index.expect_scan_library().returning(|_| Ok(0));
                mock_index
            }),
            Arc::new(InMemoryPathValidator::success(PathBuf::from(
                "/videos/movies",
            ))),
        ));

        let services = AppServices {
            auth: auth.clone(),
            hash: Arc::new(StubHashService),
            library,
            metadata: Arc::new(StubMetadataService),
            notification,
            admin_log,
            user_repo: user_repo.clone(),
            playback: Arc::new(StubPlaybackService),
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
        TestFixture {
            state,
            auth,
            user_repo,
        }
    }

    async fn register_regular_user(fixture: &TestFixture) -> String {
        fixture
            .auth
            .register(
                "regularuser",
                "regular@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .expect("registration should succeed")
            .token
    }

    async fn login_as_admin(fixture: &TestFixture) -> String {
        fixture
            .user_repo
            .create(CreateUser {
                username: "admin".to_string(),
                email: "admin@example.com".to_string(),
                password_hash: hash_password(ADMIN_PASSWORD),
                is_admin: true,
            })
            .await
            .expect("should seed admin user");

        fixture
            .auth
            .login("admin", ADMIN_PASSWORD, "device-hash", "127.0.0.1")
            .await
            .expect("admin login should succeed")
            .token
    }

    fn build_service(fixture: &TestFixture) -> Service {
        let router = Router::new()
            .hoop(affix_state::inject(fixture.state.clone()))
            .push(
                Router::with_path("v1")
                    .push(
                        Router::with_path("libraries")
                            .get(list_libraries)
                            .push(Router::with_path("{id}").get(get_library))
                            .push(Router::with_path("{id}/files").get(get_library_files)),
                    )
                    .push(
                        Router::with_path("admin")
                            .push(Router::with_path("libraries").post(create_library))
                            .push(Router::with_path("libraries/{id}/scan").post(scan_library))
                            .push(Router::with_path("libraries/{id}").delete(delete_library))
                            .push(
                                Router::with_path("media/{id}/refresh")
                                    .post(refresh_media_metadata),
                            )
                            .push(Router::with_path("logs").get(get_admin_logs))
                            .push(Router::with_path("logs/count").get(get_admin_log_count)),
                    ),
            );
        Service::new(router)
    }

    // ─── Library reads ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_libraries_requires_auth() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let res = TestClient::get("http://localhost/v1/libraries")
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn test_list_libraries_authenticated_returns_empty_list() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let token = register_regular_user(&fixture).await;

        let mut res = TestClient::get("http://localhost/v1/libraries")
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::OK));
        let body: Vec<Library> = res.take_json().await.unwrap();
        assert!(body.is_empty());
    }

    // ─── Library mutations: admin-gated ─────────────────────────────────────

    #[tokio::test]
    async fn test_create_library_regular_user_returns_403() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let token = register_regular_user(&fixture).await;

        let res = TestClient::post("http://localhost/v1/admin/libraries")
            .add_header("Authorization", format!("Bearer {token}"), true)
            .json(&CreateLibraryRequest {
                name: "Movies".to_string(),
                root_path: "movies".to_string(),
            })
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::FORBIDDEN));
    }

    #[tokio::test]
    async fn test_create_library_admin_succeeds_and_is_then_listed() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let token = login_as_admin(&fixture).await;

        let mut res = TestClient::post("http://localhost/v1/admin/libraries")
            .add_header("Authorization", format!("Bearer {token}"), true)
            .json(&CreateLibraryRequest {
                name: "Movies".to_string(),
                root_path: "movies".to_string(),
            })
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::OK));
        let created: Library = res.take_json().await.unwrap();
        assert_eq!(created.name, "Movies");

        let mut list_res = TestClient::get("http://localhost/v1/libraries")
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;
        let libraries: Vec<Library> = list_res.take_json().await.unwrap();
        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].id, created.id);
    }

    #[tokio::test]
    async fn test_scan_library_admin_returns_added_count() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let token = login_as_admin(&fixture).await;

        let mut create_res = TestClient::post("http://localhost/v1/admin/libraries")
            .add_header("Authorization", format!("Bearer {token}"), true)
            .json(&CreateLibraryRequest {
                name: "Movies".to_string(),
                root_path: "movies".to_string(),
            })
            .send(&service)
            .await;
        let created: Library = create_res.take_json().await.unwrap();

        let mut scan_res = TestClient::post(format!(
            "http://localhost/v1/admin/libraries/{}/scan",
            created.id
        ))
        .add_header("Authorization", format!("Bearer {token}"), true)
        .send(&service)
        .await;
        assert_eq!(scan_res.status_code, Some(StatusCode::OK));
        let body: ScanLibraryResponse = scan_res.take_json().await.unwrap();
        // MockIndexService::new() has no expectations set, so scanning
        // succeeds trivially with 0 files added -- this test only verifies
        // wiring/auth/response shape, not scan semantics (covered elsewhere).
        assert_eq!(body.added, 0);
    }

    #[tokio::test]
    async fn test_delete_library_admin_returns_204_then_404_on_repeat() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let token = login_as_admin(&fixture).await;

        let mut create_res = TestClient::post("http://localhost/v1/admin/libraries")
            .add_header("Authorization", format!("Bearer {token}"), true)
            .json(&CreateLibraryRequest {
                name: "Movies".to_string(),
                root_path: "movies".to_string(),
            })
            .send(&service)
            .await;
        let created: Library = create_res.take_json().await.unwrap();

        let res = TestClient::delete(format!(
            "http://localhost/v1/admin/libraries/{}",
            created.id
        ))
        .add_header("Authorization", format!("Bearer {token}"), true)
        .send(&service)
        .await;
        assert_eq!(res.status_code, Some(StatusCode::NO_CONTENT));

        let res_again = TestClient::delete(format!(
            "http://localhost/v1/admin/libraries/{}",
            created.id
        ))
        .add_header("Authorization", format!("Bearer {token}"), true)
        .send(&service)
        .await;
        assert_eq!(res_again.status_code, Some(StatusCode::NOT_FOUND));
    }

    // ─── Admin logs ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_admin_logs_regular_user_returns_403() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let token = register_regular_user(&fixture).await;

        let res = TestClient::get("http://localhost/v1/admin/logs")
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::FORBIDDEN));
    }

    #[tokio::test]
    async fn test_get_admin_logs_admin_returns_seeded_entries() {
        let fixture = make_test_state();
        fixture
            .state
            .services
            .admin_log
            .log(
                beam_domain::models::AdminLogLevel::Info,
                beam_domain::models::AdminLogCategory::System,
                "server started".to_string(),
                None,
            )
            .await
            .unwrap();

        let service = build_service(&fixture);
        let token = login_as_admin(&fixture).await;

        let mut res = TestClient::get("http://localhost/v1/admin/logs")
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::OK));
        let logs: Vec<AdminLogEntryDto> = res.take_json().await.unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message, "server started");
    }

    #[tokio::test]
    async fn test_get_admin_log_count_missing_auth_returns_401() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let res = TestClient::get("http://localhost/v1/admin/logs/count")
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    // ─── Refresh metadata ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_refresh_media_metadata_regular_user_returns_403() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let token = register_regular_user(&fixture).await;

        let res = TestClient::post("http://localhost/v1/admin/media/some-id/refresh")
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::FORBIDDEN));
    }

    #[tokio::test]
    async fn test_refresh_media_metadata_admin_returns_204() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let token = login_as_admin(&fixture).await;

        let res = TestClient::post("http://localhost/v1/admin/media/some-id/refresh")
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::NO_CONTENT));
    }
}
