//! Shared helpers for `ww_eval`'s two evaluation harnesses:
//! `real_data` (public wastewater datasets) and `monte_carlo` (randomized
//! seeded simulation over the exact `ww_detection` engine used in production).

use ww_detection::SewageNetwork;

/// splitmix64 — fast, well-mixed PRNG core. Used both to seed independent
/// Monte-Carlo runs and to draw per-(run, day, site) noise deterministically
/// from a single u64 seed (full reproducibility per run).
#[derive(Clone)]
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// Uniform f64 in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Approximate standard normal via 12-uniform sum (Irwin-Hall / CLT),
    /// matching the technique used in `ww_runner::scenario` and
    /// `SewageNetwork::null_threshold` for consistency with the shipped code.
    pub fn next_normal(&mut self) -> f64 {
        let mut x = 0.0f64;
        for _ in 0..12 {
            x += self.next_f64();
        }
        x - 6.0
    }

    /// Uniform f64 in [lo, hi).
    pub fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }

    /// Uniform usize in [0, n).
    pub fn index(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// The 8-site Belize sewage network topology from `ww_runner::scenario`,
/// reconstructed here so Monte-Carlo runs exercise the identical
/// `SewageNetwork` (hypergraph Laplacian) code path as the shipped
/// simulation, just with randomized pulses instead of the fixed scenario.
pub const MC_SITES: &[&str] = &[
    "bcity_north", "bcity_south", "bcity_wwtp",
    "belmopan_core", "belmopan_ind",
    "orange_walk", "san_ignacio", "dangriga",
];

pub fn belize_network() -> SewageNetwork {
    let s = |x: &str| x.to_string();
    let catchments: Vec<(String, Vec<String>)> = vec![
        (s("trunk"), vec![s("bcity_north"), s("bcity_south"), s("bcity_wwtp")]),
        (s("belmopan"), vec![s("belmopan_core"), s("belmopan_ind")]),
        (s("ow"), vec![s("orange_walk")]),
        (s("si"), vec![s("san_ignacio")]),
        (s("dg"), vec![s("dangriga")]),
    ];
    let extra: Vec<String> = MC_SITES.iter().map(|x| s(x)).collect();
    SewageNetwork::build(&catchments, &extra).expect("static topology is valid")
}

/// Gaussian pulse contribution at `day` (fractional days allowed).
pub fn pulse_contribution(day: f64, peak_day: f64, amplitude: f64, sigma_days: f64) -> f64 {
    let delta = day - peak_day;
    amplitude * (-delta * delta / (2.0 * sigma_days * sigma_days)).exp()
}

/// SARS-CoV-2 profile constants, copied from `ww_domain::analyte::ANALYTE_CATALOG`
/// (kept in sync manually; `ww_domain` is not pulled in here to keep `ww_eval`
/// free of the ontology-engine dependency chain).
pub mod sars_cov2 {
    pub const BASELINE_LOG: f64 = 3.8;
    pub const NOISE_STD: f64 = 0.22;
    pub const DECAY_RATE_K: f64 = 0.50;
    pub const Z_THRESHOLD: f64 = 2.5;

    pub fn ewma_alpha() -> f64 {
        (1.0_f64 - (-DECAY_RATE_K).exp()).clamp(0.10, 0.40)
    }
}
