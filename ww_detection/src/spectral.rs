//! Spectral network anomaly scoring for the wastewater sewage graph.
//!
//! The sewage network is modelled as a [`SpectralHypergraph`]:
//! * **Vertices** — monitoring sites (one vertex per site).
//! * **Hyperedges** — catchment zones (groups of sites that drain into the
//!   same trunk sewer or WWTP inlet), plus one weak background hyperedge
//!   connecting all sites to prevent isolated-vertex degeneracy.
//!
//! The normalized hypergraph Laplacian `Δ` (Zhou–Huang–Schölkopf) satisfies
//! `spec(Δ) ⊂ [0, 1]` and
//! `uᵀΔu = Σ_e w_e Σ_{v∈e} (x_v − x̄_e)²` with `x = D_v^{-1/2} u`: the
//! weighted within-catchment scatter of the degree-scaled field.  Its null
//! vector is `D_v^{1/2}·1`, **not** the constant vector.
//!
//! | Score | Formula | Range | Meaning |
//! |---|---|---|---|
//! | Rayleigh quotient (norm.) | `uᵀΔu / (uᵀu · λ_max)` | `[0,1]` | 0=mixed, 1=rough |
//! | HF energy fraction | `1 − Σ_{k<K}(vₖᵀu)²/uᵀu` | `[0,1]` | fraction in high modes |
//! | Composite | `0.6·RQ + 0.4·HFF` | `[0,1]` | combined network score |
//!
//! ## v0.4 — corrections (see `docs/ww_biosec_theory.md` §4)
//!
//! * The Rayleigh quotient is normalised by `λ_max(Δ)` (= 1 for these
//!   networks) instead of 2.  The old `R/2` only reached `[0, ½]`, so the
//!   composite was bounded by 0.7 and the 0.40 threshold sat at 57 % of range.
//! * **Input field.**  [`SewageNetwork::spectral_score`] on *raw* log₁₀
//!   concentrations is not baseline-invariant (`S(u + a·1) ≠ S(u)`): the level
//!   `μ/σ` of the analyte decided the score, not spatial structure.  Use
//!   [`SewageNetwork::spectral_score_residual`], which applies `Δ` to the
//!   mixing-scaled standardised residual `u_v = √d_v · z_v`, for which uniform
//!   mixing is the zero-energy state and there is no large DC component.
//! * White residuals score high (the statistic is high-pass), so a fixed 0.40
//!   is not meaningful; [`SewageNetwork::null_threshold`] returns an empirical
//!   null quantile.

use std::collections::HashMap;

use nalgebra::{DMatrix, DVector};
use spectral_hypergraph::{HypergraphBuilder, VertexId};
use spectral_hypergraph::laplacian::dense_normalized_laplacian;
use spectral_hypergraph::spectral::{dense_eigen, EigenDecomposition};

use crate::Result;

// Weight of the global background hyperedge that keeps every vertex connected.
const BACKGROUND_WEIGHT: f64 = 0.05;

/// A compiled sewage-network spectral model, ready for repeated scoring.
pub struct SewageNetwork {
    /// Site IDs in row-order of the Laplacian.
    pub site_ids: Vec<String>,
    laplacian:    DMatrix<f64>,
    eig:          EigenDecomposition,
    /// `√d_v` per site (row order): the kernel direction of Δ.
    sqrt_deg:     Vec<f64>,
    /// Largest eigenvalue of Δ (normaliser of the Rayleigh quotient).
    lambda_max:   f64,
    /// Number of *real* catchments (i.e. each with ≥ 2 members) that were
    /// passed to [`Self::build`].  Used by [`SpectralMode::Gated`] to decide
    /// whether the spectral score should influence severity.
    n_real_catchments: usize,
}

impl SewageNetwork {
    /// Build the spectral model from catchment definitions.
    ///
    /// `catchments`: `(catchment_label, member_site_ids)` pairs.
    /// `extra_sites`: additional site IDs to include even when they are not in
    /// any named catchment (they will still participate in the background edge).
    ///
    /// Hyperedges with `|members| < 2` are silently skipped.
    pub fn build(
        catchments:  &[(String, Vec<String>)],
        extra_sites: &[String],
    ) -> Result<Self> {
        let mut builder     = HypergraphBuilder::new();
        let mut site_ids    = Vec::<String>::new();
        let mut site_to_vid = HashMap::<String, VertexId>::new();

        // ── Deduplicate site IDs in stable order ─────────────────────────────
        let mut seen = std::collections::HashSet::<String>::new();
        let mut register = |s: &str| {
            if seen.insert(s.to_string()) {
                site_ids.push(s.to_string());
            }
        };
        for (_, members) in catchments {
            for s in members { register(s); }
        }
        for s in extra_sites { register(s); }

        // ── Add vertices ─────────────────────────────────────────────────────
        for s in &site_ids {
            let vid = builder.add_vertex(s.as_str())?;
            site_to_vid.insert(s.clone(), vid);
        }

        // ── Add catchment hyperedges ──────────────────────────────────────────
        let mut n_real_catchments = 0usize;
        for (_, members) in catchments {
            let vids: Vec<VertexId> = members
                .iter()
                .filter_map(|s| site_to_vid.get(s).copied())
                .collect();
            if vids.len() >= 2 {
                builder.add_hyperedge(&vids, 1.0)?;
                n_real_catchments += 1;
            }
        }

        // ── Global background edge (prevents IsolatedVertex degeneracy) ───────
        // All sites are members of this single weak hyperedge, so that even
        // sites belonging to no named catchment still have nonzero degree and
        // the normalized Laplacian is well-defined.
        let all_vids: Vec<VertexId> =
            site_ids.iter().filter_map(|s| site_to_vid.get(s).copied()).collect();
        if all_vids.len() >= 2 {
            builder.add_hyperedge(&all_vids, BACKGROUND_WEIGHT)?;
        }

        let hg        = builder.build()?;
        let laplacian = dense_normalized_laplacian(&hg)?;
        let eig       = dense_eigen(&laplacian);

        let mut sqrt_deg = Vec::with_capacity(site_ids.len());
        for s in &site_ids {
            let vid = site_to_vid[s];
            sqrt_deg.push(hg.vertex_degree(vid)?.sqrt());
        }
        let lambda_max = eig.eigenvalues.iter().copied()
            .fold(f64::MIN, f64::max)
            .max(1e-12);

        Ok(Self { site_ids, laplacian, eig, sqrt_deg, lambda_max, n_real_catchments })
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    fn field_vec(&self, concs: &HashMap<String, f64>) -> DVector<f64> {
        DVector::from_vec(
            self.site_ids
                .iter()
                .map(|s| *concs.get(s).unwrap_or(&0.0))
                .collect(),
        )
    }

    // ── Public scores ────────────────────────────────────────────────────────

    /// Normalised Rayleigh quotient `R(u)/λ_max ∈ [0, 1]`.
    ///
    /// Low value: the pathogen field is spatially smooth — concentrations vary
    /// gradually between connected sites (consistent with normal diffusion).
    /// High value: the field has large high-frequency components — adjacent
    /// sites have sharply diverging concentrations, the hallmark of a
    /// localised outbreak that has not yet diffused through the network.
    pub fn rayleigh_quotient_norm(&self, concs: &HashMap<String, f64>) -> f64 {
        let u     = self.field_vec(concs);
        let norm2 = u.dot(&u);
        if norm2 < 1e-10 { return 0.0; }
        let delta_u = &self.laplacian * &u;
        (u.dot(&delta_u) / norm2 / self.lambda_max).clamp(0.0, 1.0)
    }

    /// Fraction of field energy in Laplacian eigenmodes beyond the `k`
    /// smoothest (`k = 2` keeps the DC component and Fiedler direction).
    pub fn high_freq_fraction(&self, concs: &HashMap<String, f64>, k: usize) -> f64 {
        let u     = self.field_vec(concs);
        let total = u.dot(&u);
        if total < 1e-10 { return 0.0; }
        let k = k.min(self.site_ids.len());
        let low: f64 = (0..k).map(|i| {
            let v = self.eig.eigenvectors.column(i);
            let c = v.dot(&u);
            c * c
        }).sum();
        ((total - low) / total).clamp(0.0, 1.0)
    }

    /// Composite spectral score of a **raw** concentration field, in `[0, 1]`.
    ///
    /// Retained for compatibility; it is not baseline-invariant — prefer
    /// [`Self::spectral_score_residual`].
    pub fn spectral_score(&self, concs: &HashMap<String, f64>) -> f64 {
        let rq  = self.rayleigh_quotient_norm(concs);
        let hff = self.high_freq_fraction(concs, 2);
        (0.6 * rq + 0.4 * hff).clamp(0.0, 1.0)
    }

    /// Composite score of an explicit field vector (site row-order).
    fn score_vec(&self, u: &DVector<f64>) -> f64 {
        let norm2 = u.dot(u);
        if norm2 < 1e-10 { return 0.0; }
        let rq  = (u.dot(&(&self.laplacian * u)) / norm2 / self.lambda_max).clamp(0.0, 1.0);
        let k   = 2usize.min(self.site_ids.len());
        let low: f64 = (0..k).map(|i| {
            let c = self.eig.eigenvectors.column(i).dot(u);
            c * c
        }).sum();
        let hff = ((norm2 - low) / norm2).clamp(0.0, 1.0);
        (0.6 * rq + 0.4 * hff).clamp(0.0, 1.0)
    }

    /// Mixing-scaled standardised residual field `u_v = √d_v · clamp(z_v, ±6)`.
    /// Sites absent from `z` (or not yet warm) contribute 0.
    pub fn residual_field(&self, z: &HashMap<String, f64>) -> DVector<f64> {
        DVector::from_vec(
            self.site_ids.iter().enumerate()
                .map(|(i, s)| self.sqrt_deg[i] * z.get(s).copied().unwrap_or(0.0).clamp(-6.0, 6.0))
                .collect(),
        )
    }

    /// Composite score of the mixing-scaled standardised residual field —
    /// the baseline-invariant replacement for [`Self::spectral_score`].
    pub fn spectral_score_residual(&self, z: &HashMap<String, f64>) -> f64 {
        self.score_vec(&self.residual_field(z))
    }

    /// Empirical null quantile of [`Self::spectral_score_residual`] for i.i.d.
    /// `N(0,1)` residuals (deterministic: fixed-seed LCG, Irwin–Hall normals).
    /// `q ∈ (0,1)`, e.g. 0.95.
    pub fn null_threshold(&self, q: f64, samples: usize, seed: u64) -> f64 {
        let n = self.site_ids.len();
        let mut state = seed;
        let mut vals = Vec::with_capacity(samples);
        for _ in 0..samples {
            let u = DVector::from_vec((0..n).map(|i| {
                let mut x = 0.0;
                for _ in 0..12 {
                    state = state.wrapping_mul(6_364_136_223_846_793_005)
                                 .wrapping_add(1_442_695_040_888_963_407);
                    x += (state >> 11) as f64 / (1u64 << 53) as f64;
                }
                self.sqrt_deg[i] * (x - 6.0)
            }).collect());
            vals.push(self.score_vec(&u));
        }
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((q.clamp(0.0, 1.0)) * (vals.len().saturating_sub(1)) as f64).round() as usize;
        vals[idx]
    }

    /// Fiedler value (λ₂ of Δ): the algebraic connectivity of the network.
    /// Near-zero → weakly connected network; larger → well-mixed topology.
    pub fn fiedler_value(&self) -> f64 {
        self.eig.eigenvalues.iter().copied()
            .find(|&v| v > 1e-8)
            .unwrap_or(0.0)
    }

    pub fn n_sites(&self) -> usize { self.site_ids.len() }

    /// Number of catchments with ≥ 2 member sites that were registered at
    /// build time.  Used by [`crate::spectral_mode::SpectralMode::Gated`] to
    /// gate the spectral severity upgrade.
    pub fn n_real_catchments(&self) -> usize { self.n_real_catchments }

    /// The normalised hypergraph Laplacian Δ (site row-order).
    pub fn laplacian(&self) -> &DMatrix<f64> { &self.laplacian }

    /// `√d_v` per site (row-order): the kernel direction of Δ.
    pub fn sqrt_degrees(&self) -> &[f64] { &self.sqrt_deg }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn belize() -> SewageNetwork {
        let s = |x: &str| x.to_string();
        let catchments = vec![
            (s("trunk"), vec![s("bcity_north"), s("bcity_south"), s("bcity_wwtp")]),
            (s("belmopan"), vec![s("belmopan_core"), s("belmopan_ind")]),
            (s("ow"), vec![s("orange_walk")]),
            (s("si"), vec![s("san_ignacio")]),
            (s("dg"), vec![s("dangriga")]),
        ];
        let extra: Vec<String> = ["bcity_north", "bcity_south", "bcity_wwtp", "belmopan_core",
            "belmopan_ind", "orange_walk", "san_ignacio", "dangriga"].iter().map(|x| s(x)).collect();
        SewageNetwork::build(&catchments, &extra).unwrap()
    }

    #[test]
    fn exact_spectrum() {
        let n = belize();
        let ev: Vec<f64> = n.eig.eigenvalues.iter().copied().collect();
        let expect = [0.0, 1.0 / 21.0, 9.0 / 14.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        for (a, b) in ev.iter().zip(expect.iter()) {
            assert!((a - b).abs() < 1e-10, "{a} vs {b}");
        }
        assert!((n.fiedler_value() - 1.0 / 21.0).abs() < 1e-10);
        assert!((n.lambda_max - 1.0).abs() < 1e-10);
    }

    #[test]
    fn scores_are_in_unit_interval_and_uniform_mixing_is_zero() {
        let n = belize();
        // u = sqrt(d) * c  (uniform residual) is the kernel of Δ: score 0.
        let z: HashMap<String, f64> = n.site_ids.iter().map(|s| (s.clone(), 1.7)).collect();
        assert!(n.spectral_score_residual(&z) < 1e-9);
        // arbitrary fields stay in [0,1]
        for seed in 0..50u64 {
            let mut st = seed.wrapping_mul(2654435761).wrapping_add(1);
            let z: HashMap<String, f64> = n.site_ids.iter().map(|s| {
                st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (s.clone(), ((st >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 12.0)
            }).collect();
            let sc = n.spectral_score_residual(&z);
            assert!((0.0..=1.0).contains(&sc));
        }
    }

    /// Spectral decomposition of the composite score:
    /// `S = 0.6·Σ λ_i p_i / λ_max + 0.4·(1 − p_1 − p_2)` with
    /// `p_i = ⟨v_i,u⟩²/‖u‖²`.
    #[test]
    fn composite_matches_eigen_decomposition() {
        let n = belize();
        let u = DVector::from_vec(vec![0.3, -1.2, 2.2, 0.7, -0.4, 1.1, -0.9, 0.5]);
        let tot = u.dot(&u);
        let p: Vec<f64> = (0..8).map(|i| n.eig.eigenvectors.column(i).dot(&u).powi(2) / tot).collect();
        let rq: f64 = (0..8).map(|i| n.eig.eigenvalues[i] * p[i]).sum();
        let hff = 1.0 - p[0] - p[1];
        let expect = 0.6 * rq + 0.4 * hff;
        assert!((n.score_vec(&u) - expect).abs() < 1e-12);
    }

    #[test]
    fn null_threshold_is_deterministic_and_monotone() {
        let n = belize();
        let a = n.null_threshold(0.95, 4000, 42);
        let b = n.null_threshold(0.95, 4000, 42);
        assert_eq!(a, b);
        assert!(n.null_threshold(0.5, 4000, 42) <= a);
        assert!(a > 0.0 && a <= 1.0);
    }
}
