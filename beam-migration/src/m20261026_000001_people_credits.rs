use sea_orm_migration::prelude::*;

/// People and their credits, filled from provider enrichment (issue #261).
///
/// - `people`: one row per person a provider credits, keyed by the provider's
///   id for them (`external_ref`, e.g. `tmdb:287`, unique), with the name and
///   portrait URL enrichment last saw. `idx_people_name_trgm` serves people
///   search's `ILIKE`, as `idx_*_title_trgm` serves title search.
/// - `credits`: one role of one person on one movie or show. Exactly one of
///   `movie_id` / `show_id` is set (`credits_one_title`); `kind` is `cast` or
///   `crew`, and each kind carries only its own columns -- `character` and
///   `billing_order` for the cast, `job` and `department` for the crew --
///   which `credits_cast_columns` / `credits_crew_columns` hold it to. Every
///   foreign key cascades: a title's credits go with it, and so do a
///   person's. Indexed by each of the three ids, the columns a title's
///   detail, a filmography and the "credited at all" test of search read by.
///
/// Nothing existing is backfilled: credits arrive with each title's next
/// enrichment, a refresh included.
///
/// `down()` drops both tables.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "CREATE TABLE people (\
                 id UUID PRIMARY KEY, \
                 external_ref TEXT NOT NULL UNIQUE, \
                 name TEXT NOT NULL, \
                 profile_url TEXT, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT now()\
             )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_people_name_trgm ON people USING GIN (name gin_trgm_ops)",
        )
        .await?;

        db.execute_unprepared(
            "CREATE TABLE credits (\
                 id UUID PRIMARY KEY, \
                 person_id UUID NOT NULL REFERENCES people (id) ON DELETE CASCADE, \
                 movie_id UUID REFERENCES movies (id) ON DELETE CASCADE, \
                 show_id UUID REFERENCES shows (id) ON DELETE CASCADE, \
                 kind TEXT NOT NULL CONSTRAINT credits_kind CHECK (kind IN ('cast', 'crew')), \
                 character TEXT, \
                 billing_order INTEGER, \
                 job TEXT, \
                 department TEXT, \
                 CONSTRAINT credits_one_title CHECK ((movie_id IS NULL) <> (show_id IS NULL)), \
                 CONSTRAINT credits_cast_columns CHECK (kind <> 'cast' OR \
                     (billing_order IS NOT NULL AND billing_order >= 0 \
                      AND job IS NULL AND department IS NULL)), \
                 CONSTRAINT credits_crew_columns CHECK (kind <> 'crew' OR \
                     (job IS NOT NULL AND character IS NULL AND billing_order IS NULL))\
             )",
        )
        .await?;
        db.execute_unprepared("CREATE INDEX idx_credits_person_id ON credits (person_id)")
            .await?;
        db.execute_unprepared("CREATE INDEX idx_credits_movie_id ON credits (movie_id)")
            .await?;
        db.execute_unprepared("CREATE INDEX idx_credits_show_id ON credits (show_id)")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Dropping a table drops its indexes and constraints with it.
        db.execute_unprepared("DROP TABLE credits").await?;
        db.execute_unprepared("DROP TABLE people").await?;

        Ok(())
    }
}
