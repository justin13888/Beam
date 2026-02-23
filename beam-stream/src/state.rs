use sea_orm::DatabaseConnection;
use std::ops::Deref;
use std::sync::Arc;

use crate::{
    config::ServerConfig,
    repositories::UserRepository,
    services::{
        admin_log::{AdminLogService, LocalAdminLogService},
        auth::{AuthService, LocalAuthService},
        hash::{HashConfig, HashService, LocalHashService},
        library::{LibraryService, LocalLibraryService},
        metadata::{MetadataConfig, MetadataService, StubMetadataService},
        session_store::{RedisSessionStore, SessionStore},
        transcode::{LocalTranscodeService, TranscodeService},
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
    pub session_store: Arc<dyn SessionStore>,
    pub hash: Arc<dyn HashService>,
    pub library: Arc<dyn LibraryService>,
    pub metadata: Arc<dyn MetadataService>,
    pub transcode: Arc<dyn TranscodeService>,
    pub admin_log: Arc<dyn AdminLogService>,
    pub user_repo: Arc<dyn UserRepository>,
}

impl AppServices {
    pub async fn new(config: &ServerConfig, db: DatabaseConnection) -> Self {
        let hash_config = HashConfig::default();
        let metadata_config = MetadataConfig {
            cache_dir: config.cache_dir.clone(),
        };

        // Create repository implementations
        let library_repo = Arc::new(crate::repositories::SqlLibraryRepository::new(db.clone()));
        let file_repo = Arc::new(crate::repositories::SqlFileRepository::new(db.clone()));
        let movie_repo = Arc::new(crate::repositories::SqlMovieRepository::new(db.clone()));
        let show_repo = Arc::new(crate::repositories::SqlShowRepository::new(db.clone()));
        let stream_repo = Arc::new(crate::repositories::SqlMediaStreamRepository::new(
            db.clone(),
        ));
        let user_repo: Arc<dyn UserRepository> =
            Arc::new(crate::repositories::SqlUserRepository::new(db.clone()));
        let admin_log_repo = Arc::new(crate::repositories::SqlAdminLogRepository::new(db.clone()));

        let hash_service = Arc::new(LocalHashService::new(hash_config));
        let media_info_service =
            Arc::new(crate::services::media_info::LocalMediaInfoService::default());
        let transcode_service = Arc::new(LocalTranscodeService::new(
            hash_service.clone(),
            media_info_service.clone(),
        ));

        // Initialize Redis session store
        let session_store = Arc::new(
            RedisSessionStore::new(&config.redis_url)
                .await
                .expect("Failed to connect to Redis"),
        );

        let auth_service = Arc::new(LocalAuthService::new(
            user_repo.clone(),
            session_store.clone(),
            config.clone(),
        ));

        let admin_log_service: Arc<dyn AdminLogService> =
            Arc::new(LocalAdminLogService::new(admin_log_repo));

        Self {
            auth: auth_service,
            session_store,
            hash: hash_service.clone() as Arc<dyn HashService>,
            library: Arc::new(LocalLibraryService::new(
                library_repo,
                file_repo,
                movie_repo,
                show_repo,
                stream_repo,
                config.video_dir.clone(),
                hash_service.clone(),
                media_info_service,
                admin_log_service.clone(),
            )),
            metadata: Arc::new(StubMetadataService::new(metadata_config)),
            transcode: transcode_service,
            admin_log: admin_log_service,
            user_repo,
        }
    }
}
