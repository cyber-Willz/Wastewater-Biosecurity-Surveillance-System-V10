//! End-to-end evaluation of `ww_detection` against the **raw, unmodified**
//! Public Health Scotland national SARS-CoV-2 wastewater surveillance
//! export (BioRDM/COVID-Wastewater-Scotland, May 2020-Feb 2022, N1 gene
//! RT-qPCR, published with a *Scientific Data* paper).
//!
//! This binary does the *entire* pipeline itself — CSV parsing, weekly
//! aggregation, LOD flooring, log10 transform, Health-Board catchment
//! grouping, and detection — from the file exactly as downloaded from
//! GitHub. There is no separate preprocessing step; see `fetch_and_run.sh`
//! for the one-command "download the live dataset, then run this" wrapper.
//!
//! Usage:
//!   scotland_e2e <raw_csv_path> [alerts_out.csv] [trace_out.csv] [min_weeks] [metric] [alpha]
//!     min_weeks : minimum weekly observations a site needs to be included
//!                 (default 8 - many of the 122 sites have only a handful
//!                 of samples in the whole 21-month period and can't
//!                 support a meaningful baseline).
//!     metric    : "raw" (default) = Calculated_mean, gc/L, or
//!                 "normalized"    = Million_gene_copies_per_person_per_day.
//!                 NOTE: the normalized column triggers a known, separate,
//!                 pre-existing bug (a cold-start variance lock-up when
//!                 early samples are at the LOD floor) - see
//!                 EVALUATION_REPORT.md section 3. Included here for
//!                 reproducing that finding, not recommended for a real run
//!                 until that bug is fixed.
//!     alpha     : override the shipped SARS-CoV-2 EWMA alpha (default:
//!                 shipped value, 0.39).

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs::File;

use ww_detection::{AnomalyDetector, SewageNetwork};

#[derive(Debug, Clone)]
struct WeekObs {
    week_start_ymd: (i32, u32, u32), // (year, month, day) of the Monday
    log10_conc: f64,
    n_samples: usize,
}

const WAVE_WINDOWS: &[(&str, &str, &str)] = &[
    ("Alpha wave",   "2020-12-01", "2021-02-15"),
    ("Delta wave",   "2021-06-15", "2021-10-15"),
    ("Omicron wave", "2021-12-01", "2022-01-31"),
];

fn in_any_wave(date: &str) -> Option<&'static str> {
    WAVE_WINDOWS.iter().find(|(_, s, e)| date >= *s && date < *e).map(|(n, _, _)| *n)
}

/// Parse "YYYY-MM-DD" -> (y, m, d). No external date crate needed for this.
fn parse_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let parts: Vec<&str> = s.trim().split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let y: i32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let d: u32 = parts[2].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`), matching the
/// epoch `from_days` below expects. The earlier version of this function was
/// missing the final `-719468` epoch shift, which silently produced dates
/// about 1970 years in the future (e.g. `3990-07-04` for `2020-07-04`) —
/// caught by inspecting the actual output, not by construction; there was
/// no test guarding this, which the code below now has.
fn to_days(y: i32, m: u32, d: u32) -> i64 {
    let (y, m, d) = (y as i64, m as i64, d as i64);
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

fn from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

fn iso_weekday(days: i64) -> i64 {
    // 2020-01-06 (a Monday) is day `to_days(2020,1,6)`.
    (days - to_days(2020, 1, 6)).rem_euclid(7)
}

fn week_monday(y: i32, m: u32, d: u32) -> (i32, u32, u32) {
    let days = to_days(y, m, d);
    let wd = iso_weekday(days); // 0 = Monday
    from_days(days - wd)
}

fn ymd_str((y, m, d): (i32, u32, u32)) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let raw_path = args.get(1).cloned().unwrap_or_else(|| {
        eprintln!("usage: scotland_e2e <raw_csv_path> [alerts.csv] [trace.csv] [min_weeks] [raw|normalized] [alpha]");
        std::process::exit(2);
    });
    let alerts_path = args.get(2).cloned().unwrap_or_else(|| "scotland_alerts.csv".into());
    let trace_path = args.get(3).cloned().unwrap_or_else(|| "scotland_trace.csv".into());
    let min_weeks: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(8);
    let metric = args.get(5).cloned().unwrap_or_else(|| "raw".into());
    let alpha_override: Option<f64> = args.get(6).and_then(|s| s.parse().ok());

    if metric == "normalized" {
        eprintln!(
            "# WARNING: --metric=normalized reproduces a known, separate, pre-existing bug \
             (cold-start variance lock-up at the LOD floor) - see EVALUATION_REPORT.md section 3."
        );
    }

    // ---- Stage 1: parse the raw CSV exactly as published ----
    let file = File::open(&raw_path).unwrap_or_else(|e| panic!("cannot open {raw_path}: {e}"));
    let mut rdr = csv::Reader::from_reader(file);
    let headers = rdr.headers().expect("csv headers").clone();
    let idx = |name: &str| headers.iter().position(|h| h == name)
        .unwrap_or_else(|| panic!("column {name} not found in {raw_path}"));
    let i_site = idx("Site");
    let i_hb = idx("Health_Board");
    let i_date = idx("Date_collected");
    let i_raw = idx("Calculated_mean");
    let i_norm = idx("Million_gene_copies_per_person_per_day");
    let i_metric = if metric == "normalized" { i_norm } else { i_raw };

    // (site, week_monday) -> running (sum, count) for weekly averaging.
    let mut weekly_sum: BTreeMap<(String, (i32, u32, u32)), (f64, usize)> = BTreeMap::new();
    let mut site_hb: HashMap<String, String> = HashMap::new();

    let mut n_rows = 0u64;
    let mut n_used = 0u64;
    for rec in rdr.records() {
        let rec = rec.expect("csv row");
        n_rows += 1;
        let site = rec[i_site].trim().to_string();
        if site.is_empty() {
            continue;
        }
        let hb = rec[i_hb].trim().to_string();
        let Some(ymd) = parse_ymd(&rec[i_date]) else { continue };
        let raw_val = rec[i_metric].trim();
        let Ok(val) = raw_val.parse::<f64>() else { continue }; // "NA", "", etc. skipped
        let monday = week_monday(ymd.0, ymd.1, ymd.2);
        let entry = weekly_sum.entry((site.clone(), monday)).or_insert((0.0, 0));
        entry.0 += val;
        entry.1 += 1;
        site_hb.entry(site).or_insert(hb);
        n_used += 1;
    }
    eprintln!("# parsed {raw_path}: {n_rows} rows, {n_used} numeric usable, {} site-weeks",
        weekly_sum.len());

    // LOD floor: 10 gc/L for the raw metric, 0.1 million-gc/person/day for
    // the normalized one (same relative floor position).
    let floor = if metric == "normalized" { 0.1 } else { 10.0 };

    let mut by_site: BTreeMap<String, Vec<WeekObs>> = BTreeMap::new();
    for ((site, monday), (sum, n)) in weekly_sum {
        let mean = sum / n as f64;
        let log10c = mean.max(floor).log10();
        by_site.entry(site).or_default().push(WeekObs { week_start_ymd: monday, log10_conc: log10c, n_samples: n });
    }
    for v in by_site.values_mut() {
        v.sort_by_key(|o| o.week_start_ymd);
    }

    let n_sites_total = by_site.len();
    by_site.retain(|_, v| v.len() >= min_weeks);
    eprintln!("# {n_sites_total} distinct sites in the raw export; {} pass the min_weeks={min_weeks} threshold",
        by_site.len());

    let sites: Vec<String> = by_site.keys().cloned().collect();

    // ---- Stage 2: build the network from Health Board catchments ----
    let mut hb_members: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in &sites {
        hb_members.entry(site_hb[s].clone()).or_default().push(s.clone());
    }
    let catchments: Vec<(String, Vec<String>)> = hb_members.into_iter()
        .filter(|(_, m)| m.len() >= 2).collect();
    eprintln!("# {} Health-Board catchments with >=2 monitored sites (of {} sites total)",
        catchments.len(), sites.len());

    let network = SewageNetwork::build(&catchments, &sites).expect("network build");
    let spread_thr = network.null_threshold(0.95, 20_000, 0x5eed_5eed);
    let alpha = alpha_override.unwrap_or_else(ww_eval::sars_cov2::ewma_alpha);
    eprintln!("# spread_threshold(q95)={:.4}  fiedler={:.5}  alpha={:.4} ({})",
        spread_thr, network.fiedler_value(), alpha,
        if alpha_override.is_some() { "override" } else { "shipped default" });

    // ---- Stage 3: run the shared weekly-round grid through the real engine ----
    let mut all_weeks: Vec<(i32, u32, u32)> = by_site.values().flatten().map(|o| o.week_start_ymd).collect();
    all_weeks.sort();
    all_weeks.dedup();

    let mut cursor: HashMap<String, usize> = sites.iter().map(|s| (s.clone(), 0usize)).collect();
    let mut detector = AnomalyDetector::new().with_spread_threshold(spread_thr);

    let mut alerts_wtr = csv::Writer::from_path(&alerts_path).expect("alerts file");
    alerts_wtr.write_record(["site", "week_start", "log10_conc", "z_score", "spectral_score", "severity", "n_obs", "n_samples_in_week", "in_known_wave"]).unwrap();
    let mut trace_wtr = csv::Writer::from_path(&trace_path).expect("trace file");
    trace_wtr.write_record(["site", "week_start", "log10_conc", "z_before_ingest", "ewma_before", "n_obs_before"]).unwrap();

    let mut alerts_total = 0u64;
    let mut alerts_in_wave = 0u64;
    let mut wave_hit: HashMap<&'static str, bool> = WAVE_WINDOWS.iter().map(|(n, _, _)| (*n, false)).collect();

    for &week in &all_weeks {
        let week_str = ymd_str(week);
        let mut concs: HashMap<String, (f64, usize)> = HashMap::new();
        for s in &sites {
            let c = cursor[s];
            let obs_list = &by_site[s];
            if c < obs_list.len() && obs_list[c].week_start_ymd == week {
                concs.insert(s.clone(), (obs_list[c].log10_conc, obs_list[c].n_samples));
            }
        }
        if concs.is_empty() {
            continue;
        }

        let zmap: HashMap<String, f64> = concs.iter()
            .filter_map(|(s, (c, _))| detector.peek_z(s, "SARS-CoV-2", *c).map(|z| (s.clone(), z)))
            .collect();
        let spectral = network.spectral_score_residual(&zmap);

        for s in &sites {
            if let Some(&(c, n_samp)) = concs.get(s) {
                let z_before = detector.peek_z(s, "SARS-CoV-2", c);
                let ewma_before = detector.baseline_mut().ewma(s, "SARS-CoV-2");
                let n_before = detector.baseline_mut().n_obs(s, "SARS-CoV-2");
                trace_wtr.write_record(&[
                    s.clone(), week_str.clone(), format!("{c:.3}"),
                    z_before.map(|z| format!("{z:.3}")).unwrap_or_default(),
                    ewma_before.map(|e| format!("{e:.3}")).unwrap_or_default(),
                    n_before.to_string(),
                ]).unwrap();

                if let Some(event) = detector.observe_analyte(s, "SARS-CoV-2", c, spectral, alpha, ww_eval::sars_cov2::Z_THRESHOLD) {
                    let wave = in_any_wave(&week_str);
                    alerts_total += 1;
                    if let Some(w) = wave {
                        alerts_in_wave += 1;
                        wave_hit.insert(w, true);
                    }
                    alerts_wtr.write_record(&[
                        s.clone(), week_str.clone(), format!("{c:.3}"),
                        format!("{:.3}", event.z_score), format!("{:.3}", event.spectral_score),
                        event.severity.as_str().to_string(), event.n_obs.to_string(),
                        n_samp.to_string(), wave.unwrap_or("").to_string(),
                    ]).unwrap();
                }
                *cursor.get_mut(s).unwrap() += 1;
            }
        }
    }
    alerts_wtr.flush().unwrap();
    trace_wtr.flush().unwrap();

    eprintln!("# ---- summary ----");
    eprintln!("# sites used / total in export : {} / {}", sites.len(), n_sites_total);
    eprintln!("# weekly rounds processed      : {}", all_weeks.len());
    eprintln!("# total AMBER+ alerts          : {alerts_total}");
    eprintln!("# alerts inside a known wave   : {alerts_in_wave} ({:.1}%)",
        100.0 * alerts_in_wave as f64 / alerts_total.max(1) as f64);
    for (name, _, _) in WAVE_WINDOWS {
        eprintln!("# wave '{}': at least one site alerted = {}", name, wave_hit[name]);
    }
    eprintln!("# wrote {alerts_path} and {trace_path}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_known_dates() {
        for &(y, m, d) in &[(2020, 1, 6), (2020, 2, 29), (2021, 12, 31), (2022, 1, 1), (2020, 7, 4)] {
            assert_eq!(from_days(to_days(y, m, d)), (y, m, d), "roundtrip failed for {y}-{m}-{d}");
        }
    }

    #[test]
    fn jan_6_2020_is_the_reference_monday() {
        // 2020-01-06 was a real-world Monday; iso_weekday's epoch depends on it.
        assert_eq!(iso_weekday(to_days(2020, 1, 6)), 0);
        assert_eq!(iso_weekday(to_days(2020, 1, 7)), 1); // Tuesday
        assert_eq!(iso_weekday(to_days(2020, 1, 5)), 6); // Sunday
    }

    #[test]
    fn week_monday_maps_a_wednesday_back_to_its_monday() {
        // 2020-07-08 was a Wednesday; its week's Monday is 2020-07-06.
        assert_eq!(week_monday(2020, 7, 8), (2020, 7, 6));
    }

    #[test]
    fn dates_land_in_the_correct_year() {
        // Regression for the ~1970-year epoch-offset bug this replaced.
        let (y, _, _) = week_monday(2020, 7, 4);
        assert_eq!(y, 2020);
    }
}
