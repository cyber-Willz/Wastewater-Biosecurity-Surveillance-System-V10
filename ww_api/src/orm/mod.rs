//! SeaORM entity layer for `ww_api`.
//!
//! All table-level structs are defined here.  The existing `tokio-postgres` /
//! `deadpool-postgres` code continues to own the hot path (round ingestion,
//! migration, advisory locking, UNNEST bulk inserts); this module provides:
//!
//! * Typed `Model` structs for every table.
//! * Convenience query helpers that use `DatabaseConnection`.
//! * The `connect` function that creates a SeaORM connection from the same
//!   `DATABASE_URL` already used by the pool.
//!
//! # Why two drivers?
//!
//! `tokio-postgres` exposes the PostgreSQL-specific features we need:
//! advisory locks, `UNNEST` bulk inserts, `pg_advisory_xact_lock`, and
//! `nextval`.  SeaORM's async API sits on top of `sqlx` which uses a
//! separate connection pool; the two coexist safely because PostgreSQL
//! itself serialises concurrent writers at the transaction level.

pub mod alert;
pub mod analyte;
pub mod audit_log;
pub mod catchment;
pub mod catchment_member;
pub mod monitoring_site;
pub mod observation;
pub mod round;

use sea_orm::{ConnectOptions, Database, DbErr};
use std::time::Duration;

/// Open a SeaORM `DatabaseConnection` against the same database URL used by
/// the `deadpool-postgres` pool.  The connection pool managed by SeaORM
/// (sqlx internally) is sized conservatively — the `deadpool` pool owns the
/// bulk of the capacity.
pub async fn connect(database_url: &str) -> Result<sea_orm::DatabaseConnection, DbErr> {
    let mut opts = ConnectOptions::new(database_url);
    opts.max_connections(4)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(10))
        .idle_timeout(Duration::from_secs(30))
        .sqlx_logging(false);
    Database::connect(opts).await
}

// ── Re-export commonly used SeaORM traits ─────────────────────────────────────

pub use sea_orm::{
    ActiveModelTrait,
    ColumnTrait,
    DatabaseConnection,
    EntityTrait,
    ModelTrait,
    QueryFilter,
    QueryOrder,
    QuerySelect,
    Set,
};

// ── Typed query helpers ───────────────────────────────────────────────────────

use sea_orm::{Order, PaginatorTrait};
use serde_json::{json, Value};

/// Fetch the summary counts using SeaORM's paginator helpers.
///
/// Returns the same JSON shape as `handlers::summary` for parity checking.
pub async fn summary_counts(db: &DatabaseConnection) -> Result<Value, DbErr> {
    let n_sites  = monitoring_site::Entity::find().count(db).await?;
    let n_obs    = observation::Entity::find().count(db).await?;
    let n_rounds = round::Entity::find().count(db).await?;
    let n_alerts = alert::Entity::find().count(db).await?;
    let n_audit  = audit_log::Entity::find().count(db).await?;
    Ok(json!({
        "sites":         n_sites,
        "observations":  n_obs,
        "rounds":        n_rounds,
        "alerts":        n_alerts,
        "audit_entries": n_audit,
    }))
}

/// Fetch the analyte catalog rows ordered by category + name.
pub async fn list_analytes(db: &DatabaseConnection) -> Result<Vec<analyte::Model>, DbErr> {
    use analyte::{Column, Entity};
    Entity::find()
        .order_by(Column::Category, Order::Asc)
        .order_by(Column::Name, Order::Asc)
        .all(db)
        .await
}

/// Find a single alert by `alert_id`.
pub async fn find_alert(
    db: &DatabaseConnection,
    alert_id: i64,
) -> Result<Option<alert::Model>, DbErr> {
    alert::Entity::find_by_id(alert_id).one(db).await
}

/// List monitoring sites ordered by site_id.
pub async fn list_sites(
    db: &DatabaseConnection,
) -> Result<Vec<monitoring_site::Model>, DbErr> {
    use monitoring_site::{Column, Entity};
    Entity::find()
        .order_by(Column::SiteId, Order::Asc)
        .all(db)
        .await
}

/// List catchment members (for topology queries).
pub async fn list_catchment_members(
    db: &DatabaseConnection,
) -> Result<Vec<catchment_member::Model>, DbErr> {
    use catchment_member::{Column, Entity};
    Entity::find()
        .order_by(Column::Catchment, Order::Asc)
        .order_by(Column::SiteId, Order::Asc)
        .all(db)
        .await
}

/// List all audit log entries, oldest first.
pub async fn list_audit_entries(
    db: &DatabaseConnection,
) -> Result<Vec<audit_log::Model>, DbErr> {
    use audit_log::{Column, Entity};
    Entity::find()
        .order_by(Column::AuditId, Order::Asc)
        .all(db)
        .await
}

/// Paginated alert list (CRITICAL first).
pub async fn list_alerts_paged(
    db: &DatabaseConnection,
    status:   Option<&str>,
    severity: Option<&str>,
    analyte:  Option<&str>,
    site_id:  Option<&str>,
    limit:    u64,
    offset:   u64,
) -> Result<Vec<alert::Model>, DbErr> {
    use alert::{Column, Entity};
    use sea_orm::sea_query::Expr;
    use sea_orm::Condition;

    let mut q = Entity::find();

    let mut cond = Condition::all();
    if let Some(s) = status   { cond = cond.add(Column::Status.eq(s)); }
    if let Some(s) = severity { cond = cond.add(Column::Severity.eq(s)); }
    if let Some(s) = analyte  { cond = cond.add(Column::Analyte.eq(s)); }
    if let Some(s) = site_id  { cond = cond.add(Column::SiteId.eq(s)); }

    q = q.filter(cond)
         // CASE severity ordering: CRITICAL=0, RED=1, AMBER=2
         // SeaORM doesn't have a native CASE-ORDER helper, so we use a raw
         // ORDER expression via custom column expression.
         .order_by_asc(Expr::cust(
             "CASE severity WHEN 'CRITICAL' THEN 0 WHEN 'RED' THEN 1 ELSE 2 END"
         ))
         .order_by(Column::ObservedOn, Order::Asc)
         .order_by(Column::AlertId, Order::Asc)
         .offset(offset)
         .limit(limit);

    q.all(db).await
}

/// Fetch observations belonging to a specific round.
pub async fn observations_for_round(
    db: &DatabaseConnection,
    round_id: i64,
) -> Result<Vec<observation::Model>, DbErr> {
    use observation::{Column, Entity};
    Entity::find()
        .filter(Column::RoundId.eq(round_id))
        .order_by(Column::SiteId, Order::Asc)
        .all(db)
        .await
}

/// Fetch the audit log entries for given target_ids.
pub async fn audit_for_targets(
    db: &DatabaseConnection,
    target_ids: &[String],
) -> Result<Vec<audit_log::Model>, DbErr> {
    use audit_log::{Column, Entity};
    Entity::find()
        .filter(Column::TargetId.is_in(target_ids.iter().map(|s| s.as_str())))
        .order_by(Column::AuditId, Order::Asc)
        .all(db)
        .await
}
