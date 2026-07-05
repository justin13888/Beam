//! Show entity

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "shows")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,

    pub title: String,
    pub title_localized: Option<String>,
    pub description: Option<String>,
    pub year: Option<i32>,

    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,

    pub tmdb_id: Option<i32>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<i32>,
    pub anilist_id: Option<i32>,

    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::season::Entity")]
    Seasons,
    #[sea_orm(has_one = "super::metadata_enrichment::Entity")]
    MetadataEnrichment,
}

impl Related<super::season::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Seasons.def()
    }
}

impl Related<super::metadata_enrichment::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::MetadataEnrichment.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
