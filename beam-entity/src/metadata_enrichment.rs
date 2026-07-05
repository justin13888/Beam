//! Metadata enrichment queue/status entity

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "metadata_enrichment")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,

    // Polymorphic: exactly ONE must be set (movie XOR show)
    pub movie_id: Option<Uuid>,
    pub show_id: Option<Uuid>,

    pub status: EnrichmentStatus,
    pub attempts: i32,
    pub next_attempt_at: Option<DateTimeWithTimeZone>,
    pub enriched_at: Option<DateTimeWithTimeZone>,
    pub match_confidence: Option<f32>,
    /// Canonical `"provider:id"` string, e.g. `"tmdb:603"`.
    pub matched_ref: Option<String>,
    pub force_refresh: bool,
    pub last_error: Option<String>,

    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "enrichment_status")]
pub enum EnrichmentStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "enriched")]
    Enriched,
    #[sea_orm(string_value = "unmatched")]
    Unmatched,
    #[sea_orm(string_value = "failed")]
    Failed,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::movie::Entity",
        from = "Column::MovieId",
        to = "super::movie::Column::Id"
    )]
    Movie,
    #[sea_orm(
        belongs_to = "super::show::Entity",
        from = "Column::ShowId",
        to = "super::show::Column::Id"
    )]
    Show,
}

impl Related<super::movie::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Movie.def()
    }
}

impl Related<super::show::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Show.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
