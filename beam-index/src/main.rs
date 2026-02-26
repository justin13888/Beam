use std::net::SocketAddr;
use std::sync::Arc;

use confique::Config;
use eyre::{Result, eyre};
use tonic::transport::Server;
use tracing::info;

use beam_index::config::IndexConfig;
use beam_index::grpc::IndexServiceGrpc;
use beam_index::proto::index_service_server::IndexServiceServer;
use beam_index::repositories::{
    SqlAdminLogRepository, SqlFileRepository, SqlLibraryRepository, SqlMediaStreamRepository,
    SqlMovieRepository, SqlShowRepository,
};
use beam_index::services::admin_log::LocalAdminLogService;
use beam_index::services::hash::{HashConfig, LocalHashService};
use beam_index::services::index::LocalIndexService;
use beam_index::services::media_info::LocalMediaInfoService;
use beam_index::services::notification::LocalNotificationService;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .init();

    info!("Starting beam-index...");

    let config = IndexConfig::builder()
        .env()
        .load()
        .map_err(|e| eyre!("Failed to load configuration: {}", e))?;

    info!("Connecting to database at {}", config.database_url);
    let db = sea_orm::Database::connect(&config.database_url)
        .await
        .map_err(|e| eyre!("Failed to connect to database: {}", e))?;
    info!("Connected to database");

    ffmpeg_next::init().map_err(|e| eyre!("Failed to initialize ffmpeg: {}", e))?;

    // Build repositories
    let library_repo = Arc::new(SqlLibraryRepository::new(db.clone()));
    let file_repo = Arc::new(SqlFileRepository::new(db.clone()));
    let movie_repo = Arc::new(SqlMovieRepository::new(db.clone()));
    let show_repo = Arc::new(SqlShowRepository::new(db.clone()));
    let stream_repo = Arc::new(SqlMediaStreamRepository::new(db.clone()));
    let admin_log_repo = Arc::new(SqlAdminLogRepository::new(db.clone()));

    // Build services
    let notification_service = Arc::new(LocalNotificationService::new());
    let hash_service = Arc::new(LocalHashService::new(HashConfig::default()));
    let media_info_service = Arc::new(LocalMediaInfoService::default());
    let admin_log_service = Arc::new(LocalAdminLogService::new(admin_log_repo));

    let index_service = Arc::new(LocalIndexService::new(
        library_repo,
        file_repo,
        movie_repo,
        show_repo,
        stream_repo,
        hash_service,
        media_info_service,
        notification_service,
        admin_log_service,
    ));

    let grpc_handler = IndexServiceGrpc::new(index_service);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .map_err(|e| eyre!("Invalid bind address: {}", e))?;

    info!("beam-index gRPC server listening on {}", addr);

    Server::builder()
        .add_service(IndexServiceServer::new(grpc_handler))
        .serve(addr)
        .await
        .map_err(|e| eyre!("gRPC server error: {}", e))?;

    Ok(())
}
