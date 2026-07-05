/// Subcutaneous HTTP tests for the `/v1/media` REST routes.
///
/// These tests spin up the full Salvo service with in-memory implementations
/// for all external dependencies -- no Redis, no PostgreSQL required.
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use beam_auth::server::OidcRuntimeConfig;
    use beam_auth::utils::{
        oidc::NotConfiguredOidcClient,
        pending_auth_store::in_memory::InMemoryPendingAuthStore,
        repository::in_memory::InMemoryUserRepository,
        service::{AuthService, LocalAuthService},
        session_store::in_memory::InMemorySessionStore,
    };
    use salvo::prelude::*;
    use salvo::test::{ResponseExt, TestClient};

    use crate::models::{LibraryFile, MediaMetadata, MediaSource, MovieMetadata, Title};
    use crate::routes::{browse_media, get_media_detail, get_media_sources};
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

    const TEST_JWT_SECRET: &str = "test-jwt-secret-for-media-route-tests";
    const MOVIE_ID: &str = "22222222-2222-2222-2222-222222222222";

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
            unimplemented!("not called in media route tests")
        }

        async fn get_continue_watching(
            &self,
            _user_id: uuid::Uuid,
            _limit: u32,
        ) -> Result<Vec<ContinueWatchingItem>, PlaybackError> {
            unimplemented!("not called in media route tests")
        }
    }

    #[derive(Debug)]
    struct StubHashService;

    #[async_trait::async_trait]
    impl HashService for StubHashService {
        fn hash_sync(&self, _path: &std::path::Path) -> std::io::Result<u64> {
            unimplemented!("not called in media route tests")
        }
        async fn hash_async(&self, _path: PathBuf) -> std::io::Result<u64> {
            unimplemented!("not called in media route tests")
        }
    }

    #[derive(Debug)]
    struct StubLibraryService;

    #[async_trait::async_trait]
    impl LibraryService for StubLibraryService {
        async fn get_libraries(
            &self,
            _user_id: String,
        ) -> Result<Vec<crate::models::Library>, LibraryError> {
            unimplemented!("not called in media route tests")
        }
        async fn get_library_by_id(
            &self,
            _library_id: String,
        ) -> Result<Option<crate::models::Library>, LibraryError> {
            unimplemented!("not called in media route tests")
        }
        async fn get_library_files(
            &self,
            _library_id: String,
        ) -> Result<Vec<LibraryFile>, LibraryError> {
            unimplemented!("not called in media route tests")
        }
        async fn get_file_by_id(
            &self,
            _file_id: String,
        ) -> Result<Option<LibraryFile>, LibraryError> {
            unimplemented!("not called in media route tests")
        }
        async fn create_library(
            &self,
            _name: String,
            _path: String,
        ) -> Result<crate::models::Library, LibraryError> {
            unimplemented!("not called in media route tests")
        }
        async fn scan_library(&self, _library_id: String) -> Result<u32, LibraryError> {
            unimplemented!("not called in media route tests")
        }
        async fn delete_library(&self, _library_id: String) -> Result<bool, LibraryError> {
            unimplemented!("not called in media route tests")
        }
    }

    /// Configurable metadata stub: tests populate `metadata`/`sources` to
    /// drive the browse/detail/sources endpoints under test.
    #[derive(Debug, Default)]
    struct StubMetadataService {
        metadata: HashMap<String, MediaMetadata>,
        sources: HashMap<String, Vec<MediaSource>>,
        unsupported: HashMap<String, String>,
    }

    #[async_trait::async_trait]
    impl MetadataService for StubMetadataService {
        async fn get_media_metadata(&self, media_id: &str) -> Option<MediaMetadata> {
            self.metadata.get(media_id).cloned()
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
            media_id: &str,
        ) -> Result<Vec<MediaSource>, MetadataError> {
            if let Some(msg) = self.unsupported.get(media_id) {
                return Err(MetadataError::Unsupported(msg.clone()));
            }
            self.sources
                .get(media_id)
                .cloned()
                .ok_or(MetadataError::MediaNotFound)
        }
    }

    struct TestFixture {
        state: AppState,
        auth: Arc<LocalAuthService>,
    }

    fn make_test_state(metadata_service: StubMetadataService) -> TestFixture {
        let session_store = Arc::new(InMemorySessionStore::default());
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let auth = Arc::new(LocalAuthService::new(
            user_repo.clone(),
            session_store.clone(),
            TEST_JWT_SECRET.to_string(),
        ));

        let notification = Arc::new(InMemoryNotificationService::new());
        let admin_log: Arc<dyn AdminLogService> = Arc::new(LocalAdminLogService::new(Arc::new(
            InMemoryAdminLogRepository::default(),
        )));

        let services = AppServices {
            auth: auth.clone(),
            hash: Arc::new(StubHashService),
            library: Arc::new(StubLibraryService),
            metadata: Arc::new(metadata_service),
            notification,
            admin_log,
            user_repo: user_repo.clone(),
            playback: Arc::new(StubPlaybackService),
            session_store,
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
        };

        let state = AppState::new(config, services);
        TestFixture { state, auth }
    }

    async fn register_and_get_token(auth: &LocalAuthService) -> String {
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
        resp.token
    }

    fn build_service(fixture: &TestFixture) -> Service {
        let router = Router::new()
            .hoop(affix_state::inject(fixture.state.clone()))
            .push(
                Router::with_path("v1").push(
                    Router::with_path("media")
                        .get(browse_media)
                        .push(Router::with_path("{id}").get(get_media_detail))
                        .push(Router::with_path("{id}/sources").get(get_media_sources)),
                ),
            );
        Service::new(router)
    }

    fn movie_metadata(id: &str, title: &str) -> MediaMetadata {
        MediaMetadata::Movie(MovieMetadata {
            id: id.to_string(),
            title: Title {
                original: title.to_string(),
                localized: None,
                alternatives: None,
            },
            description: None,
            year: Some(1999),
            release_date: None,
            runtime: Some(136),
            duration: Some(8160.0),
            poster_url: None,
            backdrop_url: None,
            genres: vec![],
            ratings: None,
            identifiers: None,
            streams: vec![],
            file_id: None,
        })
    }

    fn movie_source(file_id: &str) -> MediaSource {
        MediaSource {
            file_id: file_id.to_string(),
            size_bytes: 1_000_000,
            mime_type: Some("video/mp4".to_string()),
            container_format: Some("mp4".to_string()),
            duration_secs: Some(8160.0),
            video: None,
            audio_tracks: vec![],
            stream_url: format!("/v1/files/{file_id}/stream"),
            download_url: format!("/v1/files/{file_id}/download"),
        }
    }

    // ─── GET /v1/media/:id ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_media_detail_returns_metadata_for_known_id() {
        let mut stub = StubMetadataService::default();
        stub.metadata
            .insert(MOVIE_ID.to_string(), movie_metadata(MOVIE_ID, "The Matrix"));
        let fixture = make_test_state(stub);
        let service = build_service(&fixture);
        let token = register_and_get_token(&fixture.auth).await;

        let mut res = TestClient::get(format!("http://localhost/v1/media/{MOVIE_ID}"))
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));
        let body: MediaMetadata = res.take_json().await.unwrap();
        assert_eq!(body.title().original, "The Matrix");
    }

    #[tokio::test]
    async fn test_get_media_detail_unknown_id_returns_404() {
        let fixture = make_test_state(StubMetadataService::default());
        let service = build_service(&fixture);
        let token = register_and_get_token(&fixture.auth).await;

        let res = TestClient::get(format!("http://localhost/v1/media/{MOVIE_ID}"))
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
    }

    #[tokio::test]
    async fn test_get_media_detail_missing_auth_header_returns_401() {
        let fixture = make_test_state(StubMetadataService::default());
        let service = build_service(&fixture);

        let res = TestClient::get(format!("http://localhost/v1/media/{MOVIE_ID}"))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn test_get_media_detail_invalid_token_returns_401() {
        let fixture = make_test_state(StubMetadataService::default());
        let service = build_service(&fixture);

        let res = TestClient::get(format!("http://localhost/v1/media/{MOVIE_ID}"))
            .add_header("Authorization", "Bearer not-a-real-token", true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    // ─── GET /v1/media ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_browse_media_requires_auth() {
        let fixture = make_test_state(StubMetadataService::default());
        let service = build_service(&fixture);

        let res = TestClient::get("http://localhost/v1/media")
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn test_browse_media_authenticated_returns_connection() {
        let fixture = make_test_state(StubMetadataService::default());
        let service = build_service(&fixture);
        let token = register_and_get_token(&fixture.auth).await;

        let mut res = TestClient::get("http://localhost/v1/media?sort_by=year&sort_order=desc")
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));
        let body: MediaConnection = res.take_json().await.unwrap();
        assert!(body.edges.is_empty());
        assert!(!body.page_info.has_next_page);
    }

    // ─── GET /v1/media/:id/sources ──────────────────────────────────────────

    #[tokio::test]
    async fn test_get_media_sources_returns_files_for_movie() {
        let mut stub = StubMetadataService::default();
        stub.sources.insert(
            MOVIE_ID.to_string(),
            vec![movie_source("33333333-3333-3333-3333-333333333333")],
        );
        let fixture = make_test_state(stub);
        let service = build_service(&fixture);
        let token = register_and_get_token(&fixture.auth).await;

        let mut res = TestClient::get(format!("http://localhost/v1/media/{MOVIE_ID}/sources"))
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));
        let body: Vec<MediaSource> = res.take_json().await.unwrap();
        assert_eq!(body.len(), 1);
        assert_eq!(
            body[0].stream_url,
            "/v1/files/33333333-3333-3333-3333-333333333333/stream"
        );
        assert_eq!(
            body[0].download_url,
            "/v1/files/33333333-3333-3333-3333-333333333333/download"
        );
    }

    #[tokio::test]
    async fn test_get_media_sources_unknown_id_returns_404() {
        let fixture = make_test_state(StubMetadataService::default());
        let service = build_service(&fixture);
        let token = register_and_get_token(&fixture.auth).await;

        let res = TestClient::get(format!("http://localhost/v1/media/{MOVIE_ID}/sources"))
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
    }

    #[tokio::test]
    async fn test_get_media_sources_show_id_returns_400() {
        const SHOW_ID: &str = "44444444-4444-4444-4444-444444444444";
        let mut stub = StubMetadataService::default();
        stub.unsupported.insert(
            SHOW_ID.to_string(),
            "sources are not available at the show level; use an episode id".to_string(),
        );
        let fixture = make_test_state(stub);
        let service = build_service(&fixture);
        let token = register_and_get_token(&fixture.auth).await;

        let res = TestClient::get(format!("http://localhost/v1/media/{SHOW_ID}/sources"))
            .add_header("Authorization", format!("Bearer {token}"), true)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn test_get_media_sources_missing_auth_header_returns_401() {
        let fixture = make_test_state(StubMetadataService::default());
        let service = build_service(&fixture);

        let res = TestClient::get(format!("http://localhost/v1/media/{MOVIE_ID}/sources"))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }
}
