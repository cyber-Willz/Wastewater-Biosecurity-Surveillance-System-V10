//! SeaORM entity: `analytes`
//!
//! Mirrors the `CREATE TABLE analytes` DDL in `0001_init.sql`.
//! The analyte catalog is seeded from the compiled-in `ANALYTE_CATALOG` on
//! startup; this entity is used for ORM-backed reads and upserts.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "analytes")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub name:          String,
    pub category:      String,
    pub target_marker: String,
    pub method:        String,
    pub baseline_log:  f64,
    pub noise_std:     f64,
    pub decay_rate_k:  f64,
    pub z_threshold:   f64,
    pub ewma_alpha:    f64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::round::Entity")]
    Rounds,
    #[sea_orm(has_many = "super::observation::Entity")]
    Observations,
    #[sea_orm(has_many = "super::alert::Entity")]
    Alerts,
}

impl Related<super::round::Entity> for Entity {
    fn to() -> RelationDef { Relation::Rounds.def() }
}
impl Related<super::observation::Entity> for Entity {
    fn to() -> RelationDef { Relation::Observations.def() }
}
impl Related<super::alert::Entity> for Entity {
    fn to() -> RelationDef { Relation::Alerts.def() }
}

impl ActiveModelBehavior for ActiveModel {}
