use sea_orm_migration::prelude::{extension::postgres::Type, *};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // `anilist_id` complements the existing tmdb_id/imdb_id/tvdb_id external
        // identifiers, populated by the cameo-backed enrichment worker (see
        // ADR-0006) for anime titles matched via AniList rather than TMDB.
        db.execute_unprepared("ALTER TABLE movies ADD COLUMN anilist_id INTEGER UNIQUE")
            .await?;
        db.execute_unprepared("ALTER TABLE shows ADD COLUMN anilist_id INTEGER UNIQUE")
            .await?;

        manager
            .create_type(
                Type::create()
                    .as_enum(EnrichmentStatus::Table)
                    .values([
                        EnrichmentStatus::Pending,
                        EnrichmentStatus::Enriched,
                        EnrichmentStatus::Unmatched,
                        EnrichmentStatus::Failed,
                    ])
                    .to_owned(),
            )
            .await?;

        // Per-title enrichment queue/status, mirroring the `files` table's
        // dual-nullable-FK-plus-CHECK polymorphism (a row is for a movie XOR a
        // show, never both, never neither). One row per title: the UNIQUE
        // constraints below let a rescan re-enqueue an existing row instead of
        // creating a duplicate, and give multi-file titles a single shared
        // enrichment record.
        manager
            .create_table(
                Table::create()
                    .table(MetadataEnrichment::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MetadataEnrichment::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(MetadataEnrichment::MovieId).uuid())
                    .col(ColumnDef::new(MetadataEnrichment::ShowId).uuid())
                    .col(
                        ColumnDef::new(MetadataEnrichment::Status)
                            .custom(EnrichmentStatus::Table)
                            .not_null()
                            .default("pending"),
                    )
                    .col(
                        ColumnDef::new(MetadataEnrichment::Attempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(MetadataEnrichment::NextAttemptAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(ColumnDef::new(MetadataEnrichment::EnrichedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(MetadataEnrichment::MatchConfidence).float())
                    .col(ColumnDef::new(MetadataEnrichment::MatchedRef).text())
                    .col(
                        ColumnDef::new(MetadataEnrichment::ForceRefresh)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(MetadataEnrichment::LastError).text())
                    .col(
                        ColumnDef::new(MetadataEnrichment::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MetadataEnrichment::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MetadataEnrichment::Table, MetadataEnrichment::MovieId)
                            .to(Movies::Table, Movies::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MetadataEnrichment::Table, MetadataEnrichment::ShowId)
                            .to(Shows::Table, Shows::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::cust(
                        "(movie_id IS NOT NULL AND show_id IS NULL) OR \
                         (movie_id IS NULL AND show_id IS NOT NULL)",
                    ))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_metadata_enrichment_movie_id")
                    .table(MetadataEnrichment::Table)
                    .col(MetadataEnrichment::MovieId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_metadata_enrichment_show_id")
                    .table(MetadataEnrichment::Table)
                    .col(MetadataEnrichment::ShowId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_metadata_enrichment_due")
                    .table(MetadataEnrichment::Table)
                    .col(MetadataEnrichment::Status)
                    .col(MetadataEnrichment::NextAttemptAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(MetadataEnrichment::Table).to_owned())
            .await?;

        manager
            .drop_type(Type::drop().name(EnrichmentStatus::Table).to_owned())
            .await?;

        let db = manager.get_connection();
        db.execute_unprepared("ALTER TABLE shows DROP COLUMN IF EXISTS anilist_id")
            .await?;
        db.execute_unprepared("ALTER TABLE movies DROP COLUMN IF EXISTS anilist_id")
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Movies {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Shows {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum EnrichmentStatus {
    #[sea_orm(iden = "enrichment_status")]
    Table,
    #[sea_orm(iden = "pending")]
    Pending,
    #[sea_orm(iden = "enriched")]
    Enriched,
    #[sea_orm(iden = "unmatched")]
    Unmatched,
    #[sea_orm(iden = "failed")]
    Failed,
}

#[derive(DeriveIden)]
enum MetadataEnrichment {
    Table,
    Id,
    MovieId,
    ShowId,
    Status,
    Attempts,
    NextAttemptAt,
    EnrichedAt,
    MatchConfidence,
    MatchedRef,
    ForceRefresh,
    LastError,
    CreatedAt,
    UpdatedAt,
}
