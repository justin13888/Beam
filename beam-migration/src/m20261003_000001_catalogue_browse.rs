use sea_orm_migration::prelude::*;

/// What browse and search need to sort and page in the database (issue #187).
///
/// * `shows.rating_tmdb` -- a show's provider rating, on the same 0-10 `REAL`
///   scale as `movies.rating_tmdb`, so the two kinds sort and filter by rating
///   together. `NULL` until a show is enriched (again): existing shows gain it
///   on their next enrichment, not from this migration.
/// * `idx_movies_title_sort` / `idx_shows_title_sort` -- `(lower(title), id)`,
///   the default browse order's keyset. Each branch of the catalogue's
///   `UNION ALL` reads its own index in that order, and a page after a cursor
///   seeks into it rather than sorting every title.
///
/// `down()` drops both indexes and the column.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE shows ADD COLUMN rating_tmdb REAL")
            .await?;
        db.execute_unprepared("CREATE INDEX idx_movies_title_sort ON movies (lower(title), id)")
            .await?;
        db.execute_unprepared("CREATE INDEX idx_shows_title_sort ON shows (lower(title), id)")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP INDEX idx_shows_title_sort")
            .await?;
        db.execute_unprepared("DROP INDEX idx_movies_title_sort")
            .await?;
        db.execute_unprepared("ALTER TABLE shows DROP COLUMN rating_tmdb")
            .await?;

        Ok(())
    }
}
