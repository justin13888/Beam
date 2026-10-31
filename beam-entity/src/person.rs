//! Person entity: someone a metadata provider credits on a title (issue #261).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "people")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// The provider-qualified id the person is credited under (`tmdb:287`);
    /// unique, so one person is one row however many titles credit them.
    #[sea_orm(unique)]
    pub external_ref: String,
    pub name: String,
    pub profile_url: Option<String>,
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::credit::Entity")]
    Credit,
}

impl Related<super::credit::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Credit.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
