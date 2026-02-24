use eyre::{Result, eyre};
use http::Method;
use salvo::cors::Cors;
use salvo::prelude::*;
use std::sync::Arc;
use tracing::info;

use beam_auth::config::ServerConfig;
use beam_auth::server::auth_routes;
use beam_auth::utils::repository::SqlUserRepository;
use beam_auth::utils::service::{AuthService, LocalAuthService};
use beam_auth::utils::session_store::RedisSessionStore;

#[handler]
async fn health_check(res: &mut Response) {
    res.status_code(StatusCode::OK);
    res.render(Text::Plain("OK"));
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    // Load environment variables from .env file if present
    dotenvy::dotenv().ok();

    // Initialize logging
    beam_auth::logging::init_tracing();

    info!("Starting beam-auth...");

    // Load configuration
    let config = ServerConfig::load_and_validate().map_err(|e| eyre!(e))?;

    info!("Configuration loaded: {:?}", config);

    // Connect to Database
    info!("Connecting to database at {}", config.database_url);
    let db = sea_orm::Database::connect(&config.database_url)
        .await
        .map_err(|e| eyre!("Failed to connect to database: {}", e))?;
    info!("Connected to database");

    // Connect to Redis
    info!("Connecting to Redis at {}", config.redis_url);
    let session_store = Arc::new(
        RedisSessionStore::new(&config.redis_url)
            .await
            .map_err(|e| eyre!("Failed to connect to Redis: {}", e))?,
    );
    info!("Connected to Redis");

    // Build services
    let user_repo = Arc::new(SqlUserRepository::new(db));
    let auth_service: Arc<dyn AuthService> = Arc::new(LocalAuthService::new(
        user_repo,
        session_store,
        config.jwt_secret.clone(),
    ));

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
        .max_age(3600)
        .into_handler();

    // Build API router
    let router = Router::new().hoop(affix_state::inject(auth_service)).push(
        Router::with_path("v1")
            .push(Router::with_path("health").get(health_check))
            .push(Router::with_path("auth").push(auth_routes())),
    );

    let service = Service::new(router).hoop(cors);

    info!("Binding to address: {}", &config.bind_address);
    let acceptor = TcpListener::new(config.bind_address.clone()).bind().await;

    info!("Server listening on {}", config.bind_address);

    Server::new(acceptor).serve(service).await;

    Ok(())
}
