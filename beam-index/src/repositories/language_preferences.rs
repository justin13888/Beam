//! SQL implementation of [`LanguagePreferenceRepository`] (issue #264).
//!
//! A user's preferences are one `user_language_preferences` row, replaced
//! by one `INSERT ... ON CONFLICT (user_id) DO UPDATE`, so two clients
//! saving at once leave one whole set and never half of each.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::sea_query::OnConflict;
use sea_orm::{DatabaseConnection, DbErr, EntityTrait, Set};
use uuid::Uuid;

use beam_domain::models::language_preferences::{LanguageCode, LanguagePreferences, SubtitleMode};
use beam_domain::repositories::LanguagePreferenceRepository;
use beam_domain::services::{Clock, RealClock};
use beam_entity::user_language_preference;

/// SQL-based implementation of the LanguagePreferenceRepository trait.
#[derive(Debug, Clone)]
pub struct SqlLanguagePreferenceRepository {
    db: Arc<DatabaseConnection>,
    /// Source of `updated_at`.
    clock: Arc<dyn Clock>,
}

impl SqlLanguagePreferenceRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self::with_clock(db, Arc::new(RealClock))
    }

    pub fn with_clock(db: Arc<DatabaseConnection>, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }
}

/// The preferences a stored row holds. A value the table's `CHECK`s should
/// have refused is an error, never a preference read some other way.
fn preferences_of(row: user_language_preference::Model) -> Result<LanguagePreferences, DbErr> {
    let user_language_preference::Model {
        user_id,
        audio_languages,
        subtitle_languages,
        subtitle_mode,
        updated_at: _,
    } = row;
    let codes = |stored: Vec<String>| -> Result<Vec<LanguageCode>, DbErr> {
        stored
            .iter()
            .map(|code| {
                LanguageCode::parse(code).ok_or_else(|| {
                    DbErr::Custom(format!(
                        "user {user_id} prefers an unknown language {code:?}"
                    ))
                })
            })
            .collect()
    };
    let mode = SubtitleMode::parse(&subtitle_mode).ok_or_else(|| {
        DbErr::Custom(format!(
            "user {user_id} has an unknown subtitle mode {subtitle_mode:?}"
        ))
    })?;
    LanguagePreferences::new(codes(audio_languages)?, codes(subtitle_languages)?, mode)
        .map_err(|err| DbErr::Custom(format!("user {user_id}: {err}")))
}

fn stored(codes: &[LanguageCode]) -> Vec<String> {
    codes.iter().map(|code| code.as_str().to_owned()).collect()
}

#[async_trait]
impl LanguagePreferenceRepository for SqlLanguagePreferenceRepository {
    async fn find(&self, user_id: Uuid) -> Result<LanguagePreferences, DbErr> {
        user_language_preference::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .map_or_else(|| Ok(LanguagePreferences::default()), preferences_of)
    }

    async fn replace(&self, user_id: Uuid, preferences: &LanguagePreferences) -> Result<(), DbErr> {
        use user_language_preference::Column;

        let row = user_language_preference::ActiveModel {
            user_id: Set(user_id),
            audio_languages: Set(stored(preferences.audio_languages())),
            subtitle_languages: Set(stored(preferences.subtitle_languages())),
            subtitle_mode: Set(preferences.subtitle_mode().as_str().to_owned()),
            updated_at: Set(self.clock.now().into()),
        };
        user_language_preference::Entity::insert(row)
            .on_conflict(
                OnConflict::column(Column::UserId)
                    .update_columns([
                        Column::AudioLanguages,
                        Column::SubtitleLanguages,
                        Column::SubtitleMode,
                        Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await?;
        Ok(())
    }
}
