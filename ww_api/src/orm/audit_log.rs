//! SeaORM entity: `audit_log`
//!
//! Append-only — the DB enforces this via trigger; no update/delete queries
//! are ever issued through this entity.  Hash-chain columns `prev_hash` and
//! `entry_hash` are added by migration 0002 and default to empty strings for
//! rows written before that migration.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "audit_log")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub audit_id:   i64,
    pub ts:         DateTimeWithTimeZone,
    pub actor:      String,
    pub action:     String,
    pub target_id:  String,
    pub details:    String,
    pub prev_hash:  String,
    pub entry_hash: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
