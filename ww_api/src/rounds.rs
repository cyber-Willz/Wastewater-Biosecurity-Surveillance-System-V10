//! `POST /v1/rounds` — ingest one detection round and persist its outcome.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{json, Value};
use ww_detection::AnomalyEvent;
use ww_domain::{analyte_by_name, AnalyteProfile};

use crate::audit;
use crate::db::ADVISORY_KEY;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

const MAX_OBS_PER_ROUND: usize = 5_000;

#[derive(Debug, Clone, Deserialize)]
pub struct ObservationIn {
    pub site_id: String,
    /// log10(copies/L or µg/L) — already floored / transformed by the caller.
    pub log10_conc: f64,
    #[serde(default = "one")]
    pub n_samples: i32,
}

fn one() -> i32 {
    1
}
fn default_source() -> String {
    "api".into()
}
fn default_actor() -> String {
    "system".into()
}

#[derive(Debug, Deserialize)]
pub struct RoundRequest {
    pub analyte: String,
    pub observed_on: NaiveDate,
    pub observations: Vec<ObservationIn>,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "default_actor")]
    pub actor: String,
}

pub async fn post_round(
    State(st): State<Arc<AppState>>,
    Json(req): Json<RoundRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    // ---- stateless validation -------------------------------------------------
    let profile: &'static AnalyteProfile = analyte_by_name(&req.analyte)
        .ok_or_else(|| ApiError::Unprocessable(format!("unknown analyte '{}'", req.analyte)))?;
    if req.observations.is_empty() {
        return Err(ApiError::Unprocessable("round has no observations".into()));
    }
    if req.observations.len() > MAX_OBS_PER_ROUND {
        return Err(ApiError::Unprocessable(format!(
            "round exceeds {MAX_OBS_PER_ROUND} observations"
        )));
    }
    let mut seen = HashSet::new();
    for o in &req.observations {
        if !o.log10_conc.is_finite() || !(-30.0..=30.0).contains(&o.log10_conc) {
            return Err(ApiError::Unprocessable(format!(
                "site '{}': log10_conc must be finite and within [-30, 30]",
                o.site_id
            )));
        }
        if o.n_samples < 1 {
            return Err(ApiError::Unprocessable(format!("site '{}': n_samples must be >= 1", o.site_id)));
        }
        if !seen.insert(o.site_id.as_str()) {
            return Err(ApiError::Unprocessable(format!("duplicate site '{}' in round", o.site_id)));
        }
    }

    // ---- serialised section: detector state + database write -------------------
    let mut eng = st.engine.lock().await;
    if eng.dirty {
        eng.rebuild(&st.pool).await?;
    }

    let engine_last = eng.last_round.get(&req.analyte).copied();
    if let Some(last) = engine_last {
        if req.observed_on <= last {
            return Err(ApiError::Conflict(format!(
                "rounds must arrive in increasing date order per analyte: \
                 latest '{}' round is {last}, got {}",
                req.analyte, req.observed_on
            )));
        }
    }

    let unknown: Vec<&str> = req
        .observations
        .iter()
        .filter(|o| !eng.sites.contains(&o.site_id))
        .map(|o| o.site_id.as_str())
        .collect();
    if !unknown.is_empty() {
        return Err(ApiError::Unprocessable(format!(
            "unknown site(s) (define them via PUT /v1/network): {}",
            unknown.join(", ")
        )));
    }

    let spread = eng.spread_threshold;
    let alpha = profile.ewma_alpha();
    let z_thr = profile.z_threshold;

    let mut obs = req.observations.clone();
    obs.sort_by(|a, b| a.site_id.cmp(&b.site_id));

    // Log the spectral mode active for this round so the audit trail records
    // whether the score is contributing to severity upgrades. Must be read
    // *before* the mutable field-level borrow below, since `network` and
    // `detector` stay borrowed from `eng` through the rest of this function.
    let spectral_mode_str = eng.spectral_mode.to_string();

    // Disjoint field borrows: immutable network, mutable detector.
    let crate::engine::Engine { network, detector, .. } = &mut *eng;
    let network = network
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("no network defined; PUT /v1/network first".into()))?;

    // Same sequence as ww_eval::scotland_e2e: peek z for the whole round,
    // score the residual field, then absorb each observation.
    let zmap: HashMap<String, f64> = obs
        .iter()
        .filter_map(|o| {
            detector
                .peek_z(&o.site_id, profile.name, o.log10_conc)
                .map(|z| (o.site_id.clone(), z))
        })
        .collect();
    let n_warm = zmap.len();
    let spectral = network.spectral_score_residual(&zmap);

    let n_real_catchments = network.n_real_catchments();

    let mut events: Vec<AnomalyEvent> = Vec::new();
    for o in &obs {
        if let Some(ev) = detector.observe_analyte_weighted(
            &o.site_id, profile.name, o.log10_conc, spectral, alpha, z_thr,
            o.n_samples as u32,
            n_real_catchments,
        ) {
            events.push(ev);
        }
    }

    // The detector has now absorbed this round. If persistence fails for any
    // reason, memory is ahead of the database: mark dirty so the next request
    // rebuilds from the source of truth.
    match persist_round(&st, &req, &obs, engine_last, n_warm, spectral, spread, &events, &spectral_mode_str).await {
        Ok(body) => {
            eng.last_round.insert(req.analyte.clone(), req.observed_on);
            // Extract webhook payload data while we still have `body`.
            let round_id = body["round_id"].as_i64().unwrap_or(0);
            let alerts_payload = body["alerts"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            // Drop the engine lock before firing webhooks so ingestion of the
            // next round is not serialised behind the (async, fire-and-forget)
            // webhook dispatch.
            drop(eng);
            st.hooks.fire_round_ingested(&body);
            st.hooks.fire_alerts_raised(round_id, &alerts_payload);
            Ok((StatusCode::CREATED, Json(body)))
        }
        Err(e) => {
            eng.dirty = true;
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_round(
    st: &AppState,
    req: &RoundRequest,
    obs: &[ObservationIn],
    engine_last: Option<NaiveDate>,
    n_warm: usize,
    spectral: f64,
    spread: f64,
    events: &[AnomalyEvent],
    spectral_mode_str: &str,
) -> ApiResult<Value> {
    let mut conn = st.pool.get().await?;
    let tx = conn.transaction().await?;

    // Multi-instance guard: hold the writer lock and confirm the database agrees
    // with our in-memory view of this analyte's latest round.
    tx.execute("SELECT pg_advisory_xact_lock($1)", &[&ADVISORY_KEY]).await?;
    let db_last: Option<NaiveDate> = tx
        .query_one("SELECT max(observed_on) FROM rounds WHERE analyte = $1", &[&req.analyte])
        .await?
        .get(0);
    if db_last != engine_last {
        return Err(ApiError::Conflict(
            "round state changed underneath this request (another writer?); retry".into(),
        ));
    }

    let round_id: i64 = tx
        .query_one(
            "INSERT INTO rounds (analyte, observed_on, n_observations, n_warm,
                                 spectral_score, spread_threshold, n_alerts, source)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING round_id",
            &[
                &req.analyte, &req.observed_on, &(obs.len() as i32), &(n_warm as i32),
                &spectral, &spread, &(events.len() as i32), &req.source,
            ],
        )
        .await?
        .get(0);

    let sites: Vec<String> = obs.iter().map(|o| o.site_id.clone()).collect();
    let concs: Vec<f64> = obs.iter().map(|o| o.log10_conc).collect();
    let ns: Vec<i32> = obs.iter().map(|o| o.n_samples).collect();
    let rows = tx
        .query(
            "INSERT INTO observations
                 (round_id, site_id, analyte, observed_on, log10_conc, n_samples, source)
             SELECT $1::int8, s, $2::text, $3::date, c, n, $4::text
             FROM UNNEST($5::text[], $6::float8[], $7::int4[]) AS t(s, c, n)
             RETURNING obs_id, site_id",
            &[&round_id, &req.analyte, &req.observed_on, &req.source, &sites, &concs, &ns],
        )
        .await?;
    let obs_ids: HashMap<String, i64> =
        rows.iter().map(|r| (r.get::<_, String>(1), r.get::<_, i64>(0))).collect();

    let mut alert_json = Vec::with_capacity(events.len());
    for ev in events {
        let obs_id = obs_ids[&ev.site_id];
        let alert_id: i64 = tx
            .query_one(
                "INSERT INTO alerts
                     (obs_id, round_id, site_id, analyte, observed_on, log10_conc, ewma,
                      z_score, spectral_score, severity, n_obs, alpha)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING alert_id",
                &[
                    &obs_id, &round_id, &ev.site_id, &ev.analyte, &req.observed_on,
                    &ev.log10_copies, &ev.ewma, &ev.z_score, &ev.spectral_score,
                    &ev.severity.as_str(), &(ev.n_obs as i32), &ev.alpha,
                ],
            )
            .await?
            .get(0);
        audit::record(
            &tx,
            "system",
            audit::ALERT_RAISED,
            &format!("alert:{alert_id}"),
            &json!({
                "site_id": ev.site_id, "analyte": ev.analyte, "severity": ev.severity.as_str(),
                "z_score": ev.z_score, "spectral_score": ev.spectral_score, "round_id": round_id,
            })
            .to_string(),
        )
        .await?;
        alert_json.push(json!({
            "alert_id": alert_id, "site_id": ev.site_id, "severity": ev.severity.as_str(),
            "z_score": ev.z_score, "spectral_score": ev.spectral_score, "ewma": ev.ewma,
            "log10_conc": ev.log10_copies, "n_obs": ev.n_obs,
        }));
    }

    audit::record(
        &tx,
        &req.actor,
        audit::ROUND_INGESTED,
        &format!("round:{round_id}"),
        &json!({
            "analyte": req.analyte, "observed_on": req.observed_on, "n_observations": obs.len(),
            "n_warm": n_warm, "n_alerts": events.len(), "source": req.source,
            "spectral_mode": spectral_mode_str,
        })
        .to_string(),
    )
    .await?;

    tx.commit().await?;

    Ok(json!({
        "round_id": round_id,
        "analyte": req.analyte,
        "observed_on": req.observed_on,
        "n_observations": obs.len(),
        "n_warm": n_warm,
        "spectral_score": spectral,
        "spread_threshold": spread,
        "alerts": alert_json,
    }))
}
