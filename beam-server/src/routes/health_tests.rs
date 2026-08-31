//! Subcutaneous tests for the dependency-aware `/v1/health` endpoint.
//!
//! The endpoint only touches [`AppState::probe`] and [`AppState::uptime_secs`],
//! so these build a full [`AppState`] with cheap stub services and drive the
//! handler through `salvo::test::TestClient`. No Postgres, no Docker.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use beam_auth::utils::oidc_config::OidcRuntimeConfig;
    use beam_auth::utils::{
        oidc::NotConfiguredOidcClient, pending_auth_store::in_memory::InMemoryPendingAuthStore,
        repository::in_memory::InMemoryUserRepository,
        session_store::in_memory::InMemorySessionStore,
    };
    use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
    use salvo::prelude::*;
    use salvo::test::{ResponseExt, TestClient};
    use serde_json::Value;

    use crate::routes::health_check;
    use crate::services::admin_log::{AdminLogService, LocalAdminLogService};
    use crate::services::hash::HashService;
    use crate::services::health::{DependencyProbe, InMemoryDependencyProbe};
    use crate::services::library::LibraryError;
    use crate::services::metadata::{
        MediaConnection, MediaFilter, MediaSearchFilters, MediaSortField, MetadataError,
        MetadataService, PageInfo, SortOrder,
    };
    use crate::services::notification::InMemoryNotificationService;
    use crate::services::playback::DbPlaybackService;
    use crate::state::{AppServices, AppState};

    #[derive(Debug)]
    struct StubHashService;

    #[async_trait::async_trait]
    impl HashService for StubHashService {
        fn hash_sync(&self, _path: &std::path::Path) -> std::io::Result<u64> {
            unimplemented!("not called in health tests")
        }
        async fn hash_async(&self, _path: PathBuf) -> std::io::Result<u64> {
            unimplemented!("not called in health tests")
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
            unimplemented!("not called in health tests")
        }
        async fn get_library_by_id(
            &self,
            _library_id: String,
        ) -> Result<Option<crate::models::Library>, LibraryError> {
            unimplemented!("not called in health tests")
        }
        async fn get_library_files(
            &self,
            _library_id: String,
        ) -> Result<Vec<crate::models::LibraryFile>, LibraryError> {
            unimplemented!("not called in health tests")
        }
        async fn get_file_by_id(
            &self,
            _file_id: String,
        ) -> Result<Option<crate::models::LibraryFile>, LibraryError> {
            unimplemented!("not called in health tests")
        }
        async fn create_library(
            &self,
            _name: String,
            _path: String,
        ) -> Result<crate::models::Library, LibraryError> {
            unimplemented!("not called in health tests")
        }
        async fn scan_library(&self, _library_id: String) -> Result<u32, LibraryError> {
            unimplemented!("not called in health tests")
        }
        async fn delete_library(&self, _library_id: String) -> Result<bool, LibraryError> {
            unimplemented!("not called in health tests")
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
            unimplemented!("not called in health tests")
        }
    }

    fn make_state(probe: Arc<dyn DependencyProbe>) -> AppState {
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
                file_repo,
                movie_repo,
                show_repo,
            ));

        let services = AppServices {
            hash: Arc::new(StubHashService),
            library: Arc::new(StubLibraryService),
            metadata: Arc::new(StubMetadataService),
            notification,
            admin_log,
            user_repo: Arc::new(InMemoryUserRepository::default()),
            playback,
            genre_repo: Arc::new(
                beam_domain::repositories::genre::in_memory::InMemoryGenreRepository::default(),
            ),
            library_repo: Arc::new(
                beam_domain::repositories::library::in_memory::InMemoryLibraryRepository::default(),
            ),
            file_repo: Arc::new(InMemoryFileRepository::default()),
            enrichment_repo: Arc::new(
                beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository::default(),
            ),
            session_store: Arc::new(InMemorySessionStore::default()),
            oidc_client: Arc::new(NotConfiguredOidcClient::new("not used in health tests")),
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
            video_dir: PathBuf::from("/tmp"),
            data_dir: PathBuf::from("/tmp"),
            database_url: "postgres://unused:unused@localhost/unused".to_string(),
            watch_enabled: false,
            anilist_enabled: false,
            cookie_secure: Some(false),
            ..Default::default()
        };

        AppState::new(config, services, probe, None)
    }

    fn service(probe: Arc<dyn DependencyProbe>) -> Service {
        let router = Router::new()
            .hoop(affix_state::inject(make_state(probe)))
            .push(Router::with_path("v1").push(Router::with_path("health").get(health_check)));
        Service::new(router)
    }

    #[tokio::test]
    async fn healthy_database_yields_200_with_ok_check_and_uptime() {
        let service = service(Arc::new(InMemoryDependencyProbe::healthy()));

        let mut res = TestClient::get("http://localhost/v1/health")
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::OK));

        let body: Value = res.take_json().await.unwrap();
        assert_eq!(body["status"], "healthy");
        assert_eq!(body["checks"]["database"], "ok");
        assert!(body["uptime_secs"].is_u64(), "uptime_secs must be present");
        assert!(body["version"].is_string());
        assert!(body["timestamp"].is_string());
    }

    #[tokio::test]
    async fn failing_database_yields_503_degraded_with_error_surfaced() {
        let service = service(Arc::new(InMemoryDependencyProbe::failing(
            "connection refused",
        )));

        let mut res = TestClient::get("http://localhost/v1/health")
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::SERVICE_UNAVAILABLE));

        let body: Value = res.take_json().await.unwrap();
        assert_eq!(body["status"], "degraded");
        assert_eq!(body["checks"]["database"], "error: connection refused");
        assert!(body["uptime_secs"].is_u64());
    }
}
