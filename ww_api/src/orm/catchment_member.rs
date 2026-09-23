//! SeaORM entity: `catchment_members`

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "catchment_members")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub catchment: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub site_id:   String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::catchment::Entity",
        from = "Column::Catchment",
        to = "super::catchment::Column::Name"
    )]
    Catchment,
    #[sea_orm(
        belongs_to = "super::monitoring_site::Entity",
        from = "Column::SiteId",
        to = "super::monitoring_site::Column::SiteId"
    )]
    MonitoringSite,
}

impl Related<super::catchment::Entity> for Entity {
    fn to() -> RelationDef { Relation::Catchment.def() }
}
impl Related<super::monitoring_site::Entity> for Entity {
    fn to() -> RelationDef { Relation::MonitoringSite.def() }
}

impl ActiveModelBehavior for ActiveModel {}
