pub use sea_orm_migration::prelude::*;

mod m20260209_000001_create_schema;
mod m20260210_000001_create_users;

mod m20260212_000001_ensure_cascade;
mod m20260222_000001_create_admin_log;
mod m20260522_000001_add_file_mtime;
mod m20260704_000001_drop_stream_cache;
mod m20260704_000002_create_sessions;
mod m20260704_000003_metadata_enrichment;
mod m20260704_000004_add_enrichment_admin_log_category;
mod m20260704_000005_enable_pg_trgm;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260209_000001_create_schema::Migration),
            Box::new(m20260210_000001_create_users::Migration),
            Box::new(m20260212_000001_ensure_cascade::Migration),
            Box::new(m20260222_000001_create_admin_log::Migration),
            Box::new(m20260522_000001_add_file_mtime::Migration),
            Box::new(m20260704_000001_drop_stream_cache::Migration),
            Box::new(m20260704_000002_create_sessions::Migration),
            Box::new(m20260704_000003_metadata_enrichment::Migration),
            Box::new(m20260704_000004_add_enrichment_admin_log_category::Migration),
            Box::new(m20260704_000005_enable_pg_trgm::Migration),
        ]
    }
}
