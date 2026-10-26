//! Credit entity: one role a person has on one movie or show (issue #261).
//!
//! Exactly one of `movie_id` and `show_id` is set, which a `CHECK` holds it
//! to. `kind` is `cast` or `crew`: a cast credit carries `character` and
//! `billing_order`, a crew credit `job` and `department`, and `CHECK`s hold
//! each kind to its own columns.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// `credits.kind` for an on-screen role.
pub const KIND_CAST: &str = "cast";
/// `credits.kind` for a production role.
pub const KIND_CREW: &str = "crew";

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "credits")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub person_id: Uuid,
    pub movie_id: Option<Uuid>,
    pub show_id: Option<Uuid>,
    /// [`KIND_CAST`] or [`KIND_CREW`].
    pub kind: String,
    pub character: Option<String>,
    pub billing_order: Option<i32>,
    pub job: Option<String>,
    pub department: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::person::Entity",
        from = "Column::PersonId",
        to = "super::person::Column::Id"
    )]
    Person,
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

impl Related<super::person::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Person.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
