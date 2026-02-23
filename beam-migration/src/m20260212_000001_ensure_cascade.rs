use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Ensure cascading deletes for Library -> LibraryMovies and Library -> LibraryShows
        // We drop existing constraints (if any) and recreate them with ON DELETE CASCADE.
        // Note: The create_schema migration included cascades, but we reinforce this here
        // to address potential inconsistencies or prior state.

        let db = manager.get_connection();

        // library_movies
        // Drop constraint by name (assuming standard Postgres naming: table_column_fkey)
        // We use IF EXISTS to be safe.
        db.execute_unprepared(
            "ALTER TABLE library_movies DROP CONSTRAINT IF EXISTS library_movies_library_id_fkey",
        )
        .await?;

        // Re-add with CASCADE
        db.execute_unprepared(
            "ALTER TABLE library_movies \
             ADD CONSTRAINT library_movies_library_id_fkey \
             FOREIGN KEY (library_id) REFERENCES libraries(id) ON DELETE CASCADE",
        )
        .await?;

        // library_shows
        db.execute_unprepared(
            "ALTER TABLE library_shows DROP CONSTRAINT IF EXISTS library_shows_library_id_fkey",
        )
        .await?;

        db.execute_unprepared(
            "ALTER TABLE library_shows \
             ADD CONSTRAINT library_shows_library_id_fkey \
             FOREIGN KEY (library_id) REFERENCES libraries(id) ON DELETE CASCADE",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Revert to default (NO ACTION / RESTRICT)
        db.execute_unprepared(
            "ALTER TABLE library_movies DROP CONSTRAINT IF EXISTS library_movies_library_id_fkey",
        )
        .await?;

        db.execute_unprepared(
            "ALTER TABLE library_movies \
             ADD CONSTRAINT library_movies_library_id_fkey \
             FOREIGN KEY (library_id) REFERENCES libraries(id)",
        )
        .await?;

        db.execute_unprepared(
            "ALTER TABLE library_shows DROP CONSTRAINT IF EXISTS library_shows_library_id_fkey",
        )
        .await?;

        db.execute_unprepared(
            "ALTER TABLE library_shows \
             ADD CONSTRAINT library_shows_library_id_fkey \
             FOREIGN KEY (library_id) REFERENCES libraries(id)",
        )
        .await?;

        Ok(())
    }
}
