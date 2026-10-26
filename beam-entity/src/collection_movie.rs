//! Which collection a movie belongs to (issue #262). Keyed by the movie: a
//! movie belongs to at most one collection.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "collection_movies")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub movie_id: Uuid,
    pub collection_id: Uuid,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::collection::Entity",
        from = "Column::CollectionId",
        to = "super::collection::Column::Id"
    )]
    Collection,
    #[sea_orm(
        belongs_to = "super::movie::Entity",
        from = "Column::MovieId",
        to = "super::movie::Column::Id"
    )]
    Movie,
}

impl Related<super::collection::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Collection.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
