use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // `stream_cache` backed an HLS/fragmented-MP4 remux-on-request cache that
        // was never fully wired up (nothing wrote to it) and is now moot: Beam
        // never transcodes or remuxes media server-side, so there is no derived
        // streaming artifact to cache. See ADR-0004.
        db.execute_unprepared("DROP TABLE IF EXISTS stream_cache")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "CREATE TABLE stream_cache (
                id UUID PRIMARY KEY,
                file_id UUID NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                target_codec TEXT NOT NULL,
                target_container TEXT NOT NULL,
                target_resolution TEXT,
                target_bitrate BIGINT,
                hls_playlist_path TEXT,
                cache_path TEXT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL
            )",
        )
        .await?;

        Ok(())
    }
}
