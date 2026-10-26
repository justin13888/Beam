use sea_orm_migration::prelude::*;

/// What a selective browse filter needs to start from its matches rather than
/// from the sort index (issue #247).
///
/// A page sorted by title or date added walks `idx_*_title_sort` /
/// `idx_*_added_sort` in order and checks each filter per row, which stops at
/// the page size when most titles pass -- and reads the whole index when few
/// do. These give the planner the other plan: read the titles a filter
/// admits through an index on what it compares, then sort just those. Which
/// is cheaper depends on how many titles the filter admits, which the
/// planner's statistics know and this code does not.
///
/// * `idx_movies_year` / `idx_shows_year` -- `(year)`, for `year`,
///   `year_from` and `year_to`.
/// * `idx_movies_rating_floor` / `idx_shows_rating_floor` --
///   `(COALESCE(rating_tmdb, 0))`, spelled as the `min_rating` filter
///   compares it, so an unrated title counts as 0 there too.
///
/// A genre filter already has `idx_movie_genres_genre_id` /
/// `idx_show_genres_genre_id`, and a search `idx_*_title_trgm`.
///
/// `down()` drops the four indexes.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("CREATE INDEX idx_movies_year ON movies (year)")
            .await?;
        db.execute_unprepared("CREATE INDEX idx_shows_year ON shows (year)")
            .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_movies_rating_floor ON movies ((COALESCE(rating_tmdb, 0)))",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_shows_rating_floor ON shows ((COALESCE(rating_tmdb, 0)))",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP INDEX idx_shows_rating_floor")
            .await?;
        db.execute_unprepared("DROP INDEX idx_movies_rating_floor")
            .await?;
        db.execute_unprepared("DROP INDEX idx_shows_year").await?;
        db.execute_unprepared("DROP INDEX idx_movies_year").await?;

        Ok(())
    }
}
