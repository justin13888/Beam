//! Shared test-only builders for routing tests that need a full [`AppState`]
//! but exercise none of the media services: every service is a stub or an
//! in-memory fake, so tests stay zero-dependency (no Postgres, no Docker).

use std::path::PathBuf;
use std::sync::Arc;

use beam_auth::utils::oidc_config::OidcRuntimeConfig;
use beam_auth::utils::{
    oidc::NotConfiguredOidcClient, pending_auth_store::in_memory::InMemoryPendingAuthStore,
    repository::in_memory::InMemoryUserRepository, session_store::in_memory::InMemorySessionStore,
};
use beam_domain::providers::artwork::test_utils::InMemoryArtworkFetcher;
use beam_domain::providers::telemetry::RecordingTelemetrySink;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library_shape::in_memory::InMemoryLibraryShapeRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository;
use beam_domain::repositories::playback_telemetry::in_memory::InMemoryPlaybackTelemetryRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;

use beam_domain::providers::enrichment::{EnrichmentProvider, NoopEnrichmentProvider};
use beam_domain::repositories::{
    EnrichmentStateRepository, LibraryRepository, MovieRepository, ShowRepository,
};
use beam_index::services::enrichment::control::{
    EnrichmentControl, EnrichmentControlDeps, TitleNfoPins,
};

use crate::services::admin_log::{AdminLogService, LocalAdminLogService};
use crate::services::artwork::{ArtworkCache, ArtworkCacheConfig};
use crate::services::hash::HashService;
use crate::services::health::InMemoryDependencyProbe;
use crate::services::library::LibraryError;
use crate::services::metadata::{MediaConnection, MetadataError, MetadataService, PageInfo};
use crate::services::notification::InMemoryNotificationService;
use crate::services::playback::DbPlaybackService;
use crate::services::playback_telemetry::{PlaybackTelemetryConfig, PlaybackTelemetryService};
use crate::services::telemetry::{LibraryReportConfig, LibraryReportService};
use crate::state::{AppServices, AppState};

#[derive(Debug)]
struct StubHashService;

#[async_trait::async_trait]
impl HashService for StubHashService {
    fn hash_sync(&self, _path: &std::path::Path) -> std::io::Result<u64> {
        unimplemented!("not called in routing tests")
    }
    async fn hash_async(&self, _path: PathBuf) -> std::io::Result<u64> {
        unimplemented!("not called in routing tests")
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
        unimplemented!("not called in routing tests")
    }
    async fn get_library_by_id(
        &self,
        _library_id: String,
    ) -> Result<Option<crate::models::Library>, LibraryError> {
        unimplemented!("not called in routing tests")
    }
    async fn get_library_files(
        &self,
        _library_id: String,
    ) -> Result<Vec<crate::models::LibraryFile>, LibraryError> {
        unimplemented!("not called in routing tests")
    }
    async fn get_file_by_id(
        &self,
        _file_id: String,
    ) -> Result<Option<crate::services::library::LocatedFile>, LibraryError> {
        unimplemented!("not called in routing tests")
    }
    async fn create_library(
        &self,
        _name: String,
        _path: String,
    ) -> Result<crate::models::Library, LibraryError> {
        unimplemented!("not called in routing tests")
    }
    async fn start_scan(
        &self,
        _library_id: uuid::Uuid,
    ) -> Result<crate::models::ScanJob, LibraryError> {
        unimplemented!("not called in routing tests")
    }
    async fn get_scan(
        &self,
        _library_id: uuid::Uuid,
    ) -> Result<Option<crate::models::ScanJob>, LibraryError> {
        unimplemented!("not called in routing tests")
    }
    async fn delete_library(&self, _library_id: String) -> Result<bool, LibraryError> {
        unimplemented!("not called in routing tests")
    }
}

#[derive(Debug, Default)]
struct StubMetadataService;

#[async_trait::async_trait]
impl MetadataService for StubMetadataService {
    async fn get_media_metadata(
        &self,
        _media_id: uuid::Uuid,
    ) -> Result<Option<crate::models::MediaMetadata>, MetadataError> {
        Ok(None)
    }
    async fn search_media(
        &self,
        _request: crate::services::metadata::BrowseRequest,
    ) -> Result<MediaConnection, MetadataError> {
        Ok(MediaConnection {
            items: vec![],
            page_info: PageInfo {
                has_next_page: false,
                has_previous_page: false,
                start_cursor: None,
                end_cursor: None,
            },
        })
    }
    async fn get_media_sources(
        &self,
        _media_id: &str,
    ) -> Result<Vec<crate::models::MediaSource>, MetadataError> {
        unimplemented!("not called in routing tests")
    }
}

/// A defaults-shaped [`AppState`] over stub services and a healthy in-memory
/// dependency probe, for tests that exercise router wiring rather than any
/// individual service.
pub(crate) fn make_app_state() -> AppState {
    make_app_state_with(|_| {})
}

/// [`make_app_state`] with the configuration adjusted, on the real clock.
///
/// `adjust` mutates the defaults-shaped config in place so a test names only
/// the fields it cares about.
pub(crate) fn make_app_state_with(
    adjust: impl FnOnce(&mut crate::config::ServerConfig),
) -> AppState {
    make_app_state_with_clock(adjust, Arc::new(beam_domain::services::RealClock))
}

/// [`make_app_state_with`] with the clock chosen too.
///
/// `clock` lets a test move time -- `uptime_secs` is otherwise always zero and
/// unassertable.
pub(crate) fn make_app_state_with_clock(
    adjust: impl FnOnce(&mut crate::config::ServerConfig),
    clock: Arc<dyn beam_domain::services::Clock>,
) -> AppState {
    make_app_state_full(
        adjust,
        clock,
        Arc::new(InMemoryDependencyProbe::healthy()),
        None,
    )
}

/// [`make_app_state_with`] with the dependency probe chosen too.
///
/// `/v1/health` reports what the probe says, so a test that wants a degraded
/// answer configures the probe to fail rather than breaking a real dependency
/// (NFR-205).
pub(crate) fn make_app_state_with_probe(
    probe: Arc<dyn crate::services::health::DependencyProbe>,
) -> AppState {
    make_app_state_full(
        |_| {},
        Arc::new(beam_domain::services::RealClock),
        probe,
        None,
    )
}

/// An artwork cache that serves nothing, for the fixtures that do not exercise
/// artwork.
///
/// Cold and never written to: the fetcher has no images, so every request is a
/// 404 and the directory is never touched -- which is why naming one that does
/// not exist is safe rather than sloppy.
pub(crate) fn cold_artwork_cache() -> Arc<ArtworkCache> {
    Arc::new(ArtworkCache::new(
        ArtworkCacheConfig {
            root: PathBuf::from("/nonexistent/artwork"),
            max_bytes: 0,
            negative_ttl: std::time::Duration::from_secs(300),
        },
        Arc::new(InMemoryArtworkFetcher::new()),
        Arc::new(beam_domain::services::RealClock),
    ))
}

/// The indexer's re-pin of a title by its NFOs, for fixtures with no library
/// on disk: there is no NFO to read, so it re-pins nothing -- a cleared title
/// is left unpinned, as one with no NFO is. The re-pin itself is tested with
/// the indexer, over real NFOs (`beam-index`).
#[derive(Debug, Default)]
pub(crate) struct NoNfoPins;

#[async_trait::async_trait]
impl TitleNfoPins for NoNfoPins {
    async fn repin_from_nfos(
        &self,
        _target: beam_domain::models::enrichment::EnrichmentTargetId,
    ) -> Result<(), beam_index::services::index::IndexError> {
        Ok(())
    }
}

/// An enrichment control over the given stores and provider, with a worker
/// wake-up nothing listens to.
pub(crate) fn enrichment_control(
    movies: Arc<dyn MovieRepository>,
    shows: Arc<dyn ShowRepository>,
    states: Arc<dyn EnrichmentStateRepository>,
    libraries: Arc<dyn LibraryRepository>,
    provider: Arc<dyn EnrichmentProvider>,
    admin_log: Arc<dyn AdminLogService>,
) -> Arc<EnrichmentControl> {
    Arc::new(EnrichmentControl::new(EnrichmentControlDeps {
        movies,
        shows,
        states,
        libraries,
        provider,
        admin_log,
        nfo_pins: Arc::new(NoNfoPins),
        worker: Arc::new(tokio::sync::Notify::new()),
    }))
}

/// A library report with no destination over an empty store: previewable,
/// never scheduled, and -- having no collector -- never sent.
pub(crate) fn idle_library_report() -> Arc<LibraryReportService> {
    Arc::new(LibraryReportService::new(
        LibraryReportConfig {
            destination: None,
            destination_origin: None,
            state_path: PathBuf::from("/nonexistent/telemetry/library-report.json"),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        Arc::new(InMemoryLibraryShapeRepository::default()),
        Arc::new(RecordingTelemetrySink::new()),
        Arc::new(beam_domain::services::RealClock),
    ))
}

/// Playback telemetry that is switched off, over empty in-memory stores:
/// every report is refused with 409, and the admin report reads nothing.
pub(crate) fn idle_playback_telemetry() -> Arc<PlaybackTelemetryService> {
    Arc::new(PlaybackTelemetryService::new(
        PlaybackTelemetryConfig {
            enabled: false,
            retention_days: 365,
        },
        Arc::new(InMemoryPlaybackTelemetryRepository::default()),
        Arc::new(InMemoryFileRepository::default()),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(beam_domain::services::RealClock),
    ))
}

/// Every seam at once. The three wrappers above name the one they vary.
pub(crate) fn make_app_state_full(
    adjust: impl FnOnce(&mut crate::config::ServerConfig),
    clock: Arc<dyn beam_domain::services::Clock>,
    probe: Arc<dyn crate::services::health::DependencyProbe>,
    metrics: Option<metrics_exporter_prometheus::PrometheusHandle>,
) -> AppState {
    make_app_state_with_telemetry(adjust, clock, probe, metrics, idle_library_report())
}

/// [`make_app_state_with_telemetry`] with the playback telemetry service
/// chosen too.
pub(crate) fn make_app_state_with_playback_telemetry(
    adjust: impl FnOnce(&mut crate::config::ServerConfig),
    clock: Arc<dyn beam_domain::services::Clock>,
    playback_telemetry: Arc<PlaybackTelemetryService>,
) -> AppState {
    make_app_state_with_services(
        adjust,
        clock,
        Arc::new(InMemoryDependencyProbe::healthy()),
        None,
        idle_library_report(),
        playback_telemetry,
        Arc::new(NoopEnrichmentProvider),
    )
}

/// [`make_app_state_full`] with the library report chosen too.
pub(crate) fn make_app_state_with_telemetry(
    adjust: impl FnOnce(&mut crate::config::ServerConfig),
    clock: Arc<dyn beam_domain::services::Clock>,
    probe: Arc<dyn crate::services::health::DependencyProbe>,
    metrics: Option<metrics_exporter_prometheus::PrometheusHandle>,
    telemetry: Arc<LibraryReportService>,
) -> AppState {
    make_app_state_with_services(
        adjust,
        clock,
        probe,
        metrics,
        telemetry,
        idle_playback_telemetry(),
        Arc::new(NoopEnrichmentProvider),
    )
}

/// [`make_app_state`] over the enrichment provider `provider`: its stores
/// are `state.services`' own, so a test acts through the routes and reads
/// back through the repositories.
pub(crate) fn make_app_state_with_enrichment(provider: Arc<dyn EnrichmentProvider>) -> AppState {
    make_app_state_with_services(
        |_| {},
        Arc::new(beam_domain::services::RealClock),
        Arc::new(InMemoryDependencyProbe::healthy()),
        None,
        idle_library_report(),
        idle_playback_telemetry(),
        provider,
    )
}

/// Every seam, both telemetry services included.
fn make_app_state_with_services(
    adjust: impl FnOnce(&mut crate::config::ServerConfig),
    clock: Arc<dyn beam_domain::services::Clock>,
    probe: Arc<dyn crate::services::health::DependencyProbe>,
    metrics: Option<metrics_exporter_prometheus::PrometheusHandle>,
    telemetry: Arc<LibraryReportService>,
    playback_telemetry: Arc<PlaybackTelemetryService>,
    enrichment_provider: Arc<dyn EnrichmentProvider>,
) -> AppState {
    let notification = Arc::new(InMemoryNotificationService::new());
    let admin_log: Arc<dyn AdminLogService> = Arc::new(LocalAdminLogService::new(Arc::new(
        InMemoryAdminLogRepository::default(),
    )));

    let file_repo = Arc::new(InMemoryFileRepository::default());
    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let show_repo = Arc::new(InMemoryShowRepository::default());
    let playback: Arc<dyn crate::services::playback::PlaybackService> =
        Arc::new(DbPlaybackService::new(
            Arc::new(InMemoryPlaybackProgressRepository::new(
                clock.clone(),
                file_repo.clone(),
            )),
            file_repo,
            movie_repo.clone(),
            show_repo.clone(),
        ));
    let artwork = cold_artwork_cache();
    let library_repo = Arc::new(
        beam_domain::repositories::library::in_memory::InMemoryLibraryRepository::default(),
    );
    let enrichment_repo = Arc::new(
        beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository::default(),
    );
    let enrichment_control = enrichment_control(
        movie_repo.clone(),
        show_repo.clone(),
        enrichment_repo.clone(),
        library_repo.clone(),
        enrichment_provider,
        admin_log.clone(),
    );

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
        library_repo,
        file_repo: Arc::new(InMemoryFileRepository::default()),
        enrichment_repo,
        enrichment_control,
        movie_repo,
        show_repo,
        artwork,
        session_store: Arc::new(InMemorySessionStore::default()),
        oidc_client: Arc::new(NotConfiguredOidcClient::new("not used in routing tests")),
        pending_auth_store: Arc::new(InMemoryPendingAuthStore::default()),
        device_auth_store: Arc::new(
            beam_auth::utils::device_auth_store::in_memory::InMemoryDeviceAuthStore::default(),
        ),
        oidc_config: OidcRuntimeConfig {
            web_url: "http://localhost:5173".to_string(),
            cookie_secure: false,
            admin_claim: None,
            admin_value: None,
            session_idle_days: 14,
            session_max_days: 60,
        },
        watch_status: Arc::new(beam_index::services::watch_status::WatchStatus::new()),
        telemetry,
        playback_telemetry,
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

    let mut config = config;
    adjust(&mut config);

    AppState::with_clock(config, services, probe, clock, metrics)
}
