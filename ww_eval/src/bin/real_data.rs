//! Evaluate the shipped `ww_detection` engine (EWMA/Welford statistical
//! detector + spectral hypergraph network score) against real public
//! wastewater surveillance data: Public Health Scotland's national
//! SARS-CoV-2 wastewater monitoring programme (BioRDM/COVID-Wastewater-Scotland,
//! Scientific Data paper), May 2020 - Feb 2022, N1 gene RT-qPCR.
//!
//! Input: a pre-aggregated weekly CSV (site, health_board, week_start,
//! log10_conc, n_samples_in_week) — see the accompanying preprocessing notes.
//!
//! ## v0.6 — Full sensitivity/specificity table
//!
//! Previous versions reported only "was at least one alert raised during a
//! known wave?".  That is a coarse hit/miss indicator that hides the
//! within-wave timing, misses between-wave false-alarm rate, and cannot
//! compare AMBER vs RED vs CRITICAL thresholds.
//!
//! This version computes a per-wave, per-severity confusion matrix:
//!
//! ```text
//! For each (wave, severity_floor) pair:
//!   TP = (site, week) pairs where an alert of ≥ severity was raised AND
//!        the week falls inside the wave window.
//!   FP = alerts of ≥ severity raised OUTSIDE every wave window.
//!   FN = (site, wave) pairs where no alert of ≥ severity was raised during
//!        the wave.
//!   TN = (site, week) pairs outside every wave window that produced no
//!        alert (any severity).
//! ```
//!
//! From these, sensitivity = TP / (TP + FN) and specificity = TN / (TN + FP)
//! are reported for each (wave, severity_floor) cell.
//!
//! A per-site summary table is also printed, listing the site's first alert
//! date in each wave (lead time vs wave start), the site's total false-alarm
//! count, and the site's observation count (to flag low-density sites).
//!
//! The spectral score's contribution can be isolated by running the binary
//! twice: once with the default `--spectral enabled` and once with
//! `--spectral disabled`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs::File;

use ww_detection::{AnomalyDetector, DetectionSeverity, SewageNetwork, SpectralMode};

#[derive(Debug, Clone)]
struct Obs {
    week_start: String,
    log10_conc: f64,
    /// Number of grab/composite samples pooled into this aggregate.
    n_samples:  u32,
}

// ── Wave windows ──────────────────────────────────────────────────────────────
//
// Widely-reported approximate UK/Scotland COVID-19 case-wave windows.
// Dates are inclusive [start, end).  NOT derived from this dataset.

const WAVE_WINDOWS: &[(&str, &str, &str)] = &[
    ("Alpha wave",   "2020-12-01", "2021-02-15"),
    ("Delta wave",   "2021-06-15", "2021-10-15"),
    ("Omicron wave", "2021-12-01", "2022-01-31"),
];

fn in_wave(date: &str) -> Option<&'static str> {
    WAVE_WINDOWS
        .iter()
        .find(|(_, s, e)| date >= *s && date < *e)
        .map(|(name, _, _)| *name)
}

// Severity floors for the confusion-matrix columns.
const SEVERITY_FLOORS: &[(&str, DetectionSeverity)] = &[
    ("AMBER+",    DetectionSeverity::Amber),
    ("RED+",      DetectionSeverity::Red),
    ("CRITICAL+", DetectionSeverity::Critical),
];

fn meets_floor(sev: &DetectionSeverity, floor: &DetectionSeverity) -> bool {
    sev >= floor
}

// ── Per-alert record ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct AlertRecord {
    site:     String,
    week:     String,
    severity: DetectionSeverity,
    z_score:  f64,
    spectral: f64,
    n_obs:    usize,
}

// ── Confusion matrix ──────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
struct ConfusionCell {
    tp: u64,
    fp: u64,
    tn: u64,
    fn_: u64,
}

impl ConfusionCell {
    fn sensitivity(&self) -> Option<f64> {
        let denom = self.tp + self.fn_;
        if denom == 0 { None } else { Some(self.tp as f64 / denom as f64) }
    }
    fn specificity(&self) -> Option<f64> {
        let denom = self.tn + self.fp;
        if denom == 0 { None } else { Some(self.tn as f64 / denom as f64) }
    }
    fn ppv(&self) -> Option<f64> {
        let denom = self.tp + self.fp;
        if denom == 0 { None } else { Some(self.tp as f64 / denom as f64) }
    }
}

fn fmt_pct(v: Option<f64>) -> String {
    v.map(|f| format!("{:5.1}%", 100.0 * f)).unwrap_or_else(|| "  N/A ".into())
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| "data/scotland_weekly.csv".to_string());
    let trace_path = args.get(2).cloned().unwrap_or_else(|| "data/real_trace.csv".to_string());
    let alpha_override: Option<f64> = args.get(3).and_then(|s| s.parse().ok());
    let spectral_arg = args.iter().position(|a| a == "--spectral")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str());
    let spectral_mode = match spectral_arg {
        Some("enabled")  => SpectralMode::Enabled,
        Some("disabled") => SpectralMode::Disabled,
        _                => SpectralMode::DEFAULT,      // gated(>=2_catchments)
    };

    let file = File::open(&path).unwrap_or_else(|e| panic!("cannot open {path}: {e}"));
    let mut rdr = csv::Reader::from_reader(file);

    let mut by_site: BTreeMap<String, Vec<Obs>> = BTreeMap::new();
    let mut site_hb: HashMap<String, String> = HashMap::new();

    // CSV columns: site, health_board, week_start, log10_conc, n_samples
    for rec in rdr.records() {
        let rec = rec.expect("csv row");
        let site = rec[0].to_string();
        let hb   = rec[1].to_string();
        let week = rec[2].to_string();
        let log10_conc: f64 = rec[3].parse().expect("log10_conc");
        let n_samples: u32  = rec.get(4).and_then(|v| v.parse().ok()).unwrap_or(1);
        site_hb.insert(site.clone(), hb);
        by_site.entry(site).or_default().push(Obs { week_start: week, log10_conc, n_samples });
    }
    for v in by_site.values_mut() {
        v.sort_by(|a, b| a.week_start.cmp(&b.week_start));
    }

    let sites: Vec<String> = by_site.keys().cloned().collect();
    eprintln!("# loaded {} sites from {}", sites.len(), path);
    for s in &sites {
        eprintln!("#   {:20} n_weeks={:4}  health_board={}",
            s, by_site[s].len(), site_hb[s]);
    }

    // Build spectral network from health-board pseudo-catchments (known
    // approximation: the real dataset does not publish sewer connectivity).
    let mut hb_members: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in &sites {
        hb_members.entry(site_hb[s].clone()).or_default().push(s.clone());
    }
    let catchments: Vec<(String, Vec<String>)> = hb_members
        .into_iter()
        .filter(|(_, m)| m.len() >= 2)
        .collect();
    eprintln!("# real catchments (≥2 sites): {}", catchments.len());

    let network = SewageNetwork::build(&catchments, &sites).expect("network build");
    let n_real_catchments = network.n_real_catchments();
    let spread_thr = network.null_threshold(0.95, 20_000, 0x5eed_5eed);
    eprintln!("# spread_threshold(q95)={:.4}  fiedler={:.5}  spectral_mode={}",
        spread_thr, network.fiedler_value(), spectral_mode);
    eprintln!("# NOTE: spectral score uses health-board proxies, not real sewer topology.");
    eprintln!("#       Its contribution here is weaker than in a deployment with real catchments.");

    let alpha = alpha_override.unwrap_or_else(ww_eval::sars_cov2::ewma_alpha);
    eprintln!("# alpha in use: {:.4} ({})",
        alpha, if alpha_override.is_some() { "override" } else { "default SARS-CoV-2" });

    // Build the union of all week dates.
    let mut all_weeks: Vec<String> = by_site
        .values().flatten().map(|o| o.week_start.clone()).collect();
    all_weeks.sort();
    all_weeks.dedup();

    let mut cursor: HashMap<String, usize> =
        sites.iter().map(|s| (s.clone(), 0usize)).collect();

    let mut detector = AnomalyDetector::new()
        .with_spread_threshold(spread_thr)
        .with_spectral_mode(spectral_mode);

    // ── Trace writer ────────────────────────────────────────────────────────
    let mut trace = csv::Writer::from_path(&trace_path).expect("trace file");
    trace.write_record([
        "site", "week_start", "log10_conc", "n_samples",
        "z_before_ingest", "ewma_before", "n_obs_before",
    ]).unwrap();

    // ── Collect all alert records ────────────────────────────────────────────
    let mut alert_records: Vec<AlertRecord> = Vec::new();

    // Track which (site, week) pairs were observed (for TN counting).
    let mut all_site_weeks: HashSet<(String, String)> = HashSet::new();

    for week in &all_weeks {
        // Phase 1: build concentration map for this round.
        let mut concs: HashMap<String, f64> = HashMap::new();
        let mut nsamp_map: HashMap<String, u32> = HashMap::new();
        for s in &sites {
            let c = cursor[s];
            let obs_list = &by_site[s];
            if c < obs_list.len() && &obs_list[c].week_start == week {
                concs.insert(s.clone(), obs_list[c].log10_conc);
                nsamp_map.insert(s.clone(), obs_list[c].n_samples);
            }
        }
        if concs.is_empty() { continue; }

        // Phase 2: peek z-scores for the spectral residual field.
        let zmap: HashMap<String, f64> = concs
            .iter()
            .filter_map(|(s, c)| detector.peek_z(s, "SARS-CoV-2", *c).map(|z| (s.clone(), z)))
            .collect();
        let spectral_raw = network.spectral_score_residual(&zmap);

        // Phase 3: ingest + detect.
        for s in &sites {
            if let Some(&c) = concs.get(s) {
                let n_samples = nsamp_map[s];
                let z_before   = detector.peek_z(s, "SARS-CoV-2", c);
                let ewma_before = detector.baseline_mut().ewma(s, "SARS-CoV-2");
                let n_before   = detector.baseline_mut().n_obs(s, "SARS-CoV-2");
                trace.write_record(&[
                    s.clone(), week.clone(),
                    format!("{c:.3}"),
                    n_samples.to_string(),
                    z_before.map(|z| format!("{z:.3}")).unwrap_or_default(),
                    ewma_before.map(|e| format!("{e:.3}")).unwrap_or_default(),
                    n_before.to_string(),
                ]).unwrap();

                all_site_weeks.insert((s.clone(), week.clone()));

                if let Some(ev) = detector.observe_analyte_weighted(
                    s, "SARS-CoV-2", c, spectral_raw, alpha,
                    ww_eval::sars_cov2::Z_THRESHOLD,
                    n_samples, n_real_catchments,
                ) {
                    alert_records.push(AlertRecord {
                        site:     s.clone(),
                        week:     week.clone(),
                        severity: ev.severity.clone(),
                        z_score:  ev.z_score,
                        spectral: ev.spectral_score,
                        n_obs:    ev.n_obs,
                    });
                }
                *cursor.get_mut(s).unwrap() += 1;
            }
        }
    }
    trace.flush().unwrap();
    eprintln!("# wrote full weekly trace to {trace_path}");

    // ── Confusion matrix ──────────────────────────────────────────────────────
    //
    // For each (wave, severity_floor):
    //   TP: (site, week) with an alert ≥ floor inside the wave.
    //   FP: (site, week) with an alert ≥ floor outside every wave.
    //   FN: (site, wave) with no alert ≥ floor inside the wave (for sites with
    //       observations during that wave).
    //   TN: (site, week) outside every wave, no alert ≥ floor.

    // Collect per-wave alerting sites.
    let mut wave_alerted: HashMap<(&str, &str), HashSet<String>> = HashMap::new(); // (wave, floor_str) → sites
    let mut all_alerted: HashSet<(String, String)> = HashSet::new();  // (site, week) with any alert

    for ar in &alert_records {
        all_alerted.insert((ar.site.clone(), ar.week.clone()));
        for (floor_str, floor) in SEVERITY_FLOORS {
            if meets_floor(&ar.severity, floor) {
                if let Some(wname) = in_wave(&ar.week) {
                    wave_alerted
                        .entry((wname, floor_str))
                        .or_default()
                        .insert(ar.site.clone());
                }
            }
        }
    }

    // Sites that had ≥ 1 observation during each wave.
    let mut sites_in_wave: HashMap<&str, HashSet<String>> = HashMap::new();
    for (s, w) in &all_site_weeks {
        if let Some(wname) = in_wave(w) {
            sites_in_wave.entry(wname).or_default().insert(s.clone());
        }
    }

    println!("\n{:=<80}", "");
    println!("SENSITIVITY / SPECIFICITY TABLE");
    println!("{:=<80}", "");

    // Header
    let floor_headers = SEVERITY_FLOORS.iter().map(|(s, _)| format!("{:^38}", s)).collect::<Vec<_>>().join(" | ");
    println!("{:<14}  {}", "Wave", floor_headers);
    println!("{:<14}  {}", "", SEVERITY_FLOORS.iter().map(|_|
        format!("{:>6} {:>6} {:>6} {:>6} {:>6} {:>6}",
            "Sens", "Spec", "PPV", "TP", "FP", "FN")
    ).collect::<Vec<_>>().join(" | "));
    println!("{:-<120}", "");

    for (wname, wstart, wend) in WAVE_WINDOWS {
        let in_wave_sites = sites_in_wave.get(*wname).cloned().unwrap_or_default();
        let n_wave_obs = all_site_weeks.iter()
            .filter(|(_, w)| w.as_str() >= *wstart && w.as_str() < *wend)
            .count();
        let n_out_obs = all_site_weeks.iter()
            .filter(|(_, w)| in_wave(w).is_none())
            .count();

        let mut row_parts = Vec::new();
        for (floor_str, floor) in SEVERITY_FLOORS {
            // TP: (site, week) pairs with alert ≥ floor inside this wave.
            let tp = alert_records.iter()
                .filter(|ar| in_wave(&ar.week) == Some(*wname) && meets_floor(&ar.severity, floor))
                .count() as u64;

            // FP: any (site, week) with alert ≥ floor outside all waves.
            let fp = alert_records.iter()
                .filter(|ar| in_wave(&ar.week).is_none() && meets_floor(&ar.severity, floor))
                .count() as u64;

            // FN: in-wave sites that never raised ≥ floor during this wave.
            let fn_ = in_wave_sites.iter()
                .filter(|s| {
                    !alert_records.iter().any(|ar| {
                        &ar.site == *s
                            && in_wave(&ar.week) == Some(*wname)
                            && meets_floor(&ar.severity, floor)
                    })
                })
                .count() as u64;

            // TN: (site, week) outside all waves with no alert ≥ floor.
            let tn = (n_out_obs as u64).saturating_sub(fp);

            let cell = ConfusionCell { tp, fp, tn, fn_ };
            row_parts.push(format!("{} {} {} {:>4} {:>4} {:>4}",
                fmt_pct(cell.sensitivity()),
                fmt_pct(cell.specificity()),
                fmt_pct(cell.ppv()),
                tp, fp, fn_,
            ));
            let _ = n_wave_obs;
        }
        println!("{:<14}  {}", wname, row_parts.join(" | "));
    }

    // ── Per-site detail ───────────────────────────────────────────────────────
    println!("\n{:=<80}", "");
    println!("PER-SITE ALERT SUMMARY");
    println!("{:=<80}", "");
    let wave_names: Vec<&str> = WAVE_WINDOWS.iter().map(|(n, _, _)| *n).collect();
    let hdr2 = wave_names.iter().map(|w| format!("{:>18}", w)).collect::<Vec<_>>().join("  ");
    println!("{:<20}  {:>6}  {:>5}  {}",
        "Site", "n_weeks", "FP", hdr2);
    println!("{:-<90}", "");

    for s in &sites {
        let n_weeks = by_site[s].len();
        let fp = alert_records.iter()
            .filter(|ar| &ar.site == s && in_wave(&ar.week).is_none())
            .count();

        let wave_cols: Vec<String> = WAVE_WINDOWS.iter().map(|(wname, _, _)| {
            let first_alert = alert_records.iter()
                .filter(|ar| &ar.site == s && in_wave(&ar.week) == Some(wname))
                .min_by_key(|ar| &ar.week);
            match first_alert {
                Some(ar) => format!("{:>18}", &ar.week[..10]),
                None     => {
                    let had_obs = all_site_weeks.iter()
                        .any(|(site, w)| site == s && in_wave(w) == Some(wname));
                    if had_obs { format!("{:>18}", "(miss)") } else { format!("{:>18}", "--") }
                }
            }
        }).collect();

        println!("{:<20}  {:>6}  {:>5}  {}",
            s, n_weeks, fp, wave_cols.join("  "));
    }

    // ── Overall summary ───────────────────────────────────────────────────────
    println!("\n{:=<80}", "");
    println!("OVERALL SUMMARY  (spectral_mode={})", spectral_mode);
    println!("{:=<80}", "");
    let total_alerts = alert_records.len();
    let in_wave_alerts = alert_records.iter().filter(|ar| in_wave(&ar.week).is_some()).count();
    let out_wave_alerts = total_alerts - in_wave_alerts;
    println!("  Total AMBER+ alerts        : {total_alerts}");
    println!("  Inside a known wave        : {in_wave_alerts}  ({:.1}%)",
        100.0 * in_wave_alerts as f64 / total_alerts.max(1) as f64);
    println!("  Outside known waves (FP)   : {out_wave_alerts}  ({:.1}%)",
        100.0 * out_wave_alerts as f64 / total_alerts.max(1) as f64);
    println!("  Weeks evaluated            : {}", all_weeks.len());
    println!("  Site-weeks observed        : {}", all_site_weeks.len());
    for (wname, _, _) in WAVE_WINDOWS {
        let hit = alert_records.iter().any(|ar| in_wave(&ar.week) == Some(wname));
        println!("  Wave '{}': detected = {}", wname, hit);
    }
}
