//! SeaORM entity: `alerts`

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "alerts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub alert_id:       i64,
    pub obs_id:         i64,
    pub round_id:       i64,
    pub site_id:        String,
    pub analyte:        String,
    pub observed_on:    Date,
    pub log10_conc:     f64,
    pub ewma:           f64,
    pub z_score:        f64,
    pub spectral_score: f64,
    pub severity:       String,
    pub n_obs:          i32,
    pub alpha:          f64,
    pub status:         String,
    pub analyst_notes:  String,
    pub reviewed_by:    Option<String>,
    pub reviewed_at:    Option<DateTimeWithTimeZone>,
    pub created_at:     DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::observation::Entity",
        from = "Column::ObsId",
        to = "super::observation::Column::ObsId"
    )]
    Observation,
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
}

impl Related<super::observation::Entity> for Entity {
    fn to() -> RelationDef { Relation::Observation.def() }
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

impl ActiveModelBehavior for ActiveModel {}
