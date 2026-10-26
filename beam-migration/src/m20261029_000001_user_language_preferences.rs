use sea_orm_migration::prelude::*;

/// Per-user audio and subtitle language preferences (issue #264).
///
/// - `user_language_preferences`: one row per user who set any. A user with
///   no row has the defaults -- no preferred language, forced subtitles only
///   -- so nothing is backfilled. `audio_languages` and `subtitle_languages`
///   are ISO 639-2/B codes, most preferred first, each three lowercase
///   letters and at most 16 of them
///   (`beam_domain::models::language_preferences::MAX_PREFERRED_LANGUAGES`);
///   `subtitle_mode` is `off`, `forced_only` or `on`. Goes with its user.
///
/// `down()` drops the table.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // A `CHECK` holding every element of a list to three lowercase
        // letters, with no function of our own: joined with commas, the list
        // must be comma-separated three-letter groups, and exactly
        // `cardinality` of them -- so no element holds a comma or two
        // groups, and no element is `NULL`, which `array_to_string` skips.
        // `cardinality` rather than `array_length`, which is `NULL` for an
        // empty array.
        let codes = |column: &str| {
            format!(
                "{column} TEXT[] NOT NULL \
                     CONSTRAINT user_language_preferences_{column} CHECK (\
                         cardinality({column}) <= 16 \
                         AND array_to_string({column}, ',') ~ '^([a-z]{{3}}(,[a-z]{{3}})*)?$' \
                         AND char_length(array_to_string({column}, ',')) \
                             = greatest(cardinality({column}) * 4 - 1, 0))"
            )
        };
        db.execute_unprepared(&format!(
            "CREATE TABLE user_language_preferences (\
                 user_id UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE, \
                 {audio}, \
                 {subtitle}, \
                 subtitle_mode TEXT NOT NULL \
                     CONSTRAINT user_language_preferences_subtitle_mode \
                     CHECK (subtitle_mode IN ('off', 'forced_only', 'on')), \
                 updated_at TIMESTAMPTZ NOT NULL\
             )",
            audio = codes("audio_languages"),
            subtitle = codes("subtitle_languages"),
        ))
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE user_language_preferences")
            .await?;
        Ok(())
    }
}
