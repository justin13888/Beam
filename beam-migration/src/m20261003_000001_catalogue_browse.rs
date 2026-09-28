use sea_orm_migration::prelude::*;

/// What browse and search need to sort and page in the database (issue #187).
///
/// * `shows.rating_tmdb` -- a show's provider rating, on the same 0-10 `REAL`
///   scale as `movies.rating_tmdb`, so the two kinds sort and filter by rating
///   together. `NULL` until a show is enriched (again): existing shows gain it
///   on their next enrichment, not from this migration.
/// * `idx_movies_title_sort` / `idx_shows_title_sort` -- `(lower(title), id)`,
///   the default browse order. Each branch of the catalogue's `UNION ALL`
///   orders and seeks on exactly `(lower(title), id)`, so it reads this index
///   in order under its limit, and a page after a cursor starts with an index
///   condition rather than a sort of every title.
/// * `idx_movies_added_sort` / `idx_shows_added_sort` -- `(created_at, id)`,
///   the same for the `date_added` sort ("recently added").
///
/// Year, rating and runtime get no index: they are nullable, and their sort
/// tuple puts a null flag first whose sense flips with the direction, which a
/// single btree cannot serve both ways. Their pages still sort every matching
/// title (NFR-301 says so).
///
/// `down()` drops the indexes and the column.
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
        db.execute_unprepared("CREATE INDEX idx_movies_added_sort ON movies (created_at, id)")
            .await?;
        db.execute_unprepared("CREATE INDEX idx_shows_added_sort ON shows (created_at, id)")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP INDEX idx_shows_added_sort")
            .await?;
        db.execute_unprepared("DROP INDEX idx_movies_added_sort")
            .await?;
        db.execute_unprepared("DROP INDEX idx_shows_title_sort")
            .await?;
        db.execute_unprepared("DROP INDEX idx_movies_title_sort")
            .await?;
        db.execute_unprepared("ALTER TABLE shows DROP COLUMN rating_tmdb")
            .await?;

        Ok(())
    }
}
