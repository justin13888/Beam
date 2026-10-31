//! Collection entity: a provider's grouping of movies, such as a franchise
//! (TMDB's `belongs_to_collection`; issue #262).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "collections")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// The provider-qualified id of the collection (`tmdb:2344`); unique, so
    /// one collection is one row however many movies belong to it.
    #[sea_orm(unique)]
    pub external_ref: String,
    pub name: String,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::collection_movie::Entity")]
    CollectionMovie,
}

impl Related<super::collection_movie::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CollectionMovie.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
