use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use confique::Config;
use eyre::{Result, eyre};
use tonic::transport::Server;
use tracing::{error, info, warn};
use uuid::Uuid;

use beam_domain::repositories::LibraryRepository;
use beam_index::config::IndexConfig;
use beam_index::grpc::IndexServiceGrpc;
use beam_index::proto::index_service_server::IndexServiceServer;
use beam_index::repositories::{
    SqlAdminLogRepository, SqlFileRepository, SqlLibraryRepository, SqlMediaStreamRepository,
    SqlMovieRepository, SqlShowRepository,
};
use beam_index::services::admin_log::LocalAdminLogService;
use beam_index::services::clock::{Clock, RealClock};
use beam_index::services::hash::{HashConfig, LocalHashService};
use beam_index::services::index::LocalIndexService;
use beam_index::services::media_info::LocalMediaInfoService;
use beam_index::services::notification::LocalNotificationService;
use beam_index::services::watcher::{FsWatcher, NotifyFsWatcher, PathDebouncer};

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
    let library_repo: Arc<dyn LibraryRepository> = Arc::new(SqlLibraryRepository::new(db.clone()));
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

    let index_service = Arc::new(
        LocalIndexService::new(
            library_repo.clone(),
            file_repo,
            movie_repo,
            show_repo,
            stream_repo,
            hash_service,
            media_info_service,
            notification_service,
            admin_log_service,
        )
        .with_hash_unknown_files(config.hash_unknown_files),
    );

    // ── Automatic scanning ──────────────────────────────────────────────────

    // Startup scan, spawned so the gRPC port opens without waiting for it.
    {
        let index = index_service.clone();
        tokio::spawn(async move {
            info!("Running startup library scan...");
            match index.scan_all_libraries().await {
                Ok(n) => info!("Startup scan complete: {n} file(s) added"),
                Err(e) => error!("Startup scan failed: {e}"),
            }
        });
    }

    // Filesystem watcher for near-real-time reconciliation (optional).
    let watcher = if config.watch_enabled {
        match NotifyFsWatcher::new() {
            Ok(w) => {
                let watcher = Arc::new(w);
                let debounce = Duration::from_millis(config.watch_debounce_ms);
                tokio::spawn(run_watch_consumer(
                    watcher.clone(),
                    index_service.clone(),
                    debounce,
                ));
                Some(watcher)
            }
            Err(e) => {
                error!("Filesystem watcher unavailable ({e}); periodic scans only");
                None
            }
        }
    } else {
        info!("Filesystem watcher disabled by configuration");
        None
    };

    // Periodic rescan backstop, also refreshing watch registrations.
    {
        let index = index_service.clone();
        let library_repo = library_repo.clone();
        let interval = Duration::from_secs(config.scan_interval_secs);
        tokio::spawn(run_periodic_maintenance(
            index,
            library_repo,
            watcher,
            interval,
        ));
    }

    // ── gRPC server (runs for the lifetime of the process) ──────────────────

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

/// Consume filesystem-watcher events, coalescing bursts within a debounce
/// window before reconciling each affected path.
async fn run_watch_consumer(
    watcher: Arc<NotifyFsWatcher>,
    index: Arc<LocalIndexService>,
    debounce: Duration,
) {
    loop {
        // Block until the first event of a burst.
        let Some(first) = watcher.next_event().await else {
            info!("Filesystem watcher closed; stopping consumer");
            return;
        };
        let mut debouncer = PathDebouncer::new();
        debouncer.submit(first);

        // Collect further events for the debounce window.
        let window = tokio::time::sleep(debounce);
        tokio::pin!(window);
        loop {
            tokio::select! {
                _ = &mut window => break,
                event = watcher.next_event() => match event {
                    Some(e) => debouncer.submit(e),
                    None => break,
                },
            }
        }

        // Reconcile the coalesced events.
        for event in debouncer.drain() {
            let path = event.path.clone();
            if let Err(e) = index
                .reconcile_path(event.library_id, event.path, event.kind)
                .await
            {
                warn!("Failed to reconcile {}: {e}", path.display());
            }
        }
    }
}

/// Periodically rescan every library as a backstop for events the watcher
/// missed, and register watches for libraries created since startup.
async fn run_periodic_maintenance(
    index: Arc<LocalIndexService>,
    library_repo: Arc<dyn LibraryRepository>,
    watcher: Option<Arc<NotifyFsWatcher>>,
    interval: Duration,
) {
    let clock = RealClock;
    let mut watched: HashSet<Uuid> = HashSet::new();
    loop {
        if let Some(watcher) = &watcher {
            refresh_watches(watcher, library_repo.as_ref(), &mut watched).await;
        }

        clock.sleep(interval).await;

        match index.scan_all_libraries().await {
            Ok(n) => info!("Periodic rescan complete: {n} file(s) added"),
            Err(e) => error!("Periodic rescan failed: {e}"),
        }
    }
}

/// Register a recursive watch for every library not already watched.
async fn refresh_watches(
    watcher: &NotifyFsWatcher,
    library_repo: &dyn LibraryRepository,
    watched: &mut HashSet<Uuid>,
) {
    let libraries = match library_repo.find_all().await {
        Ok(libraries) => libraries,
        Err(e) => {
            warn!("Watch refresh failed to list libraries: {e}");
            return;
        }
    };
    for library in libraries {
        if watched.insert(library.id) {
            match watcher.watch_library(library.id, &library.root_path) {
                Ok(()) => info!(
                    "Watching library '{}' at {}",
                    library.name,
                    library.root_path.display()
                ),
                Err(e) => {
                    warn!("Failed to watch library '{}': {e}", library.name);
                    // Allow a retry on the next maintenance cycle.
                    watched.remove(&library.id);
                }
            }
        }
    }
}
