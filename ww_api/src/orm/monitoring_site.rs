//! SeaORM entity: `monitoring_sites`

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "monitoring_sites")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub site_id:    String,
    pub name:       Option<String>,
    pub region:     Option<String>,
    pub created_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::catchment_member::Entity")]
    CatchmentMembers,
    #[sea_orm(has_many = "super::observation::Entity")]
    Observations,
    #[sea_orm(has_many = "super::alert::Entity")]
    Alerts,
}

impl Related<super::catchment_member::Entity> for Entity {
    fn to() -> RelationDef { Relation::CatchmentMembers.def() }
}
impl Related<super::observation::Entity> for Entity {
    fn to() -> RelationDef { Relation::Observations.def() }
}
impl Related<super::alert::Entity> for Entity {
    fn to() -> RelationDef { Relation::Alerts.def() }
}

impl ActiveModelBehavior for ActiveModel {}
