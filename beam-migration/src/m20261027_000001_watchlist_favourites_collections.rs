use sea_orm_migration::prelude::*;

/// Watchlists, favourites and provider collections (issue #262).
///
/// - `user_list_entries`: one movie or show on one of a user's lists. `list`
///   is `watchlist` or `favourites` (`user_list_entries_list`); exactly one of
///   `movie_id` / `show_id` is set (`user_list_entries_one_title`). A title is
///   on a list at most once, which the two unique indexes over
///   `(user_id, list, movie_id)` and `(user_id, list, show_id)` hold -- a
///   `NULL` never conflicts, so each index constrains only its own kind.
///   `idx_user_list_entries_page` serves a list's pages, newest addition
///   first; the per-title indexes serve the cascades and the carry a title
///   merge makes. Every foreign key cascades: an entry goes with its user and
///   with its title.
/// - `collections`: one row per provider collection (`external_ref`, e.g.
///   `tmdb:2344`, unique), with the name and art enrichment last saw.
/// - `collection_movies`: which collection a movie belongs to. Keyed by the
///   movie, so a movie belongs to at most one, as a TMDB movie does; both
///   foreign keys cascade.
///
/// Nothing existing is backfilled: a movie joins its collection on its next
/// enrichment, a refresh included.
///
/// `down()` drops all three tables.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "CREATE TABLE user_list_entries (\
                 id UUID PRIMARY KEY, \
                 user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE, \
                 list TEXT NOT NULL \
                     CONSTRAINT user_list_entries_list CHECK (list IN ('watchlist', 'favourites')), \
                 movie_id UUID REFERENCES movies (id) ON DELETE CASCADE, \
                 show_id UUID REFERENCES shows (id) ON DELETE CASCADE, \
                 added_at TIMESTAMPTZ NOT NULL, \
                 CONSTRAINT user_list_entries_one_title CHECK ((movie_id IS NULL) <> (show_id IS NULL))\
             )",
        )
        .await?;
        for index in [
            "CREATE UNIQUE INDEX idx_user_list_entries_user_movie \
             ON user_list_entries (user_id, list, movie_id)",
            "CREATE UNIQUE INDEX idx_user_list_entries_user_show \
             ON user_list_entries (user_id, list, show_id)",
            "CREATE INDEX idx_user_list_entries_page \
             ON user_list_entries (user_id, list, added_at DESC, id DESC)",
            "CREATE INDEX idx_user_list_entries_movie_id ON user_list_entries (movie_id)",
            "CREATE INDEX idx_user_list_entries_show_id ON user_list_entries (show_id)",
        ] {
            db.execute_unprepared(index).await?;
        }

        db.execute_unprepared(
            "CREATE TABLE collections (\
                 id UUID PRIMARY KEY, \
                 external_ref TEXT NOT NULL UNIQUE, \
                 name TEXT NOT NULL, \
                 poster_url TEXT, \
                 backdrop_url TEXT, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT now()\
             )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE collection_movies (\
                 movie_id UUID PRIMARY KEY REFERENCES movies (id) ON DELETE CASCADE, \
                 collection_id UUID NOT NULL REFERENCES collections (id) ON DELETE CASCADE\
             )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_collection_movies_collection_id ON collection_movies (collection_id)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Dropping a table drops its indexes and constraints with it.
        db.execute_unprepared("DROP TABLE collection_movies")
            .await?;
        db.execute_unprepared("DROP TABLE collections").await?;
        db.execute_unprepared("DROP TABLE user_list_entries")
            .await?;

        Ok(())
    }
}
