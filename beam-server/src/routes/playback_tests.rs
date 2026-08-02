/// Subcutaneous HTTP tests for `/v1/files/{file_id}/progress` and
/// `/v1/continue-watching`.
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
    use beam_domain::models::movie::Movie;
    use beam_domain::models::{MediaFile, MediaFileContent, MovieEntry};
    use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
    use salvo::prelude::*;
    use salvo::test::{ResponseExt, TestClient};

    use crate::routes::{ReportProgressRequest, get_continue_watching, report_playback_progress};
    use crate::services::admin_log::{AdminLogService, LocalAdminLogService};
    use crate::services::hash::HashService;
    use crate::services::library::LibraryError;
    use crate::services::metadata::{
        MediaConnection, MediaFilter, MediaSearchFilters, MediaSortField, MetadataError,
        MetadataService, PageInfo, SortOrder,
    };
    use crate::services::notification::InMemoryNotificationService;
    use crate::services::playback::{ContinueWatchingItem, DbPlaybackService, PlaybackProgressDto};
    use crate::state::{AppServices, AppState};

    #[derive(Debug)]
    struct StubHashService;

    #[async_trait::async_trait]
    impl HashService for StubHashService {
        fn hash_sync(&self, _path: &std::path::Path) -> std::io::Result<u64> {
            unimplemented!("not called in playback route tests")
        }
        async fn hash_async(&self, _path: PathBuf) -> std::io::Result<u64> {
            unimplemented!("not called in playback route tests")
        }
    }

    #[derive(Debug)]
    struct StubLibraryService;

    #[async_trait::async_trait]
    impl crate::services::library::LibraryService for StubLibraryService {
        async fn get_libraries(
            &self,
            _user_id: String,
        ) -> Result<Vec<crate::models::Library>, LibraryError> {
            unimplemented!("not called in playback route tests")
        }
        async fn get_library_by_id(
            &self,
            _library_id: String,
        ) -> Result<Option<crate::models::Library>, LibraryError> {
            unimplemented!("not called in playback route tests")
        }
        async fn get_library_files(
            &self,
            _library_id: String,
        ) -> Result<Vec<crate::models::LibraryFile>, LibraryError> {
            unimplemented!("not called in playback route tests")
        }
        async fn get_file_by_id(
            &self,
            _file_id: String,
        ) -> Result<Option<crate::models::LibraryFile>, LibraryError> {
            unimplemented!("not called in playback route tests")
        }
        async fn create_library(
            &self,
            _name: String,
            _path: String,
        ) -> Result<crate::models::Library, LibraryError> {
            unimplemented!("not called in playback route tests")
        }
        async fn scan_library(&self, _library_id: String) -> Result<u32, LibraryError> {
            unimplemented!("not called in playback route tests")
        }
        async fn delete_library(&self, _library_id: String) -> Result<bool, LibraryError> {
            unimplemented!("not called in playback route tests")
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
            unimplemented!("not called in playback route tests")
        }
    }

    struct TestFixture {
        state: AppState,
        session_store: Arc<InMemorySessionStore>,
        user_repo: Arc<InMemoryUserRepository>,
        file_repo: Arc<InMemoryFileRepository>,
        movie_repo: Arc<InMemoryMovieRepository>,
    }

    fn make_test_state() -> TestFixture {
        let session_store = Arc::new(InMemorySessionStore::default());
        let user_repo = Arc::new(InMemoryUserRepository::default());

        let notification = Arc::new(InMemoryNotificationService::new());
        let admin_log: Arc<dyn AdminLogService> = Arc::new(LocalAdminLogService::new(Arc::new(
            InMemoryAdminLogRepository::default(),
        )));

        let file_repo = Arc::new(InMemoryFileRepository::default());
        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let playback: Arc<dyn crate::services::playback::PlaybackService> =
            Arc::new(DbPlaybackService::new(
                Arc::new(InMemoryPlaybackProgressRepository::default()),
                file_repo.clone(),
                movie_repo.clone(),
                show_repo.clone(),
            ));

        let services = AppServices {
            hash: Arc::new(StubHashService),
            library: Arc::new(StubLibraryService),
            metadata: Arc::new(StubMetadataService),
            notification,
            admin_log,
            user_repo: user_repo.clone(),
            playback,
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

        let state = AppState::new(config, services);
        TestFixture {
            state,
            session_store,
            user_repo,
            file_repo,
            movie_repo,
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
        let router = Router::new()
            .hoop(affix_state::inject(fixture.state.clone()))
            .push(
                Router::with_path("v1")
                    .push(
                        Router::with_path("files/{file_id}/progress").put(report_playback_progress),
                    )
                    .push(Router::with_path("continue-watching").get(get_continue_watching)),
            );
        Service::new(router)
    }

    fn make_media_file(content: MediaFileContent) -> MediaFile {
        MediaFile {
            id: uuid::Uuid::new_v4(),
            library_id: uuid::Uuid::new_v4(),
            path: PathBuf::from("/media/test.mp4"),
            hash: 0,
            size_bytes: 1024,
            mtime: None,
            mime_type: Some("video/mp4".to_string()),
            duration: Some(std::time::Duration::from_secs(7200)),
            container_format: Some("mp4".to_string()),
            content: Some(content),
            status: beam_domain::models::FileStatus::Known,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_report_progress_requires_auth() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let res = TestClient::put(format!(
            "http://localhost/v1/files/{}/progress",
            uuid::Uuid::new_v4()
        ))
        .json(&ReportProgressRequest {
            position_secs: 10.0,
            duration_secs: Some(100.0),
        })
        .send(&service)
        .await;
        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn test_report_progress_unknown_file_returns_404() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let cookie = seed_session_cookie(&fixture).await;

        let res = TestClient::put(format!(
            "http://localhost/v1/files/{}/progress",
            uuid::Uuid::new_v4()
        ))
        .add_header("Cookie", cookie, true)
        .json(&ReportProgressRequest {
            position_secs: 10.0,
            duration_secs: Some(100.0),
        })
        .send(&service)
        .await;
        assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
    }

    #[tokio::test]
    async fn test_report_progress_known_file_returns_200_with_dto() {
        let fixture = make_test_state();
        let file = make_media_file(MediaFileContent::Movie {
            movie_entry_id: uuid::Uuid::new_v4(),
        });
        let file_id = file.id;
        fixture
            .file_repo
            .files
            .lock()
            .unwrap()
            .insert(file.id, file);

        let service = build_service(&fixture);
        let cookie = seed_session_cookie(&fixture).await;

        let mut res = TestClient::put(format!("http://localhost/v1/files/{file_id}/progress"))
            .add_header("Cookie", cookie, true)
            .json(&ReportProgressRequest {
                position_secs: 42.0,
                duration_secs: Some(100.0),
            })
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::OK));
        let body: PlaybackProgressDto = res.take_json().await.unwrap();
        assert_eq!(body.position_secs, 42.0);
        assert!(!body.completed);
    }

    #[tokio::test]
    async fn test_continue_watching_requires_auth() {
        let fixture = make_test_state();
        let service = build_service(&fixture);
        let res = TestClient::get("http://localhost/v1/continue-watching")
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn test_continue_watching_returns_reported_movie() {
        let fixture = make_test_state();

        let movie = Movie {
            id: uuid::Uuid::new_v4(),
            title: "Test Movie".to_string(),
            title_localized: None,
            description: None,
            year: None,
            release_date: None,
            runtime: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            rating_tmdb: None,
            rating_imdb: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let movie_id = movie.id;
        fixture
            .movie_repo
            .movies
            .lock()
            .unwrap()
            .insert(movie.id, movie);

        let entry = MovieEntry {
            id: uuid::Uuid::new_v4(),
            library_id: uuid::Uuid::new_v4(),
            movie_id,
            edition: None,
            is_primary: true,
            created_at: chrono::Utc::now(),
        };
        let entry_id = entry.id;
        fixture
            .movie_repo
            .entries
            .lock()
            .unwrap()
            .insert(entry.id, entry);

        let file = make_media_file(MediaFileContent::Movie {
            movie_entry_id: entry_id,
        });
        let file_id = file.id;
        fixture
            .file_repo
            .files
            .lock()
            .unwrap()
            .insert(file.id, file);

        let service = build_service(&fixture);
        let cookie = seed_session_cookie(&fixture).await;

        TestClient::put(format!("http://localhost/v1/files/{file_id}/progress"))
            .add_header("Cookie", cookie.clone(), true)
            .json(&ReportProgressRequest {
                position_secs: 10.0,
                duration_secs: Some(100.0),
            })
            .send(&service)
            .await;

        let mut res = TestClient::get("http://localhost/v1/continue-watching")
            .add_header("Cookie", cookie, true)
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::OK));
        let items: Vec<ContinueWatchingItem> = res.take_json().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].media_id, movie_id.to_string());
        assert_eq!(items[0].media_type, "movie");
    }
}
