use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    // `CliMigrator`, not `Migrator`: `up` must apply its batch all-or-nothing,
    // exactly as `beam-server` does at startup.
    cli::run_cli(beam_migration::CliMigrator).await;
}
