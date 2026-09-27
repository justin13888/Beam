use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    // `AllOrNothing<Migrator>`, not `Migrator`: `up` must apply its batch
    // all-or-nothing, exactly as `beam-server` does at startup.
    cli::run_cli(beam_migration::AllOrNothing::<beam_migration::Migrator>::new()).await;
}
