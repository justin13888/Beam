use sea_orm::DatabaseConnection;
use std::ops::Deref;
use std::sync::Arc;

use beam_auth::utils::{
    repository::{SqlUserRepository, UserRepository},
    service::{AuthService, LocalAuthService},
    session_store::RedisSessionStore,
};
use beam_index::services::index::{IndexService, LocalIndexService};

use crate::{
    config::ServerConfig,
    services::{
        admin_log::{AdminLogService, LocalAdminLogService},
        hash::{HashConfig, HashService, LocalHashService},
        library::{LibraryService, LocalLibraryService, OsPathValidator},
        metadata::{DbMetadataService, MetadataService},
        notification::{LocalNotificationService, NotificationService},
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
}

impl AppServices {
    /// Build the application's services. Also returns the concrete
    /// [`LocalIndexService`] (rather than folding it into `AppServices`
    /// itself) so the process entry point can spawn beam-index's background
    /// scan/watch tasks via `beam_index::runtime::spawn_background_indexing`
    /// -- those need methods (`scan_all_libraries`, `reconcile_path`) beyond
    /// the narrow `IndexService` trait object stored on `library`. Test
    /// fixtures that only need an `AppServices` (not a real indexer) are
    /// unaffected by this extra return value.
    pub async fn new(
        config: &ServerConfig,
        db: DatabaseConnection,
    ) -> eyre::Result<(Self, Arc<LocalIndexService>)> {
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

        let notification_service = Arc::new(LocalNotificationService::new());
        let hash_service = Arc::new(LocalHashService::new(hash_config));
        let media_info_service =
            Arc::new(crate::services::media_info::LocalMediaInfoService::default());

        // Initialize Redis session store
        let session_store = Arc::new(
            RedisSessionStore::new(&config.redis_url)
                .await
                .expect("Failed to connect to Redis"),
        );

        let auth_service = Arc::new(LocalAuthService::new(
            user_repo.clone(),
            session_store,
            config.jwt_secret.clone(),
        ));

        let admin_log_service: Arc<dyn AdminLogService> =
            Arc::new(LocalAdminLogService::new(admin_log_repo));

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
            .with_hash_unknown_files(config.hash_unknown_files),
        );

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
            metadata: Arc::new(DbMetadataService::new(
                movie_repo,
                show_repo,
                file_repo,
                stream_repo,
            )),
            notification: notification_service,
            admin_log: admin_log_service,
            user_repo,
        };

        Ok((services, index_service))
    }
}
