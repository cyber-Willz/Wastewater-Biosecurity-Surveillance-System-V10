//! Connection pool, embedded migrations, and analyte-catalog seeding.

use deadpool_postgres::{ManagerConfig, Pool, RecyclingMethod};
use tokio_postgres::NoTls;
use ww_domain::ANALYTE_CATALOG;

use crate::error::{ApiError, ApiResult};

/// Global advisory-lock key serialising migrations and round writers across
/// server instances ("wwbio" in ASCII-ish hex).
pub const ADVISORY_KEY: i64 = 0x7777_6269_6f01;

const MIGRATIONS: &[(i32, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_audit_chain.sql")),
];

/// Build a pool. When `schema` is given, every connection uses it as its
/// `search_path` (created if missing) — used by the integration tests to get an
/// isolated namespace inside one database.
pub async fn build_pool(database_url: &str, schema: Option<&str>) -> ApiResult<Pool> {
    let mut cfg: tokio_postgres::Config = database_url
        .parse()
        .map_err(|e| ApiError::internal(format!("bad DATABASE_URL: {e}")))?;

    if let Some(schema) = schema {
        if !schema.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(ApiError::internal("schema name must be [A-Za-z0-9_]"));
        }
        let (client, conn) = cfg.connect(NoTls).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        client
            .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS \"{schema}\""))
            .await?;
        cfg.options(&format!("-c search_path={schema}"));
    }

    let mgr = deadpool_postgres::Manager::from_config(
        cfg,
        NoTls,
        ManagerConfig { recycling_method: RecyclingMethod::Fast },
    );
    Pool::builder(mgr)
        .max_size(16)
        .build()
        .map_err(|e| ApiError::internal(format!("pool build: {e}")))
}

/// Apply any unapplied migrations, serialised across instances.
pub async fn migrate(pool: &Pool) -> ApiResult<()> {
    let mut conn = pool.get().await?;
    let tx = conn.transaction().await?;
    tx.execute("SELECT pg_advisory_xact_lock($1)", &[&ADVISORY_KEY]).await?;
    tx.batch_execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    integer PRIMARY KEY,
             applied_at timestamptz NOT NULL DEFAULT now())",
    )
    .await?;
    for (version, sql) in MIGRATIONS {
        let done = tx
            .query_opt("SELECT 1 FROM schema_migrations WHERE version = $1", &[version])
            .await?;
        if done.is_none() {
            tracing::info!(version, "applying migration");
            tx.batch_execute(sql).await?;
            tx.execute("INSERT INTO schema_migrations (version) VALUES ($1)", &[version])
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Mirror the compiled-in analyte catalog into the `analytes` table so that
/// SQL joins and foreign keys see the same parameters the detector uses.
pub async fn seed_analytes(pool: &Pool) -> ApiResult<()> {
    let mut conn = pool.get().await?;
    let tx = conn.transaction().await?;
    for a in ANALYTE_CATALOG.iter() {
        tx.execute(
            "INSERT INTO analytes
                (name, category, target_marker, method, baseline_log, noise_std,
                 decay_rate_k, z_threshold, ewma_alpha)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
             ON CONFLICT (name) DO UPDATE SET
                category = EXCLUDED.category, target_marker = EXCLUDED.target_marker,
                method = EXCLUDED.method, baseline_log = EXCLUDED.baseline_log,
                noise_std = EXCLUDED.noise_std, decay_rate_k = EXCLUDED.decay_rate_k,
                z_threshold = EXCLUDED.z_threshold, ewma_alpha = EXCLUDED.ewma_alpha",
            &[
                &a.name,
                &a.category.as_str(),
                &a.target_marker,
                &a.method,
                &a.baseline_log,
                &a.noise_std,
                &a.decay_rate_k,
                &a.z_threshold,
                &a.ewma_alpha(),
            ],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
