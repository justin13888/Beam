use sea_orm_migration::prelude::*;

/// What stacking a multi-part movie stores (issue #233).
///
/// - `files.part_number`: which part of a movie split across files the file
///   is (`Movie (2019) - CD2`), from 1. Every part is a file of the one entry
///   of its edition; the number orders them. A `CHECK` keeps it off anything
///   that is not a movie file, and off zero and below.
///
/// Existing rows get `NULL`. The indexer's classifier version moves with this
/// change, so the first scan after the upgrade reclassifies every file and
/// records the part of each one that has one; the titles each part used to be
/// are merged by the identity-key re-derivation before it.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "ALTER TABLE files ADD COLUMN part_number INTEGER \
                 CONSTRAINT files_part_number_requires_movie \
                 CHECK (part_number IS NULL OR (movie_entry_id IS NOT NULL AND part_number >= 1))",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Dropping the column drops the `CHECK` that reads it.
        db.execute_unprepared("ALTER TABLE files DROP COLUMN part_number")
            .await?;

        Ok(())
    }
}
