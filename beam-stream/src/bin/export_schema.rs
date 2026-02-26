//! Export GraphQL schema to a file for code generation

use beam_stream::config::ServerConfig;
use beam_stream::graphql::create_schema;
use beam_stream::state::AppServices;
use beam_stream::state::AppState;
use eyre::Result;
use std::fs;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    dotenvy::dotenv().ok();

    // Load configuration (or use defaults)
    let config = ServerConfig::load_and_validate()?;

    // Connect to database (needed for schema creation even if not used)
    // We can use the real DB since we expect it to be running in dev environment
    let db = sea_orm::Database::connect(&config.database_url).await?;

    // Create Services
    let services = AppServices::new(&config, db).await?;
    let state = AppState::new(config.clone(), services);

    // Create the GraphQL schema
    let schema = create_schema(state);

    // Export as SDL
    let sdl = schema.sdl();

    // Write to beam-stream directory
    let output_path = "schema.graphql";
    fs::write(output_path, sdl)?;

    println!("GraphQL schema exported to: beam-stream/{output_path}");

    Ok(())
}
