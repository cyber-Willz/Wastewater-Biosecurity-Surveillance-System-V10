//! Threshold-based anomaly detector combining statistical and spectral scores.
//!
//! ## Severity mapping
//!
//! | z-score  | spectral score | Severity                          |
//! |----------|----------------|-----------------------------------|
//! | ≥ 5.0    | any            | CRITICAL                          |
//! | ≥ 3.5    | any            | RED                               |
//! | ≥ 2.5    | any            | AMBER                             |
//! | ≥ 2.5    | > 0.40         | RED   (network-spread upgrade)    |
//! | ≥ 3.5    | > 0.40         | CRITICAL (network-spread upgrade) |
//! | < thresh | any            | (no alert)                        |
//!
//! ## v0.2 — Per-analyte thresholds and α
//!
//! [`AnomalyDetector::observe_analyte`] accepts explicit α and z_threshold
//! values derived from an [`AnalyteProfile`].  The original
//! [`AnomalyDetector::observe`] is kept for backward compatibility but uses
//! the default 2.5 σ threshold and α = 0.25 (infectious pathogen default).

use crate::baseline::EwmaBaseline;
use crate::site_weight::SiteWeightRegistry;
use crate::spectral_mode::SpectralMode;

// Legacy defaults — used only by the backward-compat `observe` method.
const DEFAULT_Z_THRESHOLD: f64 = 2.5;
const DEFAULT_ALPHA:       f64 = 0.25;

// Spectral score above which network-spread upgrade applies.
const NETWORK_SPREAD_THRESHOLD: f64 = 0.40;

/// Mirrors [`ww_domain::query::Severity`] without importing the domain crate.
/// Converted by the runner before writing to the ontology.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DetectionSeverity {
    Green,
    Amber,
    Red,
    Critical,
}

impl DetectionSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            DetectionSeverity::Green    => "GREEN",
            DetectionSeverity::Amber    => "AMBER",
            DetectionSeverity::Red      => "RED",
            DetectionSeverity::Critical => "CRITICAL",
        }
    }

    /// Derive severity from a statistical z-score, optionally upgraded by the
    /// spectral network score.
    ///
    /// `z_threshold`: the minimum z-score to raise *any* alert.  Pathogens
    /// use 2.5; industrial toxicants use 3.5 to reduce false positives from
    /// natural background variation.
    pub fn from_scores(z: f64, spectral: f64, z_threshold: f64) -> Self {
        Self::from_scores_with(z, spectral, z_threshold, NETWORK_SPREAD_THRESHOLD)
    }

    /// As [`Self::from_scores`] with an explicit spectral upgrade threshold
    /// (e.g. an empirical null quantile from
    /// [`crate::spectral::SewageNetwork::null_threshold`]).
    ///
    /// Properties (Thm 5.1): an alert exists iff `z ≥ z_threshold`; `spectral`
    /// only moves an existing alert up by at most one tier.  When
    /// `z_threshold ≥ 3.5` the AMBER tier is unreachable.
    pub fn from_scores_with(z: f64, spectral: f64, z_threshold: f64, spread_threshold: f64) -> Self {
        // Base tier from statistical z-score
        let base = if z >= 5.0 {
            DetectionSeverity::Critical
        } else if z >= 3.5 {
            DetectionSeverity::Red
        } else if z >= z_threshold {
            DetectionSeverity::Amber
        } else {
            DetectionSeverity::Green
        };

        // Network-spread upgrade: isolated spike → only statistical;
        // widespread anomalous gradient pattern → one tier upgrade.
        if spectral > spread_threshold {
            match base {
                DetectionSeverity::Amber => DetectionSeverity::Red,
                DetectionSeverity::Red   => DetectionSeverity::Critical,
                other                    => other,
            }
        } else {
            base
        }
    }
}

/// An anomaly event ready to be lifted into a [`ww_domain`] alert.
#[derive(Debug, Clone)]
pub struct AnomalyEvent {
    pub site_id:        String,
    pub analyte:        String,
    /// Observed log₁₀(copies/L or µg/L).
    pub log10_copies:   f64,
    /// EWMA baseline at time of detection.
    pub ewma:           f64,
    /// Statistical z-score.
    pub z_score:        f64,
    /// Composite spectral network anomaly score in `[0, 1]`.
    pub spectral_score: f64,
    pub severity:       DetectionSeverity,
    /// Number of observations in the baseline at time of detection.
    pub n_obs:          usize,
    /// EWMA α active for this analyte (for diagnostics).
    pub alpha:          f64,
}

/// Stateful anomaly detector.
///
/// Maintains one [`EwmaBaseline`] across all `(site, analyte)` pairs.
/// Call [`AnomalyDetector::observe_analyte`] once per sample-signal; the
/// detector handles baseline warm-up internally.
///
/// ## v0.6 additions
///
/// * **`site_weights`** — per-site reliability weights and minimum-sampling-
///   density gates.  A site with `weight < 1` has its z-threshold raised to
///   `z_threshold / √weight`.  A round with `n_samples < min_samples` for
///   that site produces no alert (the baseline is still updated).
///
/// * **`spectral_mode`** — controls whether the network-spread score is used
///   for severity upgrades.  Defaults to `SpectralMode::Gated { min_real_catchments: 2 }`
///   so the term is disabled until real sewer connectivity data is loaded.
///   Pass `n_real_catchments` (the number of catchments with ≥ 2 members) to
///   `observe_analyte_weighted` to allow the gate to evaluate correctly.
pub struct AnomalyDetector {
    baseline:         EwmaBaseline,
    spread_threshold: f64,
    /// Per-site reliability weights and minimum-sample-density gates.
    pub site_weights: SiteWeightRegistry,
    /// Controls whether the spectral score influences severity upgrades.
    pub spectral_mode: SpectralMode,
}

impl AnomalyDetector {
    pub fn new() -> Self {
        Self {
            baseline:         EwmaBaseline::new(),
            spread_threshold: NETWORK_SPREAD_THRESHOLD,
            site_weights:     SiteWeightRegistry::new(),
            spectral_mode:    SpectralMode::DEFAULT,
        }
    }

    /// Ingest one signal measurement with analyte-specific kinetic parameters.
    ///
    /// * `alpha`       — EWMA smoothing factor from `AnalyteProfile::ewma_alpha()`.
    /// * `z_threshold` — minimum z-score to raise an alert, from `AnalyteProfile::z_threshold`.
    /// * `spectral`    — pre-computed `SewageNetwork::spectral_score` for the current round.
    ///
    /// Returns `Some(event)` if the measurement crosses the threshold,
    /// `None` otherwise (including during warm-up).
    ///
    /// Delegates to [`Self::observe_analyte_weighted`] with `n_samples = 1`
    /// (passes the density gate for all registered sites) and
    /// `n_real_catchments = usize::MAX` (always passes the spectral mode gate).
    /// Use [`Self::observe_analyte_weighted`] when round-level sample counts
    /// and real topology counts are available.
    pub fn observe_analyte(
        &mut self,
        site_id:      &str,
        analyte:      &str,
        log10_copies: f64,
        spectral:     f64,
        alpha:        f64,
        z_threshold:  f64,
    ) -> Option<AnomalyEvent> {
        self.observe_analyte_weighted(
            site_id, analyte, log10_copies, spectral, alpha, z_threshold,
            1,           // n_samples: legacy callers pass no sample count
            usize::MAX,  // n_real_catchments: no spectral gate for legacy callers
        )
    }

    /// Backward-compatible observe with default α and z-threshold (2.5 σ).
    ///
    /// Prefer [`observe_analyte`] for new call sites; this is retained so
    /// existing tests continue to compile without modification.
    pub fn observe(
        &mut self,
        site_id:        &str,
        pathogen:       &str,
        log10_copies:   f64,
        spectral_score: f64,
    ) -> Option<AnomalyEvent> {
        self.observe_analyte(
            site_id, pathogen, log10_copies, spectral_score,
            DEFAULT_ALPHA, DEFAULT_Z_THRESHOLD,
        )
    }

    /// Ingest one signal measurement, applying per-site reliability weighting
    /// and the minimum-sampling-density gate.
    ///
    /// * `n_samples` — number of physical grab/composite samples that went into
    ///   `log10_copies`.  Compared against `SiteWeight::min_samples`; if fewer,
    ///   the baseline is updated but `None` is returned (no alert).
    /// * `n_real_catchments` — number of catchments with ≥ 2 members in the
    ///   current network topology.  Used by `SpectralMode::Gated` to decide
    ///   whether the spectral score should influence severity.
    ///
    /// When neither site-weighting nor spectral gating is needed, prefer
    /// [`observe_analyte`] — it calls this with `n_samples = 1` and
    /// `n_real_catchments = usize::MAX` (always passes both gates).
    pub fn observe_analyte_weighted(
        &mut self,
        site_id:            &str,
        analyte:            &str,
        log10_copies:       f64,
        spectral:           f64,
        alpha:              f64,
        z_threshold:        f64,
        n_samples:          u32,
        n_real_catchments:  usize,
    ) -> Option<AnomalyEvent> {
        let weight   = self.site_weights.get(site_id);
        let eff_z    = weight.adjusted_threshold(z_threshold);

        // Winsorised baseline update always happens — gate only blocks the alert.
        let update = self.baseline.ingest_robust(site_id, analyte, log10_copies, alpha, eff_z);

        // Sampling-density gate
        if !weight.passes_gate(n_samples) {
            return None;
        }

        if !update.is_warm || update.z_score < eff_z {
            return None;
        }

        // Spectral mode gate: zero out the score if not enough real catchments.
        let eff_spectral = self.spectral_mode.effective_score(spectral, n_real_catchments);

        let severity = DetectionSeverity::from_scores_with(
            update.z_score, eff_spectral, eff_z, self.spread_threshold,
        );
        Some(AnomalyEvent {
            site_id:        site_id.to_string(),
            analyte:        analyte.to_string(),
            log10_copies,
            ewma:           update.ewma,
            z_score:        update.z_score,
            spectral_score: spectral,  // store raw score for diagnostics
            severity,
            n_obs:          update.n,
            alpha:          update.alpha,
        })
    }

    /// Set the spectral-score threshold above which an alert is upgraded one
    /// tier (default 0.40, the legacy value).
    pub fn with_spread_threshold(mut self, t: f64) -> Self {
        self.spread_threshold = t;
        self
    }

    /// In-place variant of [`Self::with_spread_threshold`].
    pub fn set_spread_threshold(&mut self, t: f64) {
        self.spread_threshold = t;
    }

    /// Set the spectral mode (builder pattern).
    pub fn with_spectral_mode(mut self, mode: SpectralMode) -> Self {
        self.spectral_mode = mode;
        self
    }

    /// Set the site-weight registry (builder pattern).
    pub fn with_site_weights(mut self, weights: SiteWeightRegistry) -> Self {
        self.site_weights = weights;
        self
    }

    /// z-score of a prospective observation against the current baseline,
    /// without absorbing it (used to build the residual field for the
    /// network score before the round's observations are ingested).
    pub fn peek_z(&self, site_id: &str, analyte: &str, log10_copies: f64) -> Option<f64> {
        self.baseline.peek_z(site_id, analyte, log10_copies)
    }

    pub fn baseline_mut(&mut self) -> &mut EwmaBaseline {
        &mut self.baseline
    }
}

impl Default for AnomalyDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_map_matches_documented_table() {
        use DetectionSeverity::*;
        assert_eq!(DetectionSeverity::from_scores(5.1, 0.0, 2.5), Critical);
        assert_eq!(DetectionSeverity::from_scores(3.6, 0.0, 2.5), Red);
        assert_eq!(DetectionSeverity::from_scores(3.6, 0.5, 2.5), Critical);
        assert_eq!(DetectionSeverity::from_scores(2.6, 0.0, 2.5), Amber);
        assert_eq!(DetectionSeverity::from_scores(2.6, 0.5, 2.5), Red);
        assert_eq!(DetectionSeverity::from_scores(2.0, 0.9, 2.5), Green);
    }

    /// Thm 5.1(5): with z_thr = 3.5 (toxicants) the AMBER tier is unreachable.
    #[test]
    fn toxicant_threshold_has_no_amber() {
        for z in [3.5, 3.6, 4.0, 4.9] {
            assert_ne!(DetectionSeverity::from_scores(z, 0.0, 3.5), DetectionSeverity::Amber);
        }
    }

    #[test]
    fn spectral_score_never_creates_an_alert() {
        let mut d = AnomalyDetector::new();
        for i in 0..12 {
            assert!(d.observe_analyte("s", "a", 3.0 + 0.01 * (i % 2) as f64, 0.99, 0.3, 2.5).is_none());
        }
    }
}
