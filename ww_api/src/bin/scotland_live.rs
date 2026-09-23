//! Live end-to-end client: drives the running `ww_api` server (Axum + PostgreSQL)
//! with the raw Public Health Scotland SARS-CoV-2 wastewater export.
//!
//! Preprocessing deliberately mirrors `ww_eval::scotland_e2e` (weekly Monday
//! mean → 10 gc/L LOD floor → log10 → sites with ≥ min_weeks → Health-Board
//! catchments with ≥ 2 sites) so that the alerts the *server* raises can be
//! diffed row-for-row against the reference binary. Everything after that —
//! network definition, detection, persistence — happens over HTTP.
//!
//! Usage:
//!   WW_API_URL=http://127.0.0.1:8080 WW_API_TOKEN=... \
//!     scotland_live <raw_csv> [alerts_out.csv] [min_weeks]

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs::File;

use chrono::{Datelike, Duration, NaiveDate};
use reqwest::Client;
use serde_json::{json, Value};

const ANALYTE: &str = "SARS-CoV-2";
const LOD_FLOOR_GC_PER_L: f64 = 10.0;

/// Parse like the reference: split on '-', three integers (tolerates
/// unpadded fields); invalid calendar dates are skipped.
fn parse_ymd(s: &str) -> Option<NaiveDate> {
    let p: Vec<&str> = s.trim().split('-').collect();
    if p.len() != 3 {
        return None;
    }
    NaiveDate::from_ymd_opt(p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?)
}

fn week_monday(d: NaiveDate) -> NaiveDate {
    d - Duration::days(d.weekday().num_days_from_monday() as i64)
}

struct Api {
    http: Client,
    base: String,
    token: Option<String>,
}

impl Api {
    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let r = self.http.request(m, format!("{}{}", self.base, path));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    async fn send(&self, rb: reqwest::RequestBuilder, what: &str) -> Result<Value, Box<dyn Error>> {
        let resp = rb.send().await?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(format!("{what}: HTTP {status}: {body}").into());
        }
        Ok(body)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    let raw_path = args.get(1).cloned().unwrap_or_else(|| {
        eprintln!("usage: scotland_live <raw_csv> [alerts_out.csv] [min_weeks]");
        std::process::exit(2);
    });
    let out_path = args.get(2).cloned().unwrap_or_else(|| "scotland_alerts_api.csv".into());
    let min_weeks: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(8);

    let api = Api {
        http: Client::new(),
        base: env::var("WW_API_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into()),
        token: env::var("WW_API_TOKEN").ok().filter(|t| !t.is_empty()),
    };

    // ---- Stage 1: parse + weekly aggregation (same as scotland_e2e) ----------
    let mut rdr = csv::Reader::from_reader(File::open(&raw_path)?);
    let headers = rdr.headers()?.clone();
    let idx = |name: &str| {
        headers.iter().position(|h| h == name).unwrap_or_else(|| panic!("column {name} missing"))
    };
    let (i_site, i_hb, i_date, i_val) =
        (idx("Site"), idx("Health_Board"), idx("Date_collected"), idx("Calculated_mean"));

    let mut weekly: BTreeMap<(String, NaiveDate), (f64, usize)> = BTreeMap::new();
    let mut site_hb: BTreeMap<String, String> = BTreeMap::new();
    let (mut n_rows, mut n_used) = (0u64, 0u64);
    for rec in rdr.records() {
        let rec = rec?;
        n_rows += 1;
        let site = rec[i_site].trim().to_string();
        if site.is_empty() {
            continue;
        }
        let Some(date) = parse_ymd(&rec[i_date]) else { continue };
        let Ok(val) = rec[i_val].trim().parse::<f64>() else { continue };
        let e = weekly.entry((site.clone(), week_monday(date))).or_insert((0.0, 0));
        e.0 += val;
        e.1 += 1;
        site_hb.entry(site).or_insert_with(|| rec[i_hb].trim().to_string());
        n_used += 1;
    }
    eprintln!("# parsed {raw_path}: {n_rows} rows, {n_used} numeric usable, {} site-weeks", weekly.len());

    let mut by_site: BTreeMap<String, Vec<(NaiveDate, f64, usize)>> = BTreeMap::new();
    for ((site, monday), (sum, n)) in weekly {
        let log10c = (sum / n as f64).max(LOD_FLOOR_GC_PER_L).log10();
        by_site.entry(site).or_default().push((monday, log10c, n));
    }
    let n_sites_total = by_site.len();
    by_site.retain(|_, v| v.len() >= min_weeks);
    eprintln!("# {n_sites_total} distinct sites; {} pass min_weeks={min_weeks}", by_site.len());

    // ---- Stage 2: define the network over HTTP --------------------------------
    let mut hb_members: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in by_site.keys() {
        hb_members.entry(site_hb[s].clone()).or_default().push(s.clone());
    }
    let catchments: Vec<Value> = hb_members
        .iter()
        .filter(|(_, m)| m.len() >= 2)
        .map(|(hb, m)| json!({ "name": hb, "sites": m }))
        .collect();
    let net = api
        .send(
            api.req(reqwest::Method::PUT, "/v1/network").json(&json!({
                "actor": "scotland_live",
                "sites": by_site.keys().map(|s| json!({
                    "site_id": s, "name": s, "region": site_hb[s],
                })).collect::<Vec<_>>(),
                "catchments": catchments,
            })),
            "PUT /v1/network",
        )
        .await?;
    eprintln!(
        "# network defined: {} sites, {} catchments, spread_threshold={:.4}, fiedler={:.5}",
        net["sites"], net["catchments"], net["spread_threshold"].as_f64().unwrap_or(f64::NAN),
        net["fiedler_value"].as_f64().unwrap_or(f64::NAN)
    );

    // ---- Stage 3: one HTTP round per week --------------------------------------
    let mut rounds: BTreeMap<NaiveDate, Vec<Value>> = BTreeMap::new();
    for (site, obs) in &by_site {
        for &(monday, log10c, n) in obs {
            rounds.entry(monday).or_default().push(json!({
                "site_id": site, "log10_conc": log10c, "n_samples": n,
            }));
        }
    }
    let total_rounds = rounds.len();
    let (mut n_alerts, mut n_obs) = (0usize, 0usize);
    let t0 = std::time::Instant::now();
    for (i, (week, observations)) in rounds.into_iter().enumerate() {
        n_obs += observations.len();
        let r = api
            .send(
                api.req(reqwest::Method::POST, "/v1/rounds").json(&json!({
                    "analyte": ANALYTE,
                    "observed_on": week.to_string(),
                    "observations": observations,
                    "source": "PHS-Scotland-live",
                    "actor": "scotland_live",
                })),
                &format!("POST /v1/rounds ({week})"),
            )
            .await?;
        n_alerts += r["alerts"].as_array().map_or(0, |a| a.len());
        if (i + 1) % 20 == 0 || i + 1 == total_rounds {
            eprintln!("#   round {}/{total_rounds} ({week}): {n_obs} obs, {n_alerts} alerts so far", i + 1);
        }
    }
    eprintln!("# ingested {total_rounds} rounds in {:.1}s", t0.elapsed().as_secs_f64());

    // ---- Stage 4: read the alerts back out of PostgreSQL via the API -----------
    let mut alerts: Vec<Value> = Vec::new();
    let mut offset = 0;
    loop {
        let page = api
            .send(
                api.req(reqwest::Method::GET, "/v1/alerts")
                    .query(&[("analyte", ANALYTE), ("limit", "1000"), ("offset", &offset.to_string())]),
                "GET /v1/alerts",
            )
            .await?;
        let page = page.as_array().cloned().unwrap_or_default();
        let n = page.len();
        alerts.extend(page);
        if n < 1000 {
            break;
        }
        offset += n;
    }
    alerts.sort_by(|a, b| {
        (a["observed_on"].as_str(), a["site_id"].as_str())
            .cmp(&(b["observed_on"].as_str(), b["site_id"].as_str()))
    });

    // Same first 7 columns / formats as scotland_e2e's alerts CSV.
    let mut w = csv::Writer::from_path(&out_path)?;
    w.write_record(["site", "week_start", "log10_conc", "z_score", "spectral_score", "severity", "n_obs"])?;
    for a in &alerts {
        w.write_record([
            a["site_id"].as_str().unwrap_or("").to_string(),
            a["observed_on"].as_str().unwrap_or("").to_string(),
            format!("{:.3}", a["log10_conc"].as_f64().unwrap_or(f64::NAN)),
            format!("{:.3}", a["z_score"].as_f64().unwrap_or(f64::NAN)),
            format!("{:.3}", a["spectral_score"].as_f64().unwrap_or(f64::NAN)),
            a["severity"].as_str().unwrap_or("").to_string(),
            a["n_obs"].to_string(),
        ])?;
    }
    w.flush()?;

    let summary = api.send(api.req(reqwest::Method::GET, "/v1/summary"), "GET /v1/summary").await?;
    eprintln!("# ---- server summary (from PostgreSQL) ----");
    eprintln!("{}", serde_json::to_string_pretty(&summary)?);
    eprintln!("# wrote {out_path} ({} alerts)", alerts.len());
    Ok(())
}
