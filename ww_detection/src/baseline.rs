//! Per-`(site, analyte)` EWMA baseline with Welford online variance.
//!
//! Wastewater concentrations are log₁₀-transformed before ingestion, which
//! converts the roughly log-normal distribution into something closer to
//! Gaussian.  The EWMA tracks the centre of that distribution; Welford's
//! algorithm tracks its spread without storing raw history.
//!
//! ## v0.4 — corrections (see `docs/ww_biosec_theory.md`)
//!
//! * **Warm-up off-by-one fixed.**  Previously `State::new(first)` already
//!   counted the first sample and `ingest` then called `update(first)` again,
//!   so `n = 2` after one observation.  The state is now created *without* a
//!   second update: `n` is the true number of observations absorbed.
//! * **Self-masking fixed (winsorised update).**  Welford's variance includes
//!   every past observation, so an outbreak inflated the spread it was scored
//!   against and hid itself (Prop. 3.4).  [`EwmaBaseline::ingest_robust`]
//!   winsorises the value absorbed by the EWMA to
//!   `ewma ± ½·clip_z · s` and **excludes** observations beyond
//!   `ewma ± 1.5·clip_z · s` from the Welford variance (the z-score itself
//!   is still computed from the raw observation).
//! * `log10_copies` is **not** flow-normalised here (the earlier comment said
//!   it was); any flow/population normalisation must happen upstream.
//!
//! ## v0.2 — Per-analyte smoothing factor α
//!
//! The smoothing factor α is no longer a workspace-wide constant.  Each
//! `(site, analyte)` state stores its own α, derived from the analyte's
//! first-order decay rate k via `α = clamp(1 − e^{−k}, 0.10, 0.40)`.
//!
//! This correctly models the information lifetime of different analyte classes:
//!
//! | Category            | k (d⁻¹) | α     | Effective half-life |
//! |---------------------|----------|-------|---------------------|
//! | Infectious pathogen | 0.50     | 0.39  | ~1.4 obs            |
//! | Illicit substance   | 0.25     | 0.22  | ~2.7 obs            |
//! | Pharmaceutical      | 0.10     | 0.10  | ~6.6 obs (floor)    |
//! | PFAS (industrial)   | 0.005    | 0.10  | ~6.6 obs (floor)    |
//!
//! ## Why two means?
//!
//! * `ewma` adapts quickly to true seasonal drift (controlled by per-key α).
//! * `welford_mean` / `welford_m2` track long-run variance independent of
//!   recent level shifts.  The z-score uses `ewma` as the centre but the
//!   Welford variance as the spread, so the detector is sensitive to sudden
//!   spikes while remaining robust against slow baseline drift.

use std::collections::HashMap;

/// Minimum observations before the baseline is trusted for anomaly scoring.
const MIN_OBS: usize = 7;

/// Observations after which the absorbed value is winsorised (earlier than
/// `MIN_OBS`, so a slow outbreak that starts during warm-up is not absorbed
/// at full weight into the variance).
const WINSOR_MIN_OBS: usize = 5;

/// The EWMA sees deviations winsorised to `EWMA_CLIP_FRAC · clip_z · s`, so a
/// ramping outbreak drags the baseline along at a bounded, slow rate (a
/// permanent level shift is still absorbed eventually).
const EWMA_CLIP_FRAC: f64 = 0.5;

/// Observations beyond `VAR_EXCLUDE_MULT · clip_z · s` are excluded from the
/// Welford variance.  1.0 detects slightly more but raises the null
/// false-alarm count ~45 %; 1.5 keeps it at or below the legacy detector
/// (grid search documented in `docs/ww_biosec_theory.md` App. C).
const VAR_EXCLUDE_MULT: f64 = 1.5;

/// ## v0.5 — bounded-memory variance
///
/// Plain Welford variance is a lifetime average: once several waves' worth
/// of moderate, individually-sub-threshold elevation have been absorbed, the
/// z-score denominator permanently inflates and a later wave of the *same
/// relative size* as the first no longer reaches the same z-score (evaluated
/// against 18 months of real Scottish national wastewater surveillance data:
/// σ tripled over the first four months and then never came back down,
/// silently suppressing detection of the largest wave in the series). Exact
/// Welford is kept for the first `WELFORD_BOOTSTRAP` observations (a stable
/// early estimate; matches the pre-v0.5 behaviour that existing tests
/// exercise), then variance switches to an exponentially-weighted recurrence
/// with its own, slower decay rate `alpha / VAR_MEMORY_MULT` — slower than
/// the mean's α so a single anomalous period still can't inflate its own
/// spread (that job is still done by the winsorised exclusion below), but
/// bounded, so old waves eventually stop counting as "normal."
const WELFORD_BOOTSTRAP: usize = 20;

/// The variance's effective memory is this many times longer than the
/// EWMA mean's (e.g. α=0.39 ⇒ var half-life ≈ 14 observations vs. the
/// mean's ≈ 1.4). Chosen relative to the per-analyte α (rather than a fixed
/// absolute window) so it scales sensibly across analyte classes /
/// sampling cadences instead of hard-coding e.g. "52 weeks."
const VAR_MEMORY_MULT: f64 = 8.0;

// ── Internal state per (site, analyte) ───────────────────────────────────────

#[derive(Debug, Clone)]
struct State {
    /// Per-analyte EWMA smoothing factor, derived from decay_rate_k.
    alpha:        f64,
    ewma:         f64,
    welford_mean: f64,
    welford_m2:   f64,
    /// Sample variance derived from M2 / (n − 1); cached after each update.
    variance:     f64,
    /// Observations seen (drives warm-up).
    n:            usize,
    /// Observations absorbed into the Welford variance (`≤ n`; flagged
    /// outliers are excluded so an outbreak cannot inflate its own spread).
    wn:           usize,
}

impl State {
    fn new(first: f64, alpha: f64) -> Self {
        Self {
            alpha,
            ewma:         first,
            welford_mean: first,
            welford_m2:   0.0,
            variance:     1e-6,   // small non-zero prior avoids /0 on first z-score
            n:            1,
            wn:           1,
        }
    }

    /// Absorb one observation.  `ewma_obs` feeds the EWMA; `var_obs`, when
    /// `Some`, also feeds the variance estimator (`None` = excluded outlier).
    fn update(&mut self, ewma_obs: f64, var_obs: Option<f64>) {
        self.n += 1;

        if let Some(obs) = var_obs {
            self.wn += 1;
            if self.wn <= WELFORD_BOOTSTRAP {
                // Exact Welford while the estimate is still young.
                let delta  = obs - self.welford_mean;
                self.welford_mean += delta / self.wn as f64;
                let delta2 = obs - self.welford_mean;
                self.welford_m2   += delta * delta2;
                self.variance = if self.wn > 1 {
                    (self.welford_m2 / (self.wn - 1) as f64).max(1e-6)
                } else {
                    1e-6
                };
            } else {
                // Bounded-memory exponentially-weighted variance (West 1979
                // recurrence) from here on, so variance from waves many
                // months ago eventually stops counting as "normal."
                let var_alpha = (self.alpha / VAR_MEMORY_MULT).clamp(1e-4, 1.0);
                let diff = obs - self.welford_mean;
                let incr = var_alpha * diff;
                self.welford_mean += incr;
                self.variance = ((1.0 - var_alpha) * (self.variance + diff * incr)).max(1e-6);
            }
        }

        self.ewma = self.alpha * ewma_obs + (1.0 - self.alpha) * self.ewma;
    }

    fn z_score(&self, obs: f64) -> f64 {
        (obs - self.ewma) / self.variance.sqrt().max(1e-4)
    }

    fn is_warm(&self) -> bool {
        self.n >= MIN_OBS
    }

    /// Standard deviation used as the z-score denominator.
    fn sd(&self) -> f64 {
        self.variance.sqrt().max(1e-4)
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Thread-local EWMA baseline registry.
///
/// Each entry is keyed by `(site_id, analyte_name)` and holds the full running
/// state (including its per-analyte α) for that pair.  The registry is not
/// `Send`; wrap in `Arc<Mutex<…>>` if sharing across threads.
#[derive(Debug, Default)]
pub struct EwmaBaseline {
    states: HashMap<(String, String), State>,
}

/// Summary returned by [`EwmaBaseline::ingest`].
#[derive(Debug, Clone)]
pub struct BaselineUpdate {
    /// Current EWMA of log₁₀ concentration.
    pub ewma:    f64,
    /// z-score of the current observation relative to the EWMA + variance.
    /// `0.0` when the baseline is still warming up.
    pub z_score: f64,
    /// Number of observations seen for this `(site, analyte)` pair.
    pub n:       usize,
    /// `true` when there are enough observations for reliable z-scores.
    pub is_warm: bool,
    /// The α value active for this key (for diagnostics / export).
    pub alpha:   f64,
}

impl EwmaBaseline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest one observation and return the updated baseline summary.
    ///
    /// `log10_copies`: log₁₀(copies / L) (not flow-normalised by this crate).
    /// `alpha`: the EWMA smoothing factor for this analyte, typically
    /// derived from [`AnalyteProfile::ewma_alpha()`].  On the first call
    /// for a new key the provided α is stored and used for all future
    /// updates to that key; subsequent calls' α values are ignored.
    pub fn ingest(
        &mut self,
        site:         &str,
        analyte:      &str,
        log10_copies: f64,
        alpha:        f64,
    ) -> BaselineUpdate {
        self.ingest_robust(site, analyte, log10_copies, alpha, f64::INFINITY)
    }

    /// Like [`Self::ingest`] but the value absorbed by the EWMA is winsorised to `ewma ± ½·clip_z · s` (variance excludes beyond `1.5·clip_z · s`) once at least
    /// `WINSOR_MIN_OBS` (5) observations exist.  The returned z-score is always computed from the raw
    /// observation.  `clip_z = f64::INFINITY` reproduces plain `ingest`.
    pub fn ingest_robust(
        &mut self,
        site:         &str,
        analyte:      &str,
        log10_copies: f64,
        alpha:        f64,
        clip_z:       f64,
    ) -> BaselineUpdate {
        let key = (site.to_string(), analyte.to_string());

        // First observation for this key: create the state (n = 1, the value
        // is already absorbed) and return — no second update.
        if !self.states.contains_key(&key) {
            let st = State::new(log10_copies, alpha);
            let out = BaselineUpdate { ewma: st.ewma, z_score: 0.0, n: st.n, is_warm: false, alpha: st.alpha };
            self.states.insert(key, st);
            return out;
        }

        let state   = self.states.get_mut(&key).expect("checked above");
        let is_warm = state.is_warm();
        let z       = if is_warm { state.z_score(log10_copies) } else { 0.0 };
        let alpha   = state.alpha;

        // Robust absorption (only once >= WINSOR_MIN_OBS observations exist):
        //  * the EWMA sees the observation winsorised to ewma ± clip_z·s;
        //  * Welford's variance *excludes* observations beyond that band, so
        //    an outbreak cannot inflate the spread it is scored against.
        let mut ewma_obs = log10_copies;
        let mut var_obs  = Some(log10_copies);
        if state.n >= WINSOR_MIN_OBS && clip_z.is_finite() {
            let sd   = state.sd();
            let half = VAR_EXCLUDE_MULT * clip_z * sd;
            let dev  = log10_copies - state.ewma;
            let ehalf = EWMA_CLIP_FRAC * clip_z * sd;
            if dev.abs() > ehalf {
                ewma_obs = state.ewma + dev.clamp(-ehalf, ehalf);
            }
            if dev.abs() > half {
                var_obs = None;
            }
        }
        state.update(ewma_obs, var_obs);

        BaselineUpdate { ewma: state.ewma, z_score: z, n: state.n, is_warm, alpha }
    }

    /// z-score of `log10_copies` against the current baseline **without**
    /// absorbing it.  `None` while the key is unseen or still warming up.
    pub fn peek_z(&self, site: &str, analyte: &str, log10_copies: f64) -> Option<f64> {
        let st = self.states.get(&(site.to_string(), analyte.to_string()))?;
        if st.is_warm() { Some(st.z_score(log10_copies)) } else { None }
    }

    pub fn ewma(&self, site: &str, analyte: &str) -> Option<f64> {
        self.states.get(&(site.to_string(), analyte.to_string())).map(|s| s.ewma)
    }

    pub fn n_obs(&self, site: &str, analyte: &str) -> usize {
        self.states
            .get(&(site.to_string(), analyte.to_string()))
            .map(|s| s.n)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_is_counted_once() {
        let mut b = EwmaBaseline::new();
        let u = b.ingest("s", "a", 3.0, 0.3);
        assert_eq!(u.n, 1);
        assert_eq!(b.n_obs("s", "a"), 1);
        b.ingest("s", "a", 3.0, 0.3);
        assert_eq!(b.n_obs("s", "a"), 2);
    }

    #[test]
    fn warm_after_seven_prior_observations() {
        let mut b = EwmaBaseline::new();
        for i in 0..7 {
            let u = b.ingest("s", "a", 3.0 + 0.01 * i as f64, 0.3);
            assert!(!u.is_warm, "obs {i} must not be warm");
        }
        // 8th observation is the first to be scored against 7 prior ones.
        assert!(b.ingest("s", "a", 3.0, 0.3).is_warm);
    }

    #[test]
    fn peek_z_does_not_mutate() {
        let mut b = EwmaBaseline::new();
        for i in 0..10 {
            b.ingest("s", "a", 3.0 + 0.05 * ((i % 3) as f64), 0.3);
        }
        let n = b.n_obs("s", "a");
        let z1 = b.peek_z("s", "a", 4.0).unwrap();
        let z2 = b.peek_z("s", "a", 4.0).unwrap();
        assert_eq!(z1, z2);
        assert_eq!(n, b.n_obs("s", "a"));
        assert!(b.peek_z("s", "unseen", 4.0).is_none());
    }

    /// A slow outbreak ramp must not inflate the spread it is scored against
    /// (Prop 3.4): the robust ingest keeps s small and z large.
    #[test]
    fn robust_ingest_resists_variance_contamination() {
        let noise = [0.05, -0.04, 0.03, -0.05, 0.04, -0.03, 0.02, -0.02];
        let mut plain  = EwmaBaseline::new();
        let mut robust = EwmaBaseline::new();
        for (i, e) in noise.iter().enumerate() {
            plain.ingest("s", "a", 3.8 + e, 0.39);
            robust.ingest_robust("s", "a", 3.8 + e, 0.39, 2.5);
            let _ = i;
        }
        let (mut zp, mut zr) = (0.0f64, 0.0f64);
        for step in 1..=5 {
            let x = 3.8 + 0.7 * step as f64;
            zp = zp.max(plain.ingest("s", "a", x, 0.39).z_score);
            zr = zr.max(robust.ingest_robust("s", "a", x, 0.39, 2.5).z_score);
        }
        assert!(zr > zp, "robust max z {zr} should exceed plain {zp}");
        assert!(zr > 2.5, "ramp must be detected by the robust baseline");
    }

    #[test]
    fn infinite_clip_equals_plain_ingest() {
        let mut a = EwmaBaseline::new();
        let mut b = EwmaBaseline::new();
        for i in 0..15 {
            let x = 2.0 + ((i * 7) % 5) as f64 * 0.1 + if i > 10 { 1.5 } else { 0.0 };
            let ua = a.ingest("s", "k", x, 0.25);
            let ub = b.ingest_robust("s", "k", x, 0.25, f64::INFINITY);
            assert_eq!(ua.z_score, ub.z_score);
            assert_eq!(ua.ewma, ub.ewma);
        }
    }

    /// v0.5 regression: variance must stop growing without bound once several
    /// waves have passed, so a later wave of the same relative size as the
    /// first is scored against a comparable (not permanently inflated) σ.
    #[test]
    fn variance_is_bounded_across_repeated_waves() {
        let mut b = EwmaBaseline::new();
        let alpha = 0.39;
        let clip = 2.5;

        // Quiet warm-up.
        for w in 0..12 {
            b.ingest_robust("s", "a", 3.8 + 0.05 * ((w % 3) as f64 - 1.0), alpha, clip);
        }

        // A realistic wave: peak deviation ~0.85 log10 units above baseline,
        // matching the real Shieldhall Aug-2020 spike this fix was found
        // against (conc 4.07 vs. ewma 3.24), not an extreme synthetic one.
        let run_wave = |b: &mut EwmaBaseline| {
            for w in 0..8 {
                let dev = 0.9 * (-((w as f64 - 4.0).powi(2)) / (2.0 * 1.5f64.powi(2))).exp();
                b.ingest_robust("s", "a", 3.8 + dev, alpha, clip);
            }
            for _ in 0..8 {
                b.ingest_robust("s", "a", 3.8, alpha, clip);
            }
        };

        run_wave(&mut b);
        let probe_z1 = b.ingest_robust("s", "a", 3.8 + 0.5, alpha, clip).z_score;
        let sd1 = 0.5 / probe_z1;

        for _ in 0..8 {
            run_wave(&mut b);
        }
        let probe_z2 = b.ingest_robust("s", "a", 3.8 + 0.5, alpha, clip).z_score;
        let sd2 = 0.5 / probe_z2;

        // Bounded-memory variance should plateau, not keep drifting further
        // from its post-first-wave level as more waves of the same size
        // accumulate.
        assert!(
            sd2 < sd1 * 2.5,
            "sd grew unboundedly across repeated waves: sd1={sd1:.4} sd2={sd2:.4}"
        );
    }
}
