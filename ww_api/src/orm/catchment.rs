//! SeaORM entity: `catchments`

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "catchments")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub name: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::catchment_member::Entity")]
    Members,
}

impl Related<super::catchment_member::Entity> for Entity {
    fn to() -> RelationDef { Relation::Members.def() }
}

impl ActiveModelBehavior for ActiveModel {}
