//! SeaORM entity: `rounds`

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "rounds")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub round_id:         i64,
    pub analyte:          String,
    pub observed_on:      Date,
    pub n_observations:   i32,
    pub n_warm:           i32,
    pub spectral_score:   f64,
    pub spread_threshold: f64,
    pub n_alerts:         i32,
    pub source:           String,
    pub created_at:       DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::analyte::Entity",
        from = "Column::Analyte",
        to = "super::analyte::Column::Name"
    )]
    Analyte,
    #[sea_orm(has_many = "super::observation::Entity")]
    Observations,
    #[sea_orm(has_many = "super::alert::Entity")]
    Alerts,
}

impl Related<super::analyte::Entity> for Entity {
    fn to() -> RelationDef { Relation::Analyte.def() }
}
impl Related<super::observation::Entity> for Entity {
    fn to() -> RelationDef { Relation::Observations.def() }
}
impl Related<super::alert::Entity> for Entity {
    fn to() -> RelationDef { Relation::Alerts.def() }
}

impl ActiveModelBehavior for ActiveModel {}
