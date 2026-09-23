//! Monte-Carlo evaluation of `ww_detection` (the exact statistical + spectral
//! engine shipped in `ww_biosec`) over thousands of randomized outbreak
//! scenarios on the 8-site Belize sewage network topology.
//!
//! For each of N seeds we:
//!   1. Draw a random outbreak: site, peak day, amplitude (log10 units above
//!      baseline), width (sigma_days), and — with probability 0.5 — a
//!      "spreading" companion pulse at 1-2 other sites in the same catchment
//!      (delayed, attenuated), vs. an "isolated" single-site pulse otherwise.
//!   2. Simulate 22 days (7-day warm-up + 15 days) of noisy SARS-CoV-2-profile
//!      log10 concentrations at all 8 sites using the *same* EWMA/Welford
//!      baseline + spectral network scoring code used in `ww_runner::scenario`.
//!   3. Record whether the pulse site raised an AMBER+ alert while the pulse
//!      was actually present, the lead/lag time relative to the pulse peak,
//!      the maximum severity reached, and whether the spectral network score
//!      ever upgraded a tier.
//!   4. Separately track the false-alarm rate at *non-pulse* sites (null
//!      condition: EWMA baseline + noise only).
//!
//! Output: one CSV row per run to stdout; run summary statistics to stderr.

use std::collections::HashMap;
use std::env;

use ww_detection::{AnomalyDetector, DetectionSeverity};
use ww_eval::{belize_network, pulse_contribution, sars_cov2, SplitMix64, MC_SITES};

const N_DAYS: usize = 22;
const WARMUP_DAYS: usize = 7;
const NULL_SAMPLES: usize = 20_000;
const NULL_SEED: u64 = 0x5eed_5eed;
const NULL_QUANTILE: f64 = 0.95;

struct Draw {
    site_idx: usize,
    companions: Vec<usize>, // indices of companion sites (spreading scenario only)
    peak_day: f64,
    amplitude: f64,
    sigma_days: f64,
    scenario: &'static str, // "isolated" | "spreading"
}

/// Same catchment groupings as `ww_runner::scenario`, by site index into
/// `MC_SITES` (0=bcity_north 1=bcity_south 2=bcity_wwtp 3=belmopan_core
/// 4=belmopan_ind 5=orange_walk 6=san_ignacio 7=dangriga).
fn catchment_of(idx: usize) -> &'static [usize] {
    match idx {
        0 | 1 | 2 => &[0, 1, 2],
        3 | 4 => &[3, 4],
        5 => &[5],
        6 => &[6],
        7 => &[7],
        _ => unreachable!(),
    }
}

fn draw_scenario(rng: &mut SplitMix64) -> Draw {
    let site_idx = rng.index(MC_SITES.len());
    let peak_day = rng.uniform(9.0, 16.0);
    let amplitude = rng.uniform(0.8, 3.5);
    let sigma_days = rng.uniform(1.0, 3.0);

    let spreading = rng.next_f64() < 0.5;
    let mut companions = Vec::new();
    if spreading {
        for &c in catchment_of(site_idx) {
            if c != site_idx && rng.next_f64() < 0.7 {
                companions.push(c);
            }
        }
    }
    let scenario = if !companions.is_empty() { "spreading" } else { "isolated" };

    Draw { site_idx, companions, peak_day, amplitude, sigma_days, scenario }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let n_runs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5_000);
    let base_seed: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0xC0FFEE);

    let network = belize_network();
    let spread_thr = network.null_threshold(NULL_QUANTILE, NULL_SAMPLES, NULL_SEED);
    eprintln!(
        "# ww_eval monte_carlo: {n_runs} runs, base_seed=0x{base_seed:x}, \
         network spread_threshold(q{:.0})={:.4}, fiedler={:.5}",
        NULL_QUANTILE * 100.0, spread_thr, network.fiedler_value(),
    );

    println!(
        "run_id,scenario,site,n_companions,peak_day,amplitude,sigma_days,\
         detected,lead_time_days,max_severity,spectral_upgrade_used,detect_day"
    );

    let mut n_detected = 0u64;
    let mut n_spectral_upgrade = 0u64;
    let mut lead_times: Vec<f64> = Vec::new();

    // Null-condition false-alarm tracking: (site, non-pulse, non-companion) x
    // (day >= WARMUP_DAYS) is an eligible "null" site-day.
    let mut null_site_days: u64 = 0;
    let mut null_alerts: u64 = 0;

    for run in 0..n_runs {
        let mut rng = SplitMix64(base_seed ^ (run.wrapping_mul(0x9E3779B97F4A7C15) + 1));
        let draw = draw_scenario(&mut rng);

        let mut detector = AnomalyDetector::new().with_spread_threshold(spread_thr);

        let pulse_sites: Vec<usize> = std::iter::once(draw.site_idx)
            .chain(draw.companions.iter().copied())
            .collect();

        let onset = draw.peak_day - 2.0 * draw.sigma_days;
        let offset = draw.peak_day + 2.0 * draw.sigma_days;

        let mut detected = false;
        let mut detect_day: Option<f64> = None;
        let mut max_severity = DetectionSeverity::Green;
        let mut spectral_upgrade_used = false;

        for day in 0..N_DAYS {
            let d = day as f64;

            let mut concs: HashMap<String, f64> = HashMap::new();
            for (si, site) in MC_SITES.iter().enumerate() {
                // Deterministic per-(run, day, site) noise draw.
                let mut noise_rng = SplitMix64(
                    base_seed
                        ^ run.wrapping_mul(0xA24BAED4963EE407)
                        ^ (day as u64).wrapping_mul(97)
                        ^ (si as u64).wrapping_mul(1_000_003),
                );
                let noise = sars_cov2::NOISE_STD * noise_rng.next_normal();

                let mut pulse = 0.0;
                if si == draw.site_idx {
                    pulse += pulse_contribution(d, draw.peak_day, draw.amplitude, draw.sigma_days);
                }
                if draw.companions.contains(&si) {
                    // Companion pulses: attenuated (0.4-0.7x) and lagged (1-3 days),
                    // drawn once per run (not per day) for consistency.
                    let mut c_rng = SplitMix64(base_seed ^ run ^ (si as u64).wrapping_mul(7919));
                    let atten = c_rng.uniform(0.4, 0.7);
                    let lag = c_rng.uniform(1.0, 3.0);
                    pulse += pulse_contribution(
                        d, draw.peak_day + lag, draw.amplitude * atten, draw.sigma_days,
                    );
                }

                let conc = (sars_cov2::BASELINE_LOG + noise + pulse).max(-2.0);
                concs.insert(site.to_string(), conc);
            }

            let zmap: HashMap<String, f64> = MC_SITES
                .iter()
                .filter_map(|s| detector.peek_z(s, "SARS-CoV-2", *concs.get(*s).unwrap()).map(|z| (s.to_string(), z)))
                .collect();
            let spectral = network.spectral_score_residual(&zmap);

            for (si, site) in MC_SITES.iter().enumerate() {
                let conc = *concs.get(*site).unwrap();
                if let Some(event) = detector.observe_analyte(
                    site, "SARS-CoV-2", conc, spectral,
                    sars_cov2::ewma_alpha(), sars_cov2::Z_THRESHOLD,
                ) {
                    let is_pulse_site = pulse_sites.contains(&si);

                    if is_pulse_site && si == draw.site_idx && d >= onset && d <= offset {
                        if !detected {
                            detected = true;
                            detect_day = Some(d);
                        }
                        if event.severity > max_severity {
                            max_severity = event.severity.clone();
                        }
                        // A spectral upgrade happened iff severity is higher than the
                        // z-only base tier would give (spectral <= spread_thr never upgrades).
                        if event.spectral_score > spread_thr {
                            spectral_upgrade_used = true;
                        }
                    }

                    if !is_pulse_site && day >= WARMUP_DAYS {
                        null_alerts += 1;
                    }
                }

                if !pulse_sites.contains(&si) && day >= WARMUP_DAYS {
                    null_site_days += 1;
                }
            }
        }

        if detected {
            n_detected += 1;
            if let Some(dd) = detect_day {
                lead_times.push(dd - draw.peak_day);
            }
        }
        if spectral_upgrade_used {
            n_spectral_upgrade += 1;
        }

        println!(
            "{},{},{},{},{:.3},{:.3},{:.3},{},{},{},{},{}",
            run,
            draw.scenario,
            MC_SITES[draw.site_idx],
            draw.companions.len(),
            draw.peak_day,
            draw.amplitude,
            draw.sigma_days,
            detected as u8,
            detect_day.map(|dd| format!("{:.3}", dd - draw.peak_day)).unwrap_or_default(),
            max_severity.as_str(),
            spectral_upgrade_used as u8,
            detect_day.map(|dd| format!("{:.3}", dd)).unwrap_or_default(),
        );
    }

    let sensitivity = n_detected as f64 / n_runs as f64;
    let fa_rate = null_alerts as f64 / null_site_days.max(1) as f64;
    let mean_lead = if lead_times.is_empty() { f64::NAN } else { lead_times.iter().sum::<f64>() / lead_times.len() as f64 };
    lead_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median_lead = if lead_times.is_empty() { f64::NAN } else { lead_times[lead_times.len() / 2] };

    eprintln!("# ---- summary ----");
    eprintln!("# runs                    : {n_runs}");
    eprintln!("# sensitivity (recall)    : {:.4}  ({}/{})", sensitivity, n_detected, n_runs);
    eprintln!("# mean lead time (days)   : {:.3}  (negative = alert before nominal peak)", mean_lead);
    eprintln!("# median lead time (days) : {:.3}", median_lead);
    eprintln!("# spectral upgrade rate   : {:.4}  ({}/{})", n_spectral_upgrade as f64 / n_runs as f64, n_spectral_upgrade, n_runs);
    eprintln!("# null-condition FA rate  : {:.5}  ({} alerts / {} site-days)", fa_rate, null_alerts, null_site_days);
}
