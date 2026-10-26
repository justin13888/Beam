//! One user's audio and subtitle language preferences (issue #264). A user
//! with no row has the defaults: no preferred language, forced subtitles
//! only.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "user_language_preferences")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: Uuid,
    /// ISO 639-2/B codes, most preferred first
    /// (`beam_domain::models::language_preferences::LanguageCode`).
    pub audio_languages: Vec<String>,
    /// ISO 639-2/B codes, most preferred first.
    pub subtitle_languages: Vec<String>,
    /// `off`, `forced_only` or `on`
    /// (`beam_domain::models::language_preferences::SubtitleMode::as_str`,
    /// which a `CHECK` holds it to).
    pub subtitle_mode: String,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id",
        on_delete = "Cascade"
    )]
    User,
}

impl ActiveModelBehavior for ActiveModel {}
