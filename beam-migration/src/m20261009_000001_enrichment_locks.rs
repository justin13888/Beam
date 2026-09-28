use sea_orm_migration::prelude::*;

/// What administrator control of enrichment stores (issue #185).
///
/// - `metadata_enrichment.locked_fields`: the fields of a title enrichment
///   leaves as they are, by name. A `CHECK` holds it to the names Beam knows
///   (`beam_domain::models::MetadataField`), so a typo can never lock
///   nothing while reading as a lock. Empty -- nothing locked -- by default,
///   so every existing row keeps today's behaviour.
/// - `idx_metadata_enrichment_list`: `(status, updated_at DESC, id DESC)`,
///   the admin list's order under its status filter, so a page of the
///   unmatched or failed titles is an index range rather than a sort of the
///   whole table.
/// - `idx_metadata_enrichment_recent`: `(updated_at DESC, id DESC)`, the
///   same order with no status filter -- the list's default -- which the
///   status-led index cannot serve.
///
/// `down()` drops all three.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "ALTER TABLE metadata_enrichment \
                 ADD COLUMN locked_fields TEXT[] NOT NULL DEFAULT '{}' \
                 CONSTRAINT metadata_enrichment_locked_fields CHECK (locked_fields <@ ARRAY[ \
                     'title', 'original_title', 'description', 'year', 'release_date', \
                     'runtime', 'poster', 'backdrop', 'rating', 'genres' \
                 ]::text[])",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_metadata_enrichment_list \
                 ON metadata_enrichment (status, updated_at DESC, id DESC)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_metadata_enrichment_recent \
                 ON metadata_enrichment (updated_at DESC, id DESC)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP INDEX idx_metadata_enrichment_recent")
            .await?;
        db.execute_unprepared("DROP INDEX idx_metadata_enrichment_list")
            .await?;
        db.execute_unprepared("ALTER TABLE metadata_enrichment DROP COLUMN locked_fields")
            .await?;

        Ok(())
    }
}
