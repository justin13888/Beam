use sea_orm::DatabaseConnection;
use std::ops::Deref;
use std::sync::Arc;

use beam_auth::utils::{
    repository::{SqlUserRepository, UserRepository},
    service::{AuthService, LocalAuthService},
    session_store::RedisSessionStore,
};
use beam_index::services::index::IndexService;

use crate::{
    config::ServerConfig,
    services::{
        GrpcIndexService,
        admin_log::{AdminLogService, LocalAdminLogService},
        hash::{HashConfig, HashService, LocalHashService},
        library::{LibraryService, LocalLibraryService},
        metadata::{DbMetadataService, MetadataService},
        notification::{LocalNotificationService, NotificationService},
        transcode::{LocalMp4Generator, LocalTranscodeService, TranscodeService},
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
    pub transcode: Arc<dyn TranscodeService>,
    pub notification: Arc<dyn NotificationService>,
    pub admin_log: Arc<dyn AdminLogService>,
    pub user_repo: Arc<dyn UserRepository>,
}

impl AppServices {
    pub async fn new(config: &ServerConfig, db: DatabaseConnection) -> eyre::Result<Self> {
        let hash_config = HashConfig::default();

        // Create repository implementations
        let library_repo = Arc::new(crate::repositories::SqlLibraryRepository::new(db.clone()));
        let file_repo: Arc<dyn crate::repositories::FileRepository> =
            Arc::new(crate::repositories::SqlFileRepository::new(db.clone()));
        let movie_repo: Arc<dyn crate::repositories::MovieRepository> =
            Arc::new(crate::repositories::SqlMovieRepository::new(db.clone()));
        let show_repo: Arc<dyn crate::repositories::ShowRepository> =
            Arc::new(crate::repositories::SqlShowRepository::new(db.clone()));
        let stream_repo: Arc<dyn crate::repositories::MediaStreamRepository> = Arc::new(
            crate::repositories::SqlMediaStreamRepository::new(db.clone()),
        );
        let user_repo: Arc<dyn UserRepository> = Arc::new(SqlUserRepository::new(db.clone()));
        let admin_log_repo = Arc::new(crate::repositories::SqlAdminLogRepository::new(db.clone()));

        let notification_service = Arc::new(LocalNotificationService::new());
        let hash_service = Arc::new(LocalHashService::new(hash_config));
        let media_info_service =
            Arc::new(crate::services::media_info::LocalMediaInfoService::default());
        let mp4_generator = Arc::new(LocalMp4Generator::new(
            hash_service.clone(),
            media_info_service.clone(),
        ));
        let transcode_service = Arc::new(LocalTranscodeService::new(mp4_generator));

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

        let index_service: Arc<dyn IndexService> = Arc::new(
            GrpcIndexService::connect(config.beam_index_url.clone())
                .await
                .map_err(|e| eyre::eyre!("Failed to connect to beam-index: {}", e))?,
        );

        Ok(Self {
            auth: auth_service,
            hash: hash_service.clone() as Arc<dyn HashService>,
            library: Arc::new(LocalLibraryService::new(
                library_repo,
                file_repo.clone(),
                config.video_dir.clone(),
                notification_service.clone(),
                index_service,
            )),
            metadata: Arc::new(DbMetadataService::new(
                movie_repo,
                show_repo,
                file_repo,
                stream_repo,
            )),
            transcode: transcode_service,
            notification: notification_service,
            admin_log: admin_log_service,
            user_repo,
        })
    }
}
