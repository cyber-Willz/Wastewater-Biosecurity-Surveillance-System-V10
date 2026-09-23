//! SeaORM entity: `observations`

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "observations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub obs_id:      i64,
    pub round_id:    i64,
    pub site_id:     String,
    pub analyte:     String,
    pub observed_on: Date,
    pub log10_conc:  f64,
    pub n_samples:   i32,
    pub source:      String,
    pub ingested_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::round::Entity",
        from = "Column::RoundId",
        to = "super::round::Column::RoundId"
    )]
    Round,
    #[sea_orm(
        belongs_to = "super::monitoring_site::Entity",
        from = "Column::SiteId",
        to = "super::monitoring_site::Column::SiteId"
    )]
    MonitoringSite,
    #[sea_orm(
        belongs_to = "super::analyte::Entity",
        from = "Column::Analyte",
        to = "super::analyte::Column::Name"
    )]
    Analyte,
    #[sea_orm(has_one = "super::alert::Entity")]
    Alert,
}

impl Related<super::round::Entity> for Entity {
    fn to() -> RelationDef { Relation::Round.def() }
}
impl Related<super::monitoring_site::Entity> for Entity {
    fn to() -> RelationDef { Relation::MonitoringSite.def() }
}
impl Related<super::analyte::Entity> for Entity {
    fn to() -> RelationDef { Relation::Analyte.def() }
}
impl Related<super::alert::Entity> for Entity {
    fn to() -> RelationDef { Relation::Alert.def() }
}

impl ActiveModelBehavior for ActiveModel {}
