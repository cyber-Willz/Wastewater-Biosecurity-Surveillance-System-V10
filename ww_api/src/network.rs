//! Sewer-network topology: `GET/PUT /v1/network`.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::audit;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct SiteIn {
    pub site_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CatchmentIn {
    pub name: String,
    pub sites: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct NetworkIn {
    pub sites: Vec<SiteIn>,
    pub catchments: Vec<CatchmentIn>,
    #[serde(default = "default_actor")]
    pub actor: String,
}

fn default_actor() -> String {
    "system".into()
}

/// Declare the complete topology. Sites are upserted; catchments are replaced;
/// sites omitted from the payload are removed unless they already have
/// observations (409). The spectral model and detector are rebuilt afterwards.
pub async fn put_network(
    State(st): State<Arc<AppState>>,
    Json(req): Json<NetworkIn>,
) -> ApiResult<Json<Value>> {
    if req.sites.len() < 2 {
        return Err(ApiError::Unprocessable("a network needs at least 2 sites".into()));
    }
    let ids: HashSet<&str> = req.sites.iter().map(|s| s.site_id.as_str()).collect();
    if ids.len() != req.sites.len() {
        return Err(ApiError::Unprocessable("duplicate site_id in sites".into()));
    }
    let mut cnames = HashSet::new();
    for c in &req.catchments {
        if !cnames.insert(c.name.as_str()) {
            return Err(ApiError::Unprocessable(format!("duplicate catchment '{}'", c.name)));
        }
        if let Some(bad) = c.sites.iter().find(|s| !ids.contains(s.as_str())) {
            return Err(ApiError::Unprocessable(format!(
                "catchment '{}' references site '{bad}' that is not in `sites`",
                c.name
            )));
        }
    }

    // Serialise against round ingestion for the whole change + rebuild.
    let mut eng = st.engine.lock().await;

    let write = async {
        let mut conn = st.pool.get().await?;
        let tx = conn.transaction().await?;
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&crate::db::ADVISORY_KEY]).await?;

        let existing: HashSet<String> = tx
            .query("SELECT site_id FROM monitoring_sites", &[])
            .await?
            .iter()
            .map(|r| r.get::<_, String>(0))
            .collect();

        let mut added = 0usize;
        for s in &req.sites {
            tx.execute(
                "INSERT INTO monitoring_sites (site_id, name, region) VALUES ($1,$2,$3)
                 ON CONFLICT (site_id) DO UPDATE SET name = EXCLUDED.name, region = EXCLUDED.region",
                &[&s.site_id, &s.name, &s.region],
            )
            .await?;
            if !existing.contains(&s.site_id) {
                added += 1;
                audit::record(&tx, &req.actor, audit::SITE_ADDED, &format!("site:{}", s.site_id), "")
                    .await?;
            }
        }

        let keep: Vec<String> = req.sites.iter().map(|s| s.site_id.clone()).collect();
        let removed = tx
            .execute("DELETE FROM monitoring_sites WHERE NOT (site_id = ANY($1))", &[&keep])
            .await? as usize;

        tx.execute("DELETE FROM catchments", &[]).await?;
        for c in &req.catchments {
            tx.execute("INSERT INTO catchments (name) VALUES ($1)", &[&c.name]).await?;
            let members: Vec<String> =
                c.sites.iter().collect::<BTreeSet<_>>().into_iter().cloned().collect();
            tx.execute(
                "INSERT INTO catchment_members (catchment, site_id)
                 SELECT $1::text, unnest($2::text[])",
                &[&c.name, &members],
            )
            .await?;
        }

        audit::record(
            &tx,
            &req.actor,
            audit::NETWORK_UPDATED,
            "network",
            &json!({
                "sites": req.sites.len(), "catchments": req.catchments.len(),
                "sites_added": added, "sites_removed": removed,
            })
            .to_string(),
        )
        .await?;
        tx.commit().await?;
        Ok::<_, ApiError>((added, removed))
    };

    let (added, removed) = match write.await {
        Ok(v) => v,
        Err(e) => return Err(e), // transaction rolled back; engine state untouched
    };

    // Topology changed: rebuild the spectral model, re-derive the null
    // threshold and replay history into a fresh detector.
    if let Err(e) = eng.rebuild(&st.pool).await {
        eng.dirty = true;
        return Err(e);
    }

    let body = json!({
        "sites": req.sites.len(),
        "catchments": req.catchments.len(),
        "sites_added": added,
        "sites_removed": removed,
        "spread_threshold": eng.spread_threshold,
        "fiedler_value": eng.network.as_ref().map(|n| n.fiedler_value()),
    });
    // Fire n8n webhook after rebuild (fire-and-forget).
    st.hooks.fire_network_updated(&body);
    Ok(Json(body))
}

pub async fn get_network(State(st): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    {
        let mut eng = st.engine.lock().await;
        if eng.dirty {
            eng.rebuild(&st.pool).await?;
        }
    }
    let conn = st.pool.get().await?;
    let sites = conn
        .query(
            r#"SELECT site_id, name, region FROM monitoring_sites ORDER BY site_id COLLATE "C""#,
            &[],
        )
        .await?;
    let members = conn
        .query(
            r#"SELECT catchment, site_id FROM catchment_members
               ORDER BY catchment COLLATE "C", site_id COLLATE "C""#,
            &[],
        )
        .await?;

    let mut catchments: Vec<(String, Vec<String>)> = Vec::new();
    for r in &members {
        let (c, s): (String, String) = (r.get(0), r.get(1));
        match catchments.last_mut() {
            Some((n, v)) if *n == c => v.push(s),
            _ => catchments.push((c, vec![s])),
        }
    }

    let eng = st.engine.lock().await;
    Ok(Json(json!({
        "sites": sites.iter().map(|r| json!({
            "site_id": r.get::<_, String>(0),
            "name": r.get::<_, Option<String>>(1),
            "region": r.get::<_, Option<String>>(2),
        })).collect::<Vec<_>>(),
        "catchments": catchments.iter().map(|(n, v)| json!({"name": n, "sites": v})).collect::<Vec<_>>(),
        "spectral": {
            "n_sites": eng.network.as_ref().map(|n| n.n_sites()),
            "fiedler_value": eng.network.as_ref().map(|n| n.fiedler_value()),
            "spread_threshold": eng.spread_threshold,
            "null_quantile": crate::engine::NULL_QUANTILE,
        },
    })))
}
