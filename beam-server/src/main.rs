use eyre::{Result, eyre};
use http::Method;
use salvo::cors::Cors;
use salvo::prelude::*;
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

    match config.cookie_security_verdict() {
        beam_server::config::CookieSecurityVerdict::Ok => {}
        beam_server::config::CookieSecurityVerdict::WarnExplicitInsecure => {
            tracing::warn!(
                "BEAM_COOKIE_SECURE=false was set explicitly while BEAM_WEB_URL/\
                 BEAM_EXTRA_ALLOWED_ORIGINS suggest an HTTPS deployment -- the session \
                 cookie will be issued without the Secure flag. Only keep this override \
                 if you understand why your topology needs it."
            );
        }
        beam_server::config::CookieSecurityVerdict::ErrLikelyMisconfigured => {
            return Err(eyre!(
                "cookie security misconfiguration: cookies resolved to Secure=false (from \
                 BEAM_SERVER_URL={:?}) while BEAM_WEB_URL/BEAM_EXTRA_ALLOWED_ORIGINS suggest an \
                 HTTPS deployment. The session cookie would ship without the Secure flag on \
                 what looks like a production HTTPS site. Set BEAM_SERVER_URL to the \
                 externally-visible HTTPS URL, or set BEAM_COOKIE_SECURE=true (or =false to \
                 explicitly accept insecure cookies).",
                config.server_url
            ));
        }
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

    // Deep-health probe over the same pool, built before the connection is
    // moved into the services (sea-orm's `DatabaseConnection` is a cheap
    // `Arc`-backed clone).
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

    let state = beam_server::state::AppState::new(config.clone(), services, probe);

    // Build CORS handler
    let cors = Cors::new()
        .allow_origin(salvo::cors::AllowOrigin::mirror_request())
        .allow_methods(vec![
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(vec![
            "authorization",
            "content-type",
            "accept",
            "x-requested-with",
            "range",
        ])
        .expose_headers(vec!["accept-ranges", "content-length", "content-range"])
        .allow_credentials(true)
        .max_age(3600) // Cache the preflight for 1 hour to reduce noise
        .into_handler();

    // Build API router
    let router = create_router(state.clone());

    // Generate OpenAPI documentation
    let doc = OpenApi::new("Beam Server API", "1.0.0").merge_router(&router);
    let router = router
        .push(doc.into_router("/api-doc/openapi.json"))
        .push(Scalar::new("/api-doc/openapi.json").into_router("/openapi"));

    let service = Service::new(router).hoop(cors);

    info!("Binding to address: {}", &config.bind_address);
    let acceptor = TcpListener::new(config.bind_address.clone()).bind().await;

    info!("Server listening on {}", config.bind_address);
    info!(
        "API documentation available at http://{}/openapi",
        config.bind_address
    );

    let server = Server::new(acceptor);
    let handle = server.handle();
    let shutdown_timeout = std::time::Duration::from_secs(config.shutdown_timeout_secs);
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        info!(
            "Shutdown signal received -- draining connections for up to {}s",
            shutdown_timeout.as_secs()
        );
        handle.stop_graceful(shutdown_timeout);
    });

    server.serve(service).await;
    info!("Server stopped");

    Ok(())
}

/// Resolves when the process is asked to stop: ctrl-c anywhere, or SIGTERM
/// on unix (what container orchestrators send before a hard kill).
async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!("Failed to listen for ctrl-c; that shutdown path is disabled: {e}");
            std::future::pending::<()>().await
        }
    };

    #[cfg(unix)]
    {
        let sigterm = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut stream) => {
                    stream.recv().await;
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to listen for SIGTERM; that shutdown path is disabled: {e}"
                    );
                    std::future::pending::<()>().await
                }
            }
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm => {}
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}
