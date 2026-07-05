use sea_orm::DatabaseConnection;
use std::ops::Deref;
use std::sync::Arc;

use beam_auth::utils::{
    repository::{SqlUserRepository, UserRepository},
    service::{AuthService, LocalAuthService},
    session_store::PgSessionStore,
};
use beam_domain::providers::enrichment::{EnrichmentProvider, NoopEnrichmentProvider};
use beam_index::providers::cameo::{CameoEnrichmentProvider, CameoWiringConfig};
use beam_index::services::clock::RealClock;
use beam_index::services::enrichment::MetadataEnrichmentService;
use beam_index::services::index::{IndexService, LocalIndexService};

use crate::{
    config::ServerConfig,
    services::{
        admin_log::{AdminLogService, LocalAdminLogService},
        hash::{HashConfig, HashService, LocalHashService},
        library::{LibraryService, LocalLibraryService, OsPathValidator},
        metadata::{DbMetadataService, MetadataService},
        notification::{LocalNotificationService, NotificationService},
        playback::{DbPlaybackService, PlaybackService},
    },
};

#[derive(Clone, Debug)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

#[derive(Debug)]
pub struct AppStateInner {
    pub config: ServerConfig,
    pub services: AppServices,
}

impl AppState {
    pub fn new(config: ServerConfig, services: AppServices) -> Self {
        Self {
            inner: Arc::new(AppStateInner { config, services }),
        }
    }
}

impl Deref for AppState {
    type Target = AppStateInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(Clone, Debug)]
pub struct UserContext {
    pub user_id: String,
}

#[derive(Clone, Debug)]
pub struct AppContextInner {
    pub user_context: Option<UserContext>,
}
pub struct AppContext(Arc<AppContextInner>);

impl AppContext {
    pub fn new(user_context: Option<UserContext>) -> Self {
        Self(Arc::new(AppContextInner { user_context }))
    }

    pub fn user_context(&self) -> Option<UserContext> {
        self.0.user_context.clone()
    }
}

#[derive(Debug)]
pub struct AppServices {
    pub auth: Arc<dyn AuthService>,
    pub hash: Arc<dyn HashService>,
    pub library: Arc<dyn LibraryService>,
    pub metadata: Arc<dyn MetadataService>,
    pub notification: Arc<dyn NotificationService>,
    pub admin_log: Arc<dyn AdminLogService>,
    pub user_repo: Arc<dyn UserRepository>,
    pub playback: Arc<dyn PlaybackService>,
}

impl AppServices {
    /// Build the application's services. Also returns the concrete
    /// [`LocalIndexService`] and [`MetadataEnrichmentService`] (rather than
    /// folding them into `AppServices` itself) so the process entry point can
    /// spawn beam-index's background scan/watch/enrichment tasks via
    /// `beam_index::runtime` -- those need methods beyond the narrow
    /// `IndexService`/`MetadataService` trait objects stored on `library`/
    /// `metadata`. Test fixtures that only need an `AppServices` (not a real
    /// indexer) are unaffected by these extra return values.
    pub async fn new(
        config: &ServerConfig,
        db: DatabaseConnection,
    ) -> eyre::Result<(Self, Arc<LocalIndexService>, Arc<MetadataEnrichmentService>)> {
        let hash_config = HashConfig::default();

        // Create repository implementations
        let library_repo = Arc::new(beam_index::repositories::SqlLibraryRepository::new(
            db.clone(),
        ));
        let file_repo: Arc<dyn beam_domain::repositories::FileRepository> =
            Arc::new(beam_index::repositories::SqlFileRepository::new(db.clone()));
        let movie_repo: Arc<dyn beam_domain::repositories::MovieRepository> = Arc::new(
            beam_index::repositories::SqlMovieRepository::new(db.clone()),
        );
        let show_repo: Arc<dyn beam_domain::repositories::ShowRepository> =
            Arc::new(beam_index::repositories::SqlShowRepository::new(db.clone()));
        let stream_repo: Arc<dyn beam_domain::repositories::MediaStreamRepository> = Arc::new(
            beam_index::repositories::SqlMediaStreamRepository::new(db.clone()),
        );
        let user_repo: Arc<dyn UserRepository> = Arc::new(SqlUserRepository::new(db.clone()));
        let admin_log_repo = Arc::new(beam_index::repositories::SqlAdminLogRepository::new(
            db.clone(),
        ));
        let enrichment_repo: Arc<dyn beam_domain::repositories::EnrichmentStateRepository> =
            Arc::new(beam_index::repositories::SqlEnrichmentStateRepository::new(
                db.clone(),
            ));
        let genre_repo: Arc<dyn beam_domain::repositories::GenreRepository> = Arc::new(
            beam_index::repositories::SqlGenreRepository::new(db.clone()),
        );
        let playback_repo: Arc<dyn beam_domain::repositories::PlaybackProgressRepository> =
            Arc::new(beam_index::repositories::SqlPlaybackProgressRepository::new(db.clone()));

        let notification_service = Arc::new(LocalNotificationService::new());
        let hash_service = Arc::new(LocalHashService::new(hash_config));
        let media_info_service =
            Arc::new(crate::services::media_info::LocalMediaInfoService::default());

        let session_store = Arc::new(PgSessionStore::new(db.clone()));

        let auth_service = Arc::new(LocalAuthService::new(
            user_repo.clone(),
            session_store,
            config.jwt_secret.clone(),
        ));

        let admin_log_service: Arc<dyn AdminLogService> =
            Arc::new(LocalAdminLogService::new(admin_log_repo.clone()));

        let index_service = Arc::new(
            LocalIndexService::new(
                library_repo.clone(),
                file_repo.clone(),
                movie_repo.clone(),
                show_repo.clone(),
                stream_repo.clone(),
                hash_service.clone(),
                media_info_service.clone(),
                notification_service.clone(),
                admin_log_service.clone(),
            )
            .with_hash_unknown_files(config.hash_unknown_files)
            .with_enrichment_repo(enrichment_repo.clone()),
        );

        // Real cameo client when at least TMDB or AniList is configured;
        // NoopEnrichmentProvider (every sweep a fast no-op) otherwise -- e.g.
        // a fresh dev environment with no TMDB_API_TOKEN set.
        let enrichment_provider: Arc<dyn EnrichmentProvider> =
            match beam_index::providers::cameo::build_client(CameoWiringConfig {
                tmdb_api_token: config.tmdb_api_token.clone(),
                anilist_enabled: config.anilist_enabled,
            }) {
                Ok(Some(client)) => Arc::new(CameoEnrichmentProvider::new(client)),
                Ok(None) => Arc::new(NoopEnrichmentProvider),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to build cameo client; metadata enrichment disabled");
                    Arc::new(NoopEnrichmentProvider)
                }
            };

        let enrichment_service = Arc::new(MetadataEnrichmentService::new(
            enrichment_repo.clone(),
            movie_repo.clone(),
            show_repo.clone(),
            genre_repo,
            enrichment_provider,
            admin_log_service.clone(),
            Arc::new(RealClock),
        ));

        let playback_service = Arc::new(DbPlaybackService::new(
            playback_repo,
            file_repo.clone(),
            movie_repo.clone(),
            show_repo.clone(),
        ));

        let services = Self {
            auth: auth_service,
            hash: hash_service.clone() as Arc<dyn HashService>,
            library: Arc::new(LocalLibraryService::new(
                library_repo,
                file_repo.clone(),
                config.video_dir.clone(),
                notification_service.clone(),
                index_service.clone() as Arc<dyn IndexService>,
                Arc::new(OsPathValidator),
            )),
            metadata: Arc::new(
                DbMetadataService::new(movie_repo, show_repo, file_repo, stream_repo)
                    .with_enrichment_repo(enrichment_repo),
            ),
            notification: notification_service,
            admin_log: admin_log_service,
            user_repo,
            playback: playback_service,
        };

        Ok((services, index_service, enrichment_service))
    }
}
