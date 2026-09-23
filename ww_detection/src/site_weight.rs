//! Per-site noise/reliability weighting and minimum-sampling-density gate.
//!
//! ## Motivation
//!
//! Some monitoring sites have inherently low data quality — too few samples per
//! round, poorly maintained equipment, or high background variance.  Without a
//! reliability gate they produce frequent low-confidence alerts that crowd the
//! alert queue and erode operator trust.
//!
//! Two mechanisms work together:
//!
//! * **Minimum-sampling-density gate** — a site-round with `n_samples < min_samples`
//!   is silently excluded from alert generation (the observation still updates
//!   the baseline so history accumulates, but the detector returns `None`).
//!
//! * **Reliability weight `w ∈ (0, 1]`** — a per-site scalar captured from the
//!   analyst configuration that scales the effective z-threshold upward for
//!   noisier sites.  The adjusted threshold is
//!
//!   ```text
//!   z_eff = z_threshold / sqrt(w)
//!   ```
//!
//!   so a site with `w = 0.25` needs `2× z_threshold` to raise any alert.
//!   The formula is derived from the standard error of a weighted mean: if a
//!   site's measurements are noisier by factor `1/√w` relative to a well-
//!   calibrated reference site, its z-scores should be held to a correspondingly
//!   higher bar.
//!
//! ## Default behaviour
//!
//! If no weight is registered for a site, `w = 1.0` (full weight, no
//! adjustment) and `min_samples = 1` (any single sample passes the gate) —
//! exactly the pre-v0.6 behaviour.

use std::collections::HashMap;

/// Reliability weight for one monitoring site.
#[derive(Debug, Clone)]
pub struct SiteWeight {
    /// Reliability scalar `w ∈ (0, 1]`.  Values ≤ 0 are clamped to 1e-3.
    pub weight: f64,
    /// Minimum number of samples in a round for the site to pass the gate.
    /// Rounds where `n_samples < min_samples` do not produce alerts.
    pub min_samples: u32,
}

impl SiteWeight {
    pub fn new(weight: f64, min_samples: u32) -> Self {
        Self {
            weight: weight.clamp(1e-3, 1.0),
            min_samples: min_samples.max(1),
        }
    }

    /// Full-weight, single-sample gate — the default for unregistered sites.
    pub fn default_weight() -> Self {
        Self { weight: 1.0, min_samples: 1 }
    }

    /// Adjusted z-threshold for this site.
    ///
    /// `z_eff = z_threshold / √w`
    ///
    /// A site with `w < 1` (lower reliability) requires a higher z-score to
    /// generate an alert.  At `w = 1` the returned value equals `z_threshold`
    /// exactly.
    pub fn adjusted_threshold(&self, z_threshold: f64) -> f64 {
        z_threshold / self.weight.sqrt()
    }

    /// Returns `true` if `n_samples` passes the minimum-density gate for this site.
    pub fn passes_gate(&self, n_samples: u32) -> bool {
        n_samples >= self.min_samples
    }
}

/// Registry of per-site weights, keyed by `site_id`.
///
/// Cloning this registry is cheap (the map is wrapped in an `Arc` by the
/// caller when it needs to share across threads; the registry itself is
/// `Send + Sync + Clone`).
#[derive(Debug, Default, Clone)]
pub struct SiteWeightRegistry {
    weights: HashMap<String, SiteWeight>,
}

impl SiteWeightRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) the weight for a site.
    pub fn register(&mut self, site_id: impl Into<String>, weight: SiteWeight) {
        self.weights.insert(site_id.into(), weight);
    }

    /// Look up the weight for `site_id`, returning the default if absent.
    pub fn get(&self, site_id: &str) -> SiteWeight {
        self.weights
            .get(site_id)
            .cloned()
            .unwrap_or_else(SiteWeight::default_weight)
    }

    /// Number of site weights explicitly registered.
    pub fn len(&self) -> usize {
        self.weights.len()
    }

    pub fn is_empty(&self) -> bool {
        self.weights.is_empty()
    }

    /// Iterate over all registered `(site_id, SiteWeight)` pairs, sorted by
    /// `site_id` for stable ordering.
    pub fn iter_sorted(&self) -> impl Iterator<Item = (&str, &SiteWeight)> {
        let mut pairs: Vec<(&str, &SiteWeight)> =
            self.weights.iter().map(|(k, v)| (k.as_str(), v)).collect();
        pairs.sort_by_key(|(k, _)| *k);
        pairs.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_weight_is_identity() {
        let w = SiteWeight::default_weight();
        assert_eq!(w.adjusted_threshold(2.5), 2.5);
        assert!(w.passes_gate(1));
    }

    #[test]
    fn half_weight_raises_threshold_by_sqrt2() {
        let w = SiteWeight::new(0.25, 3);
        let thr = w.adjusted_threshold(2.5);
        // z_eff = 2.5 / sqrt(0.25) = 2.5 / 0.5 = 5.0
        assert!((thr - 5.0).abs() < 1e-10);
    }

    #[test]
    fn gate_blocks_low_sample_rounds() {
        let w = SiteWeight::new(1.0, 3);
        assert!(!w.passes_gate(2));
        assert!(w.passes_gate(3));
        assert!(w.passes_gate(10));
    }

    #[test]
    fn registry_falls_back_to_default() {
        let mut reg = SiteWeightRegistry::new();
        reg.register("site_a", SiteWeight::new(0.5, 2));
        let a = reg.get("site_a");
        assert!((a.weight - 0.5).abs() < 1e-10);
        let b = reg.get("site_b_unknown");
        assert_eq!(b.weight, 1.0);
        assert_eq!(b.min_samples, 1);
    }

    #[test]
    fn weight_clamped_to_valid_range() {
        let zero = SiteWeight::new(0.0, 1);
        assert!(zero.weight > 0.0);
        let over = SiteWeight::new(2.0, 1);
        assert_eq!(over.weight, 1.0);
    }
}
