//! Belize national wastewater biosurveillance simulation — multi-target panel.
//!
//! # Network topology
//!
//! ```text
//!   [orange_walk]          [san_ignacio]
//!        │                       │
//!        ▼                       ▼
//!   [bcity_north]  ←──    [bcity_wwtp]  ←── [belmopan_core]
//!        │                   ▲                [belmopan_ind]
//!   [bcity_south] ───────────┘
//!                                             [dangriga]  (standalone coastal)
//! ```
//!
//! Catchment zones (hyperedges for spectral scoring):
//! * **Belize City Trunk** — north, south, wwtp
//! * **Belmopan Zone**     — core, industrial
//! * **Northern District** — orange_walk
//! * **Western Highlands** — san_ignacio
//! * **Southern Coastal**  — dangriga
//!
//! # Analyte panel (18 targets across 4 categories)
//!
//! | Category              | Analytes                                        |
//! |-----------------------|-------------------------------------------------|
//! | Infectious pathogens  | SARS-CoV-2, Mpox, Vibrio cholerae, Influenza A, Poliovirus, Norovirus GII |
//! | Pharmaceutical / AMR  | Amoxicillin, Ciprofloxacin, AMR_blaTEM, Fluoxetine |
//! | Illicit substances    | Fentanyl, Cocaine, Methamphetamine, MDMA        |
//! | Industrial toxicants  | Lead_Pb, PFAS_PFOA, Trichloroethylene, Mercury_Hg |
//! | Explosive precursors  | RDX, HMX, TNT_aminoproducts, PETN_penta, TATP_DHPP, AN_nitrate_excess, Perchlorate |
//!
//! # Outbreak / anomaly scenario (15 simulation days)
//!
//! | Day  | Category   | Event |
//! |------|------------|-------|
//! | 0–7  | all        | Baseline warm-up |
//! | 8–11 | Pathogen   | SARS-CoV-2 cluster, Belize City South → North → WWTP |
//! | 11   | Pathogen   | Mpox traveller import, Belmopan Core |
//! | 9–12 | Illicit    | Fentanyl spike, Belize City South (public-safety event) |
//! | 10   | Illicit    | Cocaine surge, Orange Walk (weekend event) |
//! | 9–11 | Pharma/AMR | Antibiotic (Amoxicillin) usage surge, Belmopan Core |
//! | 10–12| Industrial | Lead pulse, Dangriga (suspected pipe rupture) |
//! | 10–13| Explosive  | RDX/HMX cluster, Belmopan Industrial Zone (suspected IED lab) |
//! | 11   | Explosive  | TATP-DHPP spike, Belize City South (post-blast or preparation event) |
//! | 12   | Explosive  | Perchlorate surge, Orange Walk (pyrotechnic / ANFO link) |

use std::collections::HashMap;

use ontology_engine::prelude::OntologyEngine;

use ww_audit::{AuditAction, AuditLog};
use ww_detection::{AnomalyDetector, SewageNetwork};
use ww_domain::{
    ANALYTE_CATALOG,
    factory::{AlertParams, DomainFactory, SampleParams, SignalParams, SiteParams},
    Result as DomainResult,
};

// ── Site definitions ─────────────────────────────────────────────────────────

struct Site {
    id:            &'static str,
    name:          &'static str,
    region:        &'static str,
    catchment_pop: i64,
    lat:           f64,
    lon:           f64,
}

/// Quantile of the null distribution of the network score used as the
/// severity-upgrade threshold.
const SPREAD_NULL_QUANTILE: f64 = 0.95;

const SITES: &[Site] = &[
    Site { id: "bcity_north",    name: "Belize City North Side",      region: "Belize District",        catchment_pop: 28_000, lat: 17.510, lon: -88.189 },
    Site { id: "bcity_south",    name: "Belize City South Side",      region: "Belize District",        catchment_pop: 24_000, lat: 17.488, lon: -88.194 },
    Site { id: "bcity_wwtp",     name: "Belize City WWTP Inlet",      region: "Belize District",        catchment_pop: 72_000, lat: 17.499, lon: -88.185 },
    Site { id: "belmopan_core",  name: "Belmopan Urban Core",         region: "Cayo District",          catchment_pop: 16_000, lat: 17.252, lon: -88.768 },
    Site { id: "belmopan_ind",   name: "Belmopan Industrial Zone",    region: "Cayo District",          catchment_pop:  4_000, lat: 17.245, lon: -88.762 },
    Site { id: "orange_walk",    name: "Orange Walk Town",            region: "Orange Walk District",   catchment_pop: 14_000, lat: 18.090, lon: -88.560 },
    Site { id: "san_ignacio",    name: "San Ignacio / Santa Elena",   region: "Cayo District",          catchment_pop: 18_000, lat: 17.157, lon: -89.072 },
    Site { id: "dangriga",       name: "Dangriga Town",               region: "Stann Creek District",   catchment_pop: 11_000, lat: 16.971, lon: -88.234 },
];

// ── Pseudo-random number generator (no external dep) ─────────────────────────

fn lcg(seed: u64) -> u64 {
    seed.wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407)
}

/// Approximate standard-normal sample using 12-uniform sum (CLT).
fn approx_normal(seed: u64) -> f64 {
    let mut x = 0.0f64;
    let mut s = seed;
    for _ in 0..12 {
        s = lcg(s);
        x += (s as f64) / (u64::MAX as f64);
    }
    x - 6.0
}

// ── Outbreak / anomaly pulses ─────────────────────────────────────────────────
//
// Each pulse adds  amplitude · exp(−(day − peak)² / (2·σ²))  to the baseline.
// Multi-category events demonstrate that the system simultaneously tracks
// pathogens, drugs, pharmaceuticals, and toxicants with independent baselines.

struct OutbreakPulse {
    site_id:    &'static str,
    analyte:    &'static str,
    peak_day:   f64,
    amplitude:  f64,   // max additional log₁₀ copies/L (or µg/L)
    sigma_days: f64,
}

const PULSES: &[OutbreakPulse] = &[
    // ── INFECTIOUS PATHOGENS ──────────────────────────────────────────────────
    // SARS-CoV-2 cluster starting at Belize City South, day 9 peak
    OutbreakPulse { site_id: "bcity_south",   analyte: "SARS-CoV-2",    peak_day:  9.0, amplitude: 2.8, sigma_days: 2.0 },
    OutbreakPulse { site_id: "bcity_north",   analyte: "SARS-CoV-2",    peak_day: 10.0, amplitude: 1.6, sigma_days: 2.0 },
    OutbreakPulse { site_id: "bcity_wwtp",    analyte: "SARS-CoV-2",    peak_day: 10.5, amplitude: 1.2, sigma_days: 2.0 },
    // Mpox traveller import at Belmopan Urban Core — sharper, localised pulse
    OutbreakPulse { site_id: "belmopan_core", analyte: "Mpox",          peak_day: 11.0, amplitude: 3.2, sigma_days: 1.5 },
    // Norovirus seasonally elevated at Orange Walk (community outbreak)
    OutbreakPulse { site_id: "orange_walk",   analyte: "Norovirus_GII", peak_day:  8.5, amplitude: 1.8, sigma_days: 2.5 },

    // ── ILLICIT SUBSTANCES ────────────────────────────────────────────────────
    // Fentanyl public-safety event at Belize City South — sharp spike
    OutbreakPulse { site_id: "bcity_south",   analyte: "Fentanyl",      peak_day:  9.5, amplitude: 2.2, sigma_days: 1.5 },
    // Cocaine weekend surge at Orange Walk
    OutbreakPulse { site_id: "orange_walk",   analyte: "Cocaine",       peak_day: 10.0, amplitude: 1.9, sigma_days: 1.0 },
    // MAMP elevation, shared between Belmopan sites
    OutbreakPulse { site_id: "belmopan_core", analyte: "Methamphetamine", peak_day: 11.5, amplitude: 1.6, sigma_days: 1.5 },
    OutbreakPulse { site_id: "belmopan_ind",  analyte: "Methamphetamine", peak_day: 12.0, amplitude: 1.2, sigma_days: 1.5 },

    // ── PHARMACEUTICAL / AMR ──────────────────────────────────────────────────
    // Community antibiotic surge (prescription drive) — Belmopan
    OutbreakPulse { site_id: "belmopan_core", analyte: "Amoxicillin",    peak_day:  9.0, amplitude: 1.4, sigma_days: 2.0 },
    // Co-elevation of AMR gene blaTEM consistent with β-lactam over-prescription
    OutbreakPulse { site_id: "belmopan_core", analyte: "AMR_blaTEM",     peak_day: 10.0, amplitude: 0.8, sigma_days: 2.5 },
    // Ciprofloxacin elevation — San Ignacio (possibly related to cholera concern)
    OutbreakPulse { site_id: "san_ignacio",   analyte: "Ciprofloxacin",  peak_day:  8.0, amplitude: 1.2, sigma_days: 2.0 },

    // ── INDUSTRIAL TOXICANTS ─────────────────────────────────────────────────
    // Lead pulse at Dangriga — suspected corroded distribution pipe or
    // illegal workshop discharge
    OutbreakPulse { site_id: "dangriga",      analyte: "Lead_Pb",         peak_day: 10.5, amplitude: 2.0, sigma_days: 1.5 },
    // TCE elevation at Belmopan Industrial Zone — solvent disposal event
    OutbreakPulse { site_id: "belmopan_ind",  analyte: "Trichloroethylene", peak_day: 11.0, amplitude: 1.8, sigma_days: 1.5 },
    // Mercury at Dangriga — possibly same discharge event as Lead
    OutbreakPulse { site_id: "dangriga",      analyte: "Mercury_Hg",      peak_day: 10.5, amplitude: 1.5, sigma_days: 2.0 },
    // ── EXPLOSIVE PRECURSORS ──────────────────────────────────────────────────
    // RDX cluster at Belmopan Industrial Zone — sustained over 4 days,
    // suggesting ongoing manufacturing or storage rather than a one-off release.
    OutbreakPulse { site_id: "belmopan_ind",  analyte: "RDX",              peak_day: 11.0, amplitude: 2.4, sigma_days: 2.0 },
    OutbreakPulse { site_id: "belmopan_core", analyte: "RDX",              peak_day: 11.5, amplitude: 1.4, sigma_days: 2.0 },
    // HMX co-elevation (consistent with plastic explosive formulation)
    OutbreakPulse { site_id: "belmopan_ind",  analyte: "HMX",              peak_day: 11.0, amplitude: 2.0, sigma_days: 2.0 },
    // TNT amino-products at Belmopan Industrial Zone
    OutbreakPulse { site_id: "belmopan_ind",  analyte: "TNT_aminoproducts", peak_day: 12.0, amplitude: 1.8, sigma_days: 1.5 },
    // TATP-DHPP sharp spike at Belize City South — short-lived consistent
    // with volatile precursor handling or a small-scale test event.
    OutbreakPulse { site_id: "bcity_south",   analyte: "TATP_DHPP",        peak_day: 11.0, amplitude: 2.6, sigma_days: 1.0 },
    // Perchlorate at Orange Walk — elevated AN/perchlorate consistent with
    // agricultural-grade ANFO or pyrotechnic sourcing.
    OutbreakPulse { site_id: "orange_walk",   analyte: "Perchlorate",       peak_day: 12.0, amplitude: 1.6, sigma_days: 1.5 },
    OutbreakPulse { site_id: "orange_walk",   analyte: "AN_nitrate_excess",  peak_day: 12.0, amplitude: 1.4, sigma_days: 1.5 },

];

fn pulse_contribution(site_id: &str, analyte: &str, day: usize) -> f64 {
    let d = day as f64;
    PULSES.iter()
        .filter(|p| p.site_id == site_id && p.analyte == analyte)
        .map(|p| {
            let delta = d - p.peak_day;
            p.amplitude * (-delta * delta / (2.0 * p.sigma_days * p.sigma_days)).exp()
        })
        .sum()
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the Belize multi-target simulation and return the compiled network model.
///
/// All domain instances are written into `engine`; every significant event is
/// appended to `audit`; anomaly events are processed by `detector`.
///
/// The 18-analyte × 8-site panel runs for 15 days: 7 warm-up days followed by
/// 8 days of simulated outbreak / anomaly events spanning all four analyte
/// categories.
pub fn run_belize(
    engine:   &OntologyEngine,
    audit:    &AuditLog,
    detector: &mut AnomalyDetector,
) -> DomainResult<SewageNetwork> {

    // ── Register monitoring sites ─────────────────────────────────────────────
    for site in SITES {
        DomainFactory::create_site(engine, SiteParams {
            site_id:       Some(site.id.to_string()),
            name:          site.name.to_string(),
            region:        site.region.to_string(),
            catchment_pop: site.catchment_pop,
            lat:           site.lat,
            lon:           site.lon,
        })?;
        audit.record("system", AuditAction::SiteAdded, site.id, site.name);
    }

    // ── Register flow links ───────────────────────────────────────────────────
    let flow_links: &[(&str, &str)] = &[
        ("bcity_south",   "bcity_wwtp"),
        ("bcity_north",   "bcity_wwtp"),
        ("belmopan_core", "bcity_wwtp"),
        ("belmopan_ind",  "bcity_wwtp"),
        ("san_ignacio",   "bcity_wwtp"),
        ("orange_walk",   "bcity_north"),
    ];
    for &(up, down) in flow_links {
        DomainFactory::add_flow_link(engine, up, down)?;
    }

    // ── Build spectral network model ──────────────────────────────────────────
    // The SewageNetwork (hypergraph Laplacian) is shared across all analyte
    // categories — the physical transport equation structure is the same;
    // only k and f differ.
    let catchments: Vec<(String, Vec<String>)> = vec![
        ("Belize City Trunk".into(),  vec!["bcity_north".into(), "bcity_south".into(), "bcity_wwtp".into()]),
        ("Belmopan Zone".into(),      vec!["belmopan_core".into(), "belmopan_ind".into()]),
        ("Northern District".into(),  vec!["orange_walk".into()]),
        ("Western Highlands".into(),  vec!["san_ignacio".into()]),
        ("Southern Coastal".into(),   vec!["dangriga".into()]),
    ];
    let extra: Vec<String> = SITES.iter().map(|s| s.id.to_string()).collect();

    let network = SewageNetwork::build(&catchments, &extra)
        .map_err(|e| ww_domain::DomainError::Conversion(e.to_string()))?;

    // Empirical null quantile of the residual-field score (i.i.d. N(0,1)
    // residuals): the fixed legacy 0.40 is not meaningful for a high-pass
    // statistic whose white-noise mean is well above it.
    let spread_thr = network.null_threshold(SPREAD_NULL_QUANTILE, 20_000, 0x5eed_5eed);
    detector.set_spread_threshold(spread_thr);

    println!(
        "  Network : {} sites  Fiedler λ₂={:.4}  spread threshold (null q{:.0}) = {:.3}  Analyte panel: {} targets\n",
        network.n_sites(),
        network.fiedler_value(),
        SPREAD_NULL_QUANTILE * 100.0,
        spread_thr,
        ANALYTE_CATALOG.len(),
    );

    // Print panel summary
    println!("  Panel breakdown:");
    for cat_label in &[
        ("infectious_pathogen", "Infectious pathogens"),
        ("pharmaceutical_amr",  "Pharmaceutical / AMR"),
        ("illicit_substance",   "Illicit substances"),
        ("industrial_toxicant", "Industrial toxicants"),
        ("explosive_precursor",  "Explosive precursors"),
    ] {
        let count = ANALYTE_CATALOG.iter()
            .filter(|a| a.category.as_str() == cat_label.0)
            .count();
        let names: Vec<_> = ANALYTE_CATALOG.iter()
            .filter(|a| a.category.as_str() == cat_label.0)
            .map(|a| a.name)
            .collect();
        println!("    {:22} ({:2})  {}", cat_label.1, count, names.join(", "));
    }
    println!();

    // ── Simulation loop ───────────────────────────────────────────────────────
    const N_DAYS: usize = 15;

    // Category-level alert counters for the end-of-run summary.
    let mut cat_alerts: HashMap<&'static str, usize> = HashMap::new();

    for day in 0..N_DAYS {
        let mut day_alerts = 0usize;

        // Iterate across the full analyte catalog.
        for (ai, analyte) in ANALYTE_CATALOG.iter().enumerate() {

            // Phase 1: generate concentration field for all sites this (day, analyte)
            let mut concs:      HashMap<String, f64> = HashMap::new();
            let mut sample_map: HashMap<String, String> = HashMap::new();
            let mut signal_map: HashMap<String, String> = HashMap::new();

            for (si, site) in SITES.iter().enumerate() {
                // Deterministic per-(day, site, analyte) seed
                let seed = (day as u64)
                    .wrapping_mul(97)
                    .wrapping_add(si as u64 * 1_000_003)
                    .wrapping_add(ai as u64 * 999_983);

                let noise  = analyte.noise_std * approx_normal(seed);
                let pulse  = pulse_contribution(site.id, analyte.name, day);
                let conc   = (analyte.baseline_log + noise + pulse).max(-2.0);

                let sample_id = DomainFactory::create_sample(engine, SampleParams {
                    sample_id:   None,
                    site_id:     site.id.to_string(),
                    flow_liters: 25_000.0 + 5_000.0 * (si as f64),
                    qc_passed:   true,
                    notes:       format!("day {day} [{cat}]", cat = analyte.category.as_str()),
                })?;

                let signal_id = DomainFactory::create_signal(engine, SignalParams {
                    signal_id:          None,
                    sample_id:          sample_id.clone(),
                    pathogen:           analyte.name.to_string(),
                    target_gene:        analyte.target_marker.to_string(),
                    log10_copies_per_l: conc,
                    method:             analyte.method.to_string(),
                    analyte_category:   analyte.category.as_str().to_string(),
                    decay_rate_k:       analyte.decay_rate_k,
                })?;

                audit.record(
                    "system",
                    AuditAction::SampleIngested,
                    &sample_id,
                    format!(
                        "site={} analyte={} cat={} k={:.3} α={:.2} log10={:.2}",
                        site.id, analyte.name,
                        analyte.category.as_str(),
                        analyte.decay_rate_k,
                        analyte.ewma_alpha(),
                        conc,
                    ),
                );

                concs.insert(site.id.to_string(), conc);
                sample_map.insert(site.id.to_string(), sample_id);
                signal_map.insert(site.id.to_string(), signal_id);
            }

            // Phase 2: spectral score of the *standardised residual field*.
            // The same SewageNetwork (topology) is used for all categories.
            // z-scores are peeked (not absorbed) so the field reflects each
            // site's deviation from its own baseline; raw log10 levels are
            // never fed to Δ (that score is not baseline-invariant).
            let zmap: HashMap<String, f64> = SITES.iter()
                .filter_map(|site| {
                    let c = *concs.get(site.id)?;
                    detector.peek_z(site.id, analyte.name, c)
                        .map(|z| (site.id.to_string(), z))
                })
                .collect();
            let spectral = network.spectral_score_residual(&zmap);

            // Phase 3: statistical anomaly detection per site
            for site in SITES {
                let conc      = *concs.get(site.id).unwrap();
                let signal_id = signal_map.get(site.id).unwrap();

                if let Some(event) = detector.observe_analyte(
                    site.id,
                    analyte.name,
                    conc,
                    spectral,
                    analyte.ewma_alpha(),
                    analyte.z_threshold,
                ) {
                    let alert_id = DomainFactory::create_alert(engine, AlertParams {
                        alert_id:         None,
                        site_id:          site.id.to_string(),
                        pathogen:         analyte.name.to_string(),
                        analyte_category: analyte.category.as_str().to_string(),
                        severity:         event.severity.as_str().to_string(),
                        signal_id:        signal_id.clone(),
                        z_score:          event.z_score,
                        spectral_score:   event.spectral_score,
                    })?;

                    audit.record(
                        "system",
                        AuditAction::AlertRaised,
                        &alert_id,
                        format!(
                            "site={} analyte={} cat={} z={:.2} spectral={:.3} sev={}",
                            site.id, analyte.name,
                            analyte.category.as_str(),
                            event.z_score,
                            event.spectral_score,
                            event.severity.as_str(),
                        ),
                    );

                    println!(
                        "  {} [Day {:2}] {:8}  {}  {}  @  {}\n           \
                         z={:.2}  spectral={:.3}  conc={:.2}  k={:.3}  α={:.2}  n={}\n           \
                         alert_id={}",
                        event.severity.as_str().chars().next().unwrap_or('?'),
                        day,
                        event.severity.as_str(),
                        analyte.category.tag(),
                        analyte.name,
                        site.name,
                        event.z_score,
                        event.spectral_score,
                        conc,
                        analyte.decay_rate_k,
                        event.alpha,
                        event.n_obs,
                        alert_id,
                    );

                    *cat_alerts.entry(analyte.category.as_str()).or_insert(0) += 1;
                    day_alerts += 1;
                }
            }
        }

        let n_signals = SITES.len() * ANALYTE_CATALOG.len();
        let arrow = if day_alerts > 0 {
            format!(" ← {} alert(s)", day_alerts)
        } else if day < 7 {
            "   [warm-up]".into()
        } else {
            String::new()
        };
        println!("  Day {:2} | {:4} signals processed{}", day, n_signals, arrow);
    }

    // ── Per-category summary ──────────────────────────────────────────────────
    println!("\n  Category alert summary:");
    for (cat, label) in &[
        ("infectious_pathogen", "Infectious pathogens"),
        ("pharmaceutical_amr",  "Pharmaceutical / AMR"),
        ("illicit_substance",   "Illicit substances"),
        ("industrial_toxicant", "Industrial toxicants"),
        ("explosive_precursor",  "Explosive precursors"),
    ] {
        let n = cat_alerts.get(cat).copied().unwrap_or(0);
        println!("    {:22}  {:3} alert(s)", label, n);
    }
    println!();

    Ok(network)
}
