//! Integration test against a real PostgreSQL. Skipped (with a note) unless
//! `WW_TEST_DATABASE_URL` is set, e.g.
//!
//!   WW_TEST_DATABASE_URL=postgres://ww:pw@127.0.0.1/ww_biosec cargo test -p ww_api
//!
//! Each run uses its own throw-away schema, dropped at the end.

use std::sync::Arc;

use chrono::{Duration, NaiveDate};
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Value};
use ww_api::{db, router, AppState};

const TOKEN: &str = "test-token";

struct Harness {
    base: String,
    http: Client,
    state: Arc<AppState>,
    url: String,
    schema: String,
}

impl Harness {
    async fn start() -> Option<Self> {
        let Ok(url) = std::env::var("WW_TEST_DATABASE_URL") else {
            eprintln!("SKIPPED: set WW_TEST_DATABASE_URL to run the PostgreSQL integration test");
            return None;
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let schema = format!("t_{}_{}", std::process::id(), nanos);
        let pool = db::build_pool(&url, Some(&schema)).await.unwrap();
        let state = AppState::init_with_url(pool, Some(TOKEN.into()), &url).await.unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Some(Self { base: format!("http://{addr}"), http: Client::new(), state, url, schema })
    }

    async fn call(&self, m: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut rb = self.http.request(m, format!("{}{path}", self.base)).bearer_auth(TOKEN);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let resp = rb.send().await.unwrap();
        let status = resp.status();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn cleanup(self) {
        let pool = db::build_pool(&self.url, Some(&self.schema)).await.unwrap();
        let conn = pool.get().await.unwrap();
        conn.batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", self.schema)).await.unwrap();
    }
}

const SITES: [&str; 6] = ["s1", "s2", "s3", "s4", "s5", "s6"];

fn monday(i: i64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2024, 1, 1).unwrap() + Duration::weeks(i)
}

fn quiet_round(i: i64) -> Value {
    // Deterministic, low-noise baseline around log10 = 4.0.
    json!({
        "analyte": "SARS-CoV-2",
        "observed_on": monday(i).to_string(),
        "observations": SITES.iter().enumerate().map(|(k, s)| json!({
            "site_id": s,
            "log10_conc": 4.0 + 0.03 * (((i * 7 + k as i64 * 3) % 5) as f64 - 2.0),
        })).collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn full_flow_persists_reviews_audits_and_replays() {
    let Some(h) = Harness::start().await else { return };

    // -- auth ---------------------------------------------------------------
    let anon = h.http.get(format!("{}/v1/summary", h.base)).send().await.unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(h.http.get(format!("{}/healthz", h.base)).send().await.unwrap().status(), StatusCode::OK);

    // -- rounds before a network exists are refused ---------------------------
    let (st, _) = h.call(Method::POST, "/v1/rounds", Some(quiet_round(0))).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "unknown sites before network defined");

    // -- topology -------------------------------------------------------------
    let net = json!({
        "sites": SITES.iter().map(|s| json!({"site_id": s, "region": "test"})).collect::<Vec<_>>(),
        "catchments": [
            {"name": "A", "sites": ["s1", "s2", "s3"]},
            {"name": "B", "sites": ["s4", "s5", "s6"]},
        ],
    });
    let (st, body) = h.call(Method::PUT, "/v1/network", Some(net)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["sites_added"], 6);
    assert!(body["spread_threshold"].as_f64().unwrap() > 0.0);

    // -- 12 quiet weeks: no alerts ---------------------------------------------
    for i in 0..12 {
        let (st, body) = h.call(Method::POST, "/v1/rounds", Some(quiet_round(i))).await;
        assert_eq!(st, StatusCode::CREATED, "round {i}: {body}");
        assert!(body["alerts"].as_array().unwrap().is_empty(), "round {i} should be quiet: {body}");
    }

    // -- validation -------------------------------------------------------------
    let (st, _) = h.call(Method::POST, "/v1/rounds", Some(quiet_round(5))).await;
    assert_eq!(st, StatusCode::CONFLICT, "out-of-order / duplicate date");
    let mut bad = quiet_round(12);
    bad["observations"][0]["site_id"] = json!("nope");
    let (st, _) = h.call(Method::POST, "/v1/rounds", Some(bad)).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "unknown site");
    let mut dup = quiet_round(12);
    dup["observations"][1]["site_id"] = json!("s1");
    let (st, _) = h.call(Method::POST, "/v1/rounds", Some(dup)).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "duplicate site in round");
    let mut nan = quiet_round(12);
    nan["observations"][0]["log10_conc"] = json!(99.0);
    let (st, _) = h.call(Method::POST, "/v1/rounds", Some(nan)).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "out-of-range value");
    let (st, _) = h.call(Method::POST, "/v1/rounds", Some(json!({
        "analyte": "NotAThing", "observed_on": "2030-01-01",
        "observations": [{"site_id": "s1", "log10_conc": 1.0}],
    }))).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "unknown analyte");

    // Rejected requests must not have consumed baseline state or a date slot.
    let (_, summ) = h.call(Method::GET, "/v1/summary", None).await;
    assert_eq!(summ["rounds"], 12);
    assert_eq!(summ["observations"], 72);

    // -- a genuine spike at s1 ---------------------------------------------------
    let mut spike = quiet_round(12);
    spike["observations"][0]["log10_conc"] = json!(5.6);
    let (st, body) = h.call(Method::POST, "/v1/rounds", Some(spike)).await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let alerts = body["alerts"].as_array().unwrap();
    assert_eq!(alerts.len(), 1, "{body}");
    assert_eq!(alerts[0]["site_id"], "s1");
    assert_eq!(alerts[0]["severity"], "CRITICAL", "z ≫ 5 with sd≈0.03: {body}");
    let alert_id = alerts[0]["alert_id"].as_i64().unwrap();

    // -- review workflow ----------------------------------------------------------
    let (st, _) = h.call(Method::POST, &format!("/v1/alerts/{alert_id}/review"),
        Some(json!({"analyst": "a", "outcome": "escalate"}))).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "escalate needs to_team");
    let (st, _) = h.call(Method::POST, &format!("/v1/alerts/{alert_id}/review"),
        Some(json!({"analyst": "a", "outcome": "dismiss"}))).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "dismiss needs a reason");
    let (st, a) = h.call(Method::POST, &format!("/v1/alerts/{alert_id}/review"),
        Some(json!({"analyst": "Dr. T", "outcome": "confirm", "notes": "real"}))).await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(a["status"], "CONFIRMED");
    assert_eq!(a["reviewed_by"], "Dr. T");
    let (st, _) = h.call(Method::POST, &format!("/v1/alerts/{alert_id}/review"),
        Some(json!({"analyst": "b", "outcome": "confirm"}))).await;
    assert_eq!(st, StatusCode::CONFLICT, "closed alerts cannot be re-reviewed");
    let (st, _) = h.call(Method::POST, "/v1/alerts/999999/review",
        Some(json!({"analyst": "b", "outcome": "confirm"}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // -- audit trail + DB-level immutability ----------------------------------------
    let (_, trail) = h.call(Method::GET, &format!("/v1/audit?target_id=alert:{alert_id}"), None).await;
    let actions: Vec<&str> = trail.as_array().unwrap().iter()
        .map(|e| e["action"].as_str().unwrap()).collect();
    assert!(actions.contains(&"ALERT_RAISED") && actions.contains(&"ALERT_CONFIRMED"), "{actions:?}");

    let conn = h.state.pool.get().await.unwrap();
    for sql in ["UPDATE audit_log SET actor='x'", "DELETE FROM audit_log", "TRUNCATE audit_log"] {
        assert!(conn.execute(sql, &[]).await.is_err(), "audit_log must reject: {sql}");
    }
    drop(conn);

    // -- evidence chain -------------------------------------------------------------
    let (st, ev) = h.call(Method::GET, &format!("/v1/alerts/{alert_id}/evidence"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ev["site"]["catchments"], json!(["A"]));
    assert_eq!(ev["same_round_sites"].as_array().unwrap().len(), 5);

    // -- restart equivalence: a fresh engine rebuilt from PostgreSQL is bit-identical
    let pool2 = db::build_pool(&h.url, Some(&h.schema)).await.unwrap();
    let st2 = AppState::init_with_url(pool2, Some(TOKEN.into()), &h.url).await.unwrap();
    for s in SITES {
        for probe in [3.7, 4.0, 4.4, 5.0] {
            let live = h.state.peek_z(s, "SARS-CoV-2", probe).await;
            let replayed = st2.peek_z(s, "SARS-CoV-2", probe).await;
            assert!(live.is_some(), "{s} should be warm");
            assert_eq!(live.map(f64::to_bits), replayed.map(f64::to_bits),
                "replayed baseline differs from live for {s} @ {probe}");
        }
    }
    {
        let (a, b) = (h.state.engine.lock().await, st2.engine.lock().await);
        assert_eq!(a.last_round, b.last_round);
        assert_eq!(a.spread_threshold.to_bits(), b.spread_threshold.to_bits());
    }

    // -- topology change guards: cannot drop a site that has observations -----------
    let shrink = json!({
        "sites": [{"site_id": "s1"}, {"site_id": "s2"}],
        "catchments": [{"name": "A", "sites": ["s1", "s2"]}],
    });
    let (st, body) = h.call(Method::PUT, "/v1/network", Some(shrink)).await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    let (_, summ) = h.call(Method::GET, "/v1/summary", None).await;
    assert_eq!(summ["sites"], 6, "failed topology change must roll back completely");

    h.cleanup().await;
}
