//! `ww_api` — Axum REST service backed by PostgreSQL for the ww_biosec
//! wastewater surveillance pipeline.
//!
//! * PostgreSQL is the system of record (sites, catchments, observations,
//!   alerts, review decisions, append-only audit log).
//! * The `ww_detection` engine (EWMA baselines + spectral hypergraph score) runs
//!   in-process; its state is derived from the database by replay
//!   (see [`engine`]), so a restart loses nothing.
//! * Every `/v1` route requires `Authorization: Bearer <token>`.
//!
//! ## SeaORM integration (v0.7)
//!
//! `ww_api` now exposes a SeaORM [`DatabaseConnection`] on [`AppState`]
//! alongside the existing `deadpool-postgres` pool.  The `orm` module holds
//! typed entity definitions and typed query helpers for all six tables.  The
//! raw `tokio-postgres` code is retained for the hot-path operations that rely
//! on PostgreSQL-specific features (advisory locks, UNNEST bulk inserts,
//! `nextval`).

pub mod audit;
pub mod db;
pub mod engine;
pub mod error;
pub mod handlers;
pub mod network;
/// SeaORM entity layer.  Entities mirror the DDL in `migrations/0001_init.sql`
/// and `0002_audit_chain.sql`.  Use `orm::connect(database_url)` to obtain a
/// [`DatabaseConnection`] and the typed query helpers in this module for
/// read-heavy routes.
pub mod orm;
pub mod rounds;
pub mod webhook;

use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::header,
    middleware::{self, Next},
    response::Response,
    routing::{get, post, put},
    Router,
};
use deadpool_postgres::Pool;
use sea_orm::DatabaseConnection;
use tokio::sync::Mutex;

use crate::engine::Engine;
use crate::error::{ApiError, ApiResult};
use crate::webhook::{WebhookConfig, WebhookDispatcher};

pub struct AppState {
    pub pool: Pool,
    /// SeaORM connection for the entity / typed-query layer.
    pub db:   DatabaseConnection,
    pub engine: Mutex<Engine>,
    /// `None` disables auth (explicit opt-in in `main`; used by tests).
    pub token: Option<String>,
    /// Outbound webhook dispatcher for n8n integration.
    /// Fires are async fire-and-forget; failures are logged, never fatal.
    pub hooks: WebhookDispatcher,
}

impl AppState {
    /// Migrate, seed the analyte catalog, and build the detector from the
    /// database contents.
    pub async fn init(pool: Pool, token: Option<String>) -> ApiResult<Arc<Self>> {
        // Build the SeaORM connection from the same DATABASE_URL env var.
        // Fallback: derive the DSN from the pool's manager config — but since
        // we always have the URL available at the call sites that build the
        // pool, callers should prefer `init_with_url`.
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://ww:ww@localhost/ww_biosec".into());
        Self::init_with_url(pool, token, &database_url).await
    }

    /// Preferred entry-point: accepts the database URL explicitly so the
    /// SeaORM connection is initialised deterministically.
    pub async fn init_with_url(
        pool: Pool,
        token: Option<String>,
        database_url: &str,
    ) -> ApiResult<Arc<Self>> {
        db::migrate(&pool).await?;
        db::seed_analytes(&pool).await?;

        // Open the SeaORM connection.
        let db = orm::connect(database_url)
            .await
            .map_err(|e| ApiError::internal(format!("sea-orm connect: {e}")))?;

        let mut engine = Engine::empty();
        engine.rebuild(&pool).await?;
        let hooks = {
            let cfg = WebhookConfig::from_env();
            if cfg.base_url.is_some() {
                tracing::info!(base_url = cfg.base_url.as_deref().unwrap(), "n8n webhooks enabled");
            } else {
                tracing::info!("n8n webhooks disabled (WW_N8N_BASE_URL not set)");
            }
            WebhookDispatcher::new(cfg)
        };
        Ok(Arc::new(Self { pool, db, engine: Mutex::new(engine), token, hooks }))
    }

    /// z-score of a prospective value against the live baseline (diagnostics/tests).
    pub async fn peek_z(&self, site: &str, analyte: &str, log10_conc: f64) -> Option<f64> {
        self.engine.lock().await.detector.peek_z(site, analyte, log10_conc)
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/analytes", get(handlers::analytes))
        .route("/network", get(network::get_network).put(network::put_network))
        .route("/rounds", post(rounds::post_round))
        .route("/observations", get(handlers::list_observations))
        .route("/alerts", get(handlers::list_alerts))
        .route("/alerts/:id", get(handlers::get_alert))
        .route("/alerts/:id/evidence", get(handlers::alert_evidence))
        .route("/alerts/:id/review", post(handlers::review_alert))
        .route("/summary", get(handlers::summary))
        .route("/audit", get(handlers::list_audit))
        .route("/audit/export", get(handlers::export_audit))
        .route("/audit/verify", get(handlers::verify_audit_chain))
        .route("/network/site-weights", get(handlers::get_site_weights).put(handlers::put_site_weights))
        .route("/network/spectral-mode", put(handlers::put_spectral_mode))
        // ── SeaORM-backed read routes ──────────────────────────────────────
        .route("/orm/analytes", get(handlers::orm_analytes))
        .route("/orm/sites", get(handlers::orm_sites))
        .route("/orm/summary", get(handlers::orm_summary))
        .route("/orm/alerts", get(handlers::orm_alerts))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));

    Router::new()
        .route("/healthz", get(handlers::healthz))
        .nest("/v1", api)
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .with_state(state)
}

async fn require_token(
    State(st): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if let Some(expected) = &st.token {
        let presented = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            return Err(ApiError::Unauthorized);
        }
    }
    Ok(next.run(req).await)
}

/// Length-independent-ish constant-time comparison (no early exit on content).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn token_comparison() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret2"));
        assert!(!constant_time_eq(b"", b"secret"));
        assert!(constant_time_eq(b"", b""));
    }
}
