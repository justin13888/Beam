use sea_orm_migration::prelude::*;

/// Per-user library access and parental ratings (issue #263).
///
/// - `movies.content_rating`, `shows.content_rating`: the provider's
///   certification for the title (`PG-13`, `TV-MA`), as enrichment stores it;
///   `NULL` while the provider has not reported one. At most 32 characters
///   (`movies_content_rating_length`, `shows_content_rating_length`): a real
///   certification is a few.
/// - `user_access_policies`: what an administrator restricted a user to. One
///   row per restricted user; a user with no row sees everything, as every
///   user did before this. `all_libraries` says whether the user sees every
///   library or only the ones `user_library_access` grants them;
///   `max_content_rating` is the highest age band of
///   `beam_domain::models::access::ContentRating` they may see, held to those
///   names by `user_access_policies_max_content_rating`, or `NULL` for no
///   ceiling. Goes with its user.
/// - `user_library_access`: the libraries a user whose policy does not cover
///   every library may see, one row per grant. Goes with its user and with
///   its library; `idx_user_library_access_library_id` serves the library's
///   cascade.
///
/// Nothing existing is backfilled: no title has a certification until its
/// next enrichment, and no user is restricted until an administrator says so.
///
/// `down()` drops both tables and both columns.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        for table in ["movies", "shows"] {
            db.execute_unprepared(&format!(
                "ALTER TABLE {table} ADD COLUMN content_rating TEXT \
                     CONSTRAINT {table}_content_rating_length \
                     CHECK (char_length(content_rating) BETWEEN 1 AND 32)"
            ))
            .await?;
        }

        db.execute_unprepared(
            "CREATE TABLE user_access_policies (\
                 user_id UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE, \
                 all_libraries BOOLEAN NOT NULL, \
                 max_content_rating TEXT \
                     CONSTRAINT user_access_policies_max_content_rating CHECK (max_content_rating IN \
                     ('all_ages', 'older_children', 'parental_guidance', 'teen', 'mature', 'adult')), \
                 updated_at TIMESTAMPTZ NOT NULL\
             )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE user_library_access (\
                 user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE, \
                 library_id UUID NOT NULL REFERENCES libraries (id) ON DELETE CASCADE, \
                 PRIMARY KEY (user_id, library_id)\
             )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_user_library_access_library_id ON user_library_access (library_id)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Dropping a table drops its indexes and constraints with it, and
        // dropping a column the `CHECK` that reads it.
        db.execute_unprepared("DROP TABLE user_library_access")
            .await?;
        db.execute_unprepared("DROP TABLE user_access_policies")
            .await?;
        for table in ["shows", "movies"] {
            db.execute_unprepared(&format!("ALTER TABLE {table} DROP COLUMN content_rating"))
                .await?;
        }

        Ok(())
    }
}
