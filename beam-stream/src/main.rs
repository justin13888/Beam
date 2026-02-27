use std::sync::atomic::Ordering;

use eyre::{Result, eyre};
use http::Method;
use salvo::cors::Cors;
use salvo::prelude::*;
use tracing::info;

use beam_stream::config::ServerConfig;
use beam_stream::graphql::create_schema;
use beam_stream::routes::create_router;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    // Load environment variables from .env file if present
    dotenvy::dotenv().ok();

    // Initialize JSON logging
    beam_stream::logging::init_tracing();

    info!("Starting beam-stream...");

    // Load configuration
    let config = ServerConfig::load_and_validate().map_err(|e| eyre!(e))?;

    info!("Configuration loaded: {:?}", config);

    // Ensure cache directory exists (video_dir is validated by config)
    tokio::fs::create_dir_all(&config.cache_dir)
        .await
        .map_err(|e| eyre!("Failed to create cache directory: {e}"))?;

    // Initialize ffmpeg bindings
    ffmpeg_next::init().map_err(|e| eyre!("Failed to initialize ffmpeg: {e}"))?;

    // Initialize m3u8-rs static variables
    m3u8_rs::WRITE_OPT_FLOAT_PRECISION.store(5, Ordering::Relaxed);

    // Connect to Database
    info!("Connecting to database at {}", config.database_url);
    let db = sea_orm::Database::connect(&config.database_url)
        .await
        .map_err(|e| eyre!("Failed to connect to database: {}", e))?;
    info!("Connected to database");

    // Initialize App Services and State
    let services = beam_stream::state::AppServices::new(&config, db)
        .await
        .map_err(|e| eyre!("Failed to initialize services: {e}"))?;
    let state = beam_stream::state::AppState::new(config.clone(), services);

    let schema = create_schema(state.clone());

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
        ])
        .allow_credentials(true)
        .max_age(3600) // Cache the preflight for 1 hour to reduce noise
        .into_handler();

    // Build API router
    let router = create_router(state.clone(), schema);

    // Generate OpenAPI documentation
    let doc = OpenApi::new("Beam Stream API", "1.0.0").merge_router(&router);
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
    info!(
        "GraphiQL interface available at http://{}/v1/graphql",
        config.bind_address
    );

    Server::new(acceptor).serve(service).await;

    Ok(())
}
