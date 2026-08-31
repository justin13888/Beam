use std::num::NonZeroUsize;
use std::time::Duration;

use eyre::{Result, eyre};
use kynos::server::{Server, error::ServerError, shutdown::Shutdown};
use tracing::info;

use beam_server::config::ServerConfig;
use beam_server::routes::create_router;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    // Load environment variables from .env file if present
    dotenvy::dotenv().ok();

    // Initialize JSON logging
    beam_server::logging::init_tracing();

    info!("Starting beam-server...");

    // Load configuration
    let config = ServerConfig::load_and_validate().map_err(|e| eyre!(e))?;

    info!("Configuration loaded: {:?}", config);

    match beam_server::bootstrap::check_cookie_security(&config) {
        Ok(beam_server::bootstrap::StartupGate::Proceed) => {}
        Ok(beam_server::bootstrap::StartupGate::ProceedWithWarning(warning)) => {
            tracing::warn!("{warning}");
        }
        Err(message) => return Err(eyre!(message)),
    }

    // Ensure the data directory exists (video_dir is validated by config)
    tokio::fs::create_dir_all(&config.data_dir)
        .await
        .map_err(|e| eyre!("Failed to create data directory: {e}"))?;

    // Initialize ffmpeg bindings (beam-index's probing at index time is the
    // only thing in this process that needs them -- beam-server itself
    // never transcodes/remuxes and has no direct ffmpeg dependency).
    beam_index::probe::init().map_err(|e| eyre!("Failed to initialize ffmpeg: {e}"))?;

    // Connect to Database
    info!(
        "Connecting to database at {}",
        config.redacted_database_url()
    );
    let db = beam_server::db::connect(&config)
        .await
        .map_err(|e| eyre!("Failed to connect to database after retries: {}", e))?;
    info!("Connected to database");

    // Apply pending migrations. The supported topology is a single server
    // process against one Postgres (see docs/operations/deployment.md), so
    // there is no concurrent-migrator race to coordinate.
    if config.auto_migrate {
        use beam_migration::MigratorTrait;
        let pending = beam_migration::Migrator::get_pending_migrations(&db)
            .await
            .map_err(|e| eyre!("Failed to check pending migrations: {e}"))?
            .len();
        beam_migration::Migrator::up(&db, None)
            .await
            .map_err(|e| eyre!("Failed to apply database migrations: {e}"))?;
        info!("Database migrations up to date ({pending} applied at startup)");
    } else {
        info!(
            "BEAM_AUTO_MIGRATE=false -- migrations are operator-managed via the beam-migration CLI"
        );
    }

    // One pool behind one `Arc`, shared by the health probe and by every
    // repository and store inside `AppServices`. sea-orm's
    // `DatabaseConnection` is only `Clone` while its `mock` feature is off, and
    // the hermetic SQL-shape tests need that feature -- so the sharing is
    // explicit rather than a `.clone()` per consumer.
    let db = std::sync::Arc::new(db);

    // Deep-health probe over the same pool.
    let probe: std::sync::Arc<dyn beam_server::services::health::DependencyProbe> =
        std::sync::Arc::new(beam_server::services::health::DbProbe::new(db.clone()));

    // Initialize App Services and State
    let (services, index_service, enrichment_service) =
        beam_server::state::AppServices::new(&config, db)
            .await
            .map_err(|e| eyre!("Failed to initialize services: {e}"))?;

    // Start in-process indexing (startup scan, filesystem watcher, periodic
    // rescan backstop). There is no separate indexer process.
    beam_index::runtime::spawn_background_indexing(
        index_service,
        beam_index::runtime::BackgroundIndexingConfig {
            scan_interval_secs: config.scan_interval_secs,
            watch_enabled: config.watch_enabled,
            watch_debounce_ms: config.watch_debounce_ms,
        },
    );

    // Start the metadata-enrichment sweep loop. `AppServices::new` wires this
    // to the cameo-backed provider when TMDB/AniList are configured, falling
    // back to a no-op (fast, harmless skip) otherwise.
    beam_index::runtime::spawn_enrichment_worker(
        enrichment_service,
        std::time::Duration::from_secs(config.enrich_interval_secs),
    );

    // Install the global Prometheus recorder when metrics are enabled. Done
    // once at startup: every `metrics::counter!`/`histogram!` call anywhere in
    // the process (the HTTP observer, beam-index domain counters) records into
    // it, and `GET /metrics` renders it from the handle carried on the state.
    // When disabled, no recorder exists, every facade call is a no-op, and
    // `/metrics` answers 503 rather than disappearing -- the router's shape,
    // and therefore the exported description, must not depend on configuration.
    let metrics_handle = if config.enable_metrics {
        let handle = metrics_exporter_prometheus::PrometheusBuilder::new()
            .install_recorder()
            .map_err(|e| eyre!("Failed to install Prometheus metrics recorder: {e}"))?;
        info!("Metrics enabled -- Prometheus exposition at /metrics");
        Some(handle)
    } else {
        None
    };

    let state = beam_server::state::AppState::new(config.clone(), services, probe, metrics_handle);

    // `build` is where Kynos checks that every operation can be described and
    // that no two interceptors contribute the same header or status. A router
    // that cannot describe itself never reaches a listener.
    let service = create_router()
        .build(state)
        .map_err(|e| eyre!("The router does not describe itself: {e}"))?;

    // `prepare` binds before it serves, so "address already in use" fails here
    // rather than after the process has claimed to be up -- and it is the only
    // way to learn which port was chosen when the configured one is zero.
    let bound = Server::new(service)
        .bind(config.bind_address.clone())
        .graceful_shutdown(Shutdown::signals())
        .shutdown_timeout(Duration::from_secs(config.shutdown_timeout_secs))
        .max_connections(NonZeroUsize::new(10_000).expect("10000 is not zero"))
        .prepare()
        .await?;

    for address in bound.local_addrs() {
        info!("Server listening on http://{address}");
        info!("API documentation available at http://{address}/openapi");
    }

    match bound.serve().await {
        Ok(()) => info!("Server stopped"),
        // A drain that ran out of time, or one a second signal cut short, is an
        // operational fact rather than a startup failure. The process stops
        // either way; exiting non-zero would make a normal rollout look like a
        // crash to whatever is watching the exit code.
        Err(kynos::Error::Server(ServerError::ShutdownTimeout { timeout })) => {
            tracing::warn!(
                ?timeout,
                "Graceful shutdown hit its deadline -- remaining connections were cut"
            );
        }
        Err(kynos::Error::Server(ServerError::ShutdownForced)) => {
            tracing::warn!("Shutdown forced by a second termination signal");
        }
        Err(error) => return Err(eyre!("Server failed: {error}")),
    }

    Ok(())
}
