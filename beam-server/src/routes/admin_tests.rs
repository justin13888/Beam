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

    use beam_auth::server::OidcRuntimeConfig;
    use beam_auth::utils::{
        models::{CreateUser, User},
        oidc::NotConfiguredOidcClient,
        pending_auth_store::in_memory::InMemoryPendingAuthStore,
        repository::{UserRepository, in_memory::InMemoryUserRepository},
        session_store::{SessionData, SessionStore, in_memory::InMemorySessionStore},
    };
    use beam_domain::models::enrichment::EnrichmentTargetId;
    use beam_domain::models::file::{CreateMediaFile, FileStatus};
    use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
    use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
    use beam_domain::repositories::{EnrichmentStateRepository, FileRepository};
    use beam_index::services::index::MockIndexService;
    use salvo::prelude::*;
    use salvo::test::{ResponseExt, TestClient};

    use crate::models::{
        AdminLogEntryDto, AdminStatusResponse, AdminUserListResponse, CreateLibraryRequest,
        Library, ScanLibraryResponse, UpdateAdminUserRequest,
    };
    use crate::routes::{
        create_library, delete_library, get_admin_log_count, get_admin_logs, get_admin_status,
        get_library, get_library_files, list_admin_users, list_libraries, refresh_media_metadata,
        scan_library, update_admin_user,
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

        async fn get_history(
            &self,
            _user_id: uuid::Uuid,
            _limit: u64,
            _offset: u64,
        ) -> Result<(Vec<crate::services::playback::HistoryItem>, u64), PlaybackError> {
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

    type InMemoryFileRepo = beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    type InMemoryEnrichmentRepo =
        beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository;

    struct TestFixture {
        state: AppState,
        session_store: Arc<InMemorySessionStore>,
        user_repo: Arc<InMemoryUserRepository>,
        file_repo: Arc<InMemoryFileRepo>,
        enrichment_repo: Arc<InMemoryEnrichmentRepo>,
    }

    fn make_test_state() -> TestFixture {
        let session_store = Arc::new(InMemorySessionStore::default());
        let user_repo = Arc::new(InMemoryUserRepository::default());

        let notification = Arc::new(InMemoryNotificationService::new());
        let admin_log: Arc<dyn AdminLogService> = Arc::new(LocalAdminLogService::new(Arc::new(
            InMemoryAdminLogRepository::default(),
        )));

        // Shared between the library service and `AppServices::{library_repo,
        // file_repo}` so the status endpoint's counts reflect libraries/files
        // created through the library service.
        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepo::default());
        let enrichment_repo = Arc::new(InMemoryEnrichmentRepo::default());

        let library: Arc<dyn LibraryService> = Arc::new(LocalLibraryService::new(
            library_repo.clone(),
            file_repo.clone(),
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
            hash: Arc::new(StubHashService),
            library,
            metadata: Arc::new(StubMetadataService),
            notification,
            admin_log,
            user_repo: user_repo.clone(),
            playback: Arc::new(StubPlaybackService),
            genre_repo: Arc::new(
                beam_domain::repositories::genre::in_memory::InMemoryGenreRepository::default(),
            ),
            library_repo,
            file_repo: file_repo.clone(),
            enrichment_repo: enrichment_repo.clone(),
            session_store: session_store.clone(),
            oidc_client: Arc::new(NotConfiguredOidcClient::new("not used in these tests")),
            pending_auth_store: Arc::new(InMemoryPendingAuthStore::default()),
            oidc_config: OidcRuntimeConfig {
                web_url: "http://localhost:5173".to_string(),
                cookie_secure: false,
                admin_claim: None,
                admin_value: None,
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
            oidc_admin_claim: None,
            oidc_admin_value: None,
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
            file_repo,
            enrichment_repo,
        }
    }

    /// Seeds a user + session directly (bypassing the OIDC login flow, which
    /// isn't under test here) and returns a `Cookie` header value.
    async fn seed_user_cookie(fixture: &TestFixture, is_admin: bool) -> String {
        let (oidc_subject, email, display_name) = if is_admin {
            ("admin-subj", "admin@example.com", "Admin User")
        } else {
            ("regular-subj", "regular@example.com", "Regular User")
        };

        let user = fixture
            .user_repo
            .create(CreateUser {
                oidc_issuer: "https://test.example".to_string(),
                oidc_subject: oidc_subject.to_string(),
                email: Some(email.to_string()),
                display_name: display_name.to_string(),
                avatar_url: None,
                is_admin,
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
                            .push(Router::with_path("logs/count").get(get_admin_log_count))
                            .push(Router::with_path("users").get(list_admin_users))
                            .push(Router::with_path("users/{id}").patch(update_admin_user))
                            .push(Router::with_path("status").get(get_admin_status)),
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
        let cookie = seed_user_cookie(&fixture, false).await;

        let mut res = TestClient::get("http://localhost/v1/libraries")
            .add_header("Cookie", cookie, true)
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
        let cookie = seed_user_cookie(&fixture, false).await;

        let res = TestClient::post("http://localhost/v1/admin/libraries")
            .add_header("Cookie", cookie, true)
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
        let cookie = seed_user_cookie(&fixture, true).await;

        let mut res = TestClient::post("http://localhost/v1/admin/libraries")
            .add_header("Cookie", cookie.clone(), true)
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
            .add_header("Cookie", cookie, true)
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
        let cookie = seed_user_cookie(&fixture, true).await;

        let mut create_res = TestClient::post("http://localhost/v1/admin/libraries")
            .add_header("Cookie", cookie.clone(), true)
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
        .add_header("Cookie", cookie, true)
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
        let cookie = seed_user_cookie(&fixture, true).await;

        let mut create_res = TestClient::post("http://localhost/v1/admin/libraries")
            .add_header("Cookie", cookie.clone(), true)
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
        .add_header("Cookie", cookie.clone(), true)
        .send(&service)
        .await;
        assert_eq!(res.status_code, Some(StatusCode::NO_CONTENT));

        let res_again = TestClient::delete(format!(
            "http://localhost/v1/admin/libraries/{}",
            created.id
        ))
        .add_header("Cookie", cookie, true)
        .send(&service)
        .await;
        assert_eq!(res_again.status_code, Some(StatusCode::NOT_FOUND));
    }

    // ─── Admin logs ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_admin_logs_regular_user_returns_403() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let cookie = seed_user_cookie(&fixture, false).await;

        let res = TestClient::get("http://localhost/v1/admin/logs")
            .add_header("Cookie", cookie, true)
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
        let cookie = seed_user_cookie(&fixture, true).await;

        let mut res = TestClient::get("http://localhost/v1/admin/logs")
            .add_header("Cookie", cookie, true)
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
        let cookie = seed_user_cookie(&fixture, false).await;

        let res = TestClient::post("http://localhost/v1/admin/media/some-id/refresh")
            .add_header("Cookie", cookie, true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::FORBIDDEN));
    }

    #[tokio::test]
    async fn test_refresh_media_metadata_admin_returns_204() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let cookie = seed_user_cookie(&fixture, true).await;

        let res = TestClient::post("http://localhost/v1/admin/media/some-id/refresh")
            .add_header("Cookie", cookie, true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::NO_CONTENT));
    }

    // ─── Admin users & system status (issue #85) ─────────────────────────────

    /// Seeds an additional non-admin user (no session) and returns it.
    async fn seed_plain_user(fixture: &TestFixture, subject: &str, name: &str) -> User {
        fixture
            .user_repo
            .create(CreateUser {
                oidc_issuer: "https://test.example".to_string(),
                oidc_subject: subject.to_string(),
                email: None,
                display_name: name.to_string(),
                avatar_url: None,
                is_admin: false,
            })
            .await
            .expect("seed user should succeed")
    }

    /// Creates a live session for `user_id` directly in the fixture's store.
    async fn seed_session_for(fixture: &TestFixture, user_id: &str) {
        fixture
            .session_store
            .create(
                &SessionData {
                    user_id: user_id.to_string(),
                    device_hash: "target-device".to_string(),
                    ip: "127.0.0.1".to_string(),
                    created_at: chrono::Utc::now().timestamp(),
                    last_active: chrono::Utc::now().timestamp(),
                },
                86400,
                86400,
            )
            .await
            .expect("seed session should succeed");
    }

    #[tokio::test]
    async fn test_admin_users_and_status_regular_user_returns_403() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let cookie = seed_user_cookie(&fixture, false).await;
        let target = seed_plain_user(&fixture, "target-subj", "Target").await;

        let res = TestClient::get("http://localhost/v1/admin/users")
            .add_header("Cookie", cookie.clone(), true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::FORBIDDEN));

        let res = TestClient::patch(format!("http://localhost/v1/admin/users/{}", target.id))
            .add_header("Cookie", cookie.clone(), true)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::FORBIDDEN));

        let res = TestClient::get("http://localhost/v1/admin/status")
            .add_header("Cookie", cookie, true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::FORBIDDEN));
    }

    #[tokio::test]
    async fn test_list_admin_users_missing_auth_returns_401() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let res = TestClient::get("http://localhost/v1/admin/users")
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn test_list_admin_users_paginates_with_total() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let cookie = seed_user_cookie(&fixture, true).await; // user 1 (admin)
        seed_plain_user(&fixture, "s2", "Alice").await;
        seed_plain_user(&fixture, "s3", "Bob").await;
        seed_plain_user(&fixture, "s4", "Carol").await;

        // Default limit returns everyone, all enabled, exactly one admin.
        let mut res = TestClient::get("http://localhost/v1/admin/users")
            .add_header("Cookie", cookie.clone(), true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::OK));
        let body: AdminUserListResponse = res.take_json().await.unwrap();
        assert_eq!(body.total, 4);
        assert_eq!(body.items.len(), 4);
        assert!(body.items.iter().all(|u| !u.disabled));
        assert_eq!(body.items.iter().filter(|u| u.is_admin).count(), 1);

        // Two pages of two cover all four users exactly once, and `total`
        // stays the full count on every page.
        let mut page1_res = TestClient::get("http://localhost/v1/admin/users?limit=2&offset=0")
            .add_header("Cookie", cookie.clone(), true)
            .send(&service)
            .await;
        let page1: AdminUserListResponse = page1_res.take_json().await.unwrap();
        let mut page2_res = TestClient::get("http://localhost/v1/admin/users?limit=2&offset=2")
            .add_header("Cookie", cookie.clone(), true)
            .send(&service)
            .await;
        let page2: AdminUserListResponse = page2_res.take_json().await.unwrap();
        assert_eq!(page1.items.len(), 2);
        assert_eq!(page2.items.len(), 2);
        assert_eq!(page1.total, 4);
        assert_eq!(page2.total, 4);
        let mut ids: Vec<String> = page1
            .items
            .iter()
            .chain(page2.items.iter())
            .map(|u| u.id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 4, "pages must cover every user exactly once");

        // limit is clamped to at least 1.
        let mut clamped_res = TestClient::get("http://localhost/v1/admin/users?limit=0")
            .add_header("Cookie", cookie, true)
            .send(&service)
            .await;
        let clamped: AdminUserListResponse = clamped_res.take_json().await.unwrap();
        assert_eq!(clamped.items.len(), 1);
    }

    #[tokio::test]
    async fn test_disable_user_revokes_sessions_and_reenable_flips_flag() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let admin_cookie = seed_user_cookie(&fixture, true).await;

        let target = seed_plain_user(&fixture, "target-subj", "Target").await;
        seed_session_for(&fixture, &target.id.to_string()).await;
        seed_session_for(&fixture, &target.id.to_string()).await;
        assert_eq!(
            fixture
                .session_store
                .list_for_user(&target.id.to_string())
                .await
                .unwrap()
                .len(),
            2
        );

        let res = TestClient::patch(format!("http://localhost/v1/admin/users/{}", target.id))
            .add_header("Cookie", admin_cookie.clone(), true)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::NO_CONTENT));

        let stored = fixture
            .user_repo
            .find_by_id(target.id)
            .await
            .unwrap()
            .unwrap();
        assert!(stored.disabled);
        assert_eq!(
            fixture
                .session_store
                .list_for_user(&target.id.to_string())
                .await
                .unwrap()
                .len(),
            0,
            "disabling must revoke every session of the target"
        );

        // Re-enable: the flag flips back and no session reappears.
        let res = TestClient::patch(format!("http://localhost/v1/admin/users/{}", target.id))
            .add_header("Cookie", admin_cookie, true)
            .json(&UpdateAdminUserRequest { disabled: false })
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::NO_CONTENT));
        let stored = fixture
            .user_repo
            .find_by_id(target.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!stored.disabled);
        assert_eq!(
            fixture
                .session_store
                .list_for_user(&target.id.to_string())
                .await
                .unwrap()
                .len(),
            0,
            "re-enabling must not mint sessions"
        );
    }

    #[tokio::test]
    async fn test_admin_cannot_disable_themselves() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let cookie = seed_user_cookie(&fixture, true).await;
        let admin = fixture
            .user_repo
            .find_by_oidc_identity("https://test.example", "admin-subj")
            .await
            .unwrap()
            .expect("admin was seeded");

        let res = TestClient::patch(format!("http://localhost/v1/admin/users/{}", admin.id))
            .add_header("Cookie", cookie.clone(), true)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));

        let stored = fixture
            .user_repo
            .find_by_id(admin.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!stored.disabled, "self-disable must not change the account");
    }

    #[tokio::test]
    async fn test_patch_user_unknown_id_returns_404_and_invalid_id_400() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let cookie = seed_user_cookie(&fixture, true).await;

        let res = TestClient::patch(format!(
            "http://localhost/v1/admin/users/{}",
            uuid::Uuid::new_v4()
        ))
        .add_header("Cookie", cookie.clone(), true)
        .json(&UpdateAdminUserRequest { disabled: true })
        .send(&service)
        .await;
        assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));

        let res = TestClient::patch("http://localhost/v1/admin/users/not-a-uuid")
            .add_header("Cookie", cookie, true)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn test_admin_status_reports_counts_queue_and_recent_scans() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let cookie = seed_user_cookie(&fixture, true).await; // user 1 (admin)
        seed_plain_user(&fixture, "s2", "Alice").await; // user 2

        // One library created through the API, one file indexed into it.
        let mut create_res = TestClient::post("http://localhost/v1/admin/libraries")
            .add_header("Cookie", cookie.clone(), true)
            .json(&CreateLibraryRequest {
                name: "Movies".to_string(),
                root_path: "movies".to_string(),
            })
            .send(&service)
            .await;
        assert_eq!(create_res.status_code, Some(StatusCode::OK));
        let library: Library = create_res.take_json().await.unwrap();
        fixture
            .file_repo
            .create(CreateMediaFile {
                library_id: uuid::Uuid::parse_str(&library.id).unwrap(),
                path: PathBuf::from("/videos/movies/a.mkv"),
                hash: 1,
                size_bytes: 10,
                mtime: None,
                mime_type: None,
                duration: None,
                container_format: None,
                content: None,
                status: FileStatus::Known,
            })
            .await
            .unwrap();

        // Enrichment queue: two rows, one of which then fails terminally.
        fixture
            .enrichment_repo
            .ensure_pending(EnrichmentTargetId::Movie(uuid::Uuid::new_v4()))
            .await
            .unwrap();
        fixture
            .enrichment_repo
            .ensure_pending(EnrichmentTargetId::Movie(uuid::Uuid::new_v4()))
            .await
            .unwrap();
        let due = fixture
            .enrichment_repo
            .fetch_due(chrono::Utc::now(), 10)
            .await
            .unwrap();
        fixture
            .enrichment_repo
            .mark_failed(due[0].id, "provider exploded", chrono::Utc::now())
            .await
            .unwrap();

        // Two scan log entries plus one unrelated system entry that must be
        // filtered out of `recent_scans`.
        for message in ["scan one", "scan two"] {
            fixture
                .state
                .services
                .admin_log
                .log(
                    beam_domain::models::AdminLogLevel::Info,
                    beam_domain::models::AdminLogCategory::LibraryScan,
                    message.to_string(),
                    None,
                )
                .await
                .unwrap();
        }
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

        let mut res = TestClient::get("http://localhost/v1/admin/status")
            .add_header("Cookie", cookie, true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::OK));
        let body: AdminStatusResponse = res.take_json().await.unwrap();

        // uptime_secs deserialized as u64 (its presence is shape-verified);
        // version is the crate's own.
        assert_eq!(body.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(body.counts.users, 2);
        assert_eq!(body.counts.libraries, 1);
        assert_eq!(body.counts.files, 1);
        assert_eq!(body.enrichment.pending, 1);
        assert_eq!(body.enrichment.failed, 1);
        assert_eq!(body.enrichment.enriched, 0);
        assert_eq!(body.enrichment.unmatched, 0);

        let messages: Vec<&str> = body
            .recent_scans
            .iter()
            .map(|scan| scan.message.as_str())
            .collect();
        assert_eq!(body.recent_scans.len(), 2);
        assert!(messages.contains(&"scan one"));
        assert!(messages.contains(&"scan two"));
        assert!(
            !messages.contains(&"server started"),
            "non-scan categories must be filtered out"
        );
    }
}
