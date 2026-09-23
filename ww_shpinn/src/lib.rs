//! `ww_shpinn` — Spectral-Hypergraph Physics-Informed regression for
//! wastewater transport.
//!
//! Implements the SHPINN specification of `docs/ww_biosec_theory.md` §7 in a
//! form that is exactly solvable and dependency-light:
//!
//! 1. **Forward PINN** ([`solve_forward`]).  The advection–dispersion–decay
//!    equation
//!    ```text
//!      ∂ₜc + v ∂ₓc − D ∂ₓₓc + k c = f(x,t)
//!    ```
//!    is *linear* in `c`.  Represent `c(x,t) = Σⱼ wⱼ φⱼ(x,t)` with fixed random
//!    `tanh` features `φⱼ = tanh(aⱼ(x−xⱼ) + bⱼ(t−tⱼ))` (a physics-informed
//!    extreme-learning machine).  The PDE residual, initial/boundary conditions
//!    and any measurements are then *linear* in `w`, so the physics-informed
//!    loss is minimised exactly by (ridge-regularised) least squares — no
//!    gradient descent, no stochastic optimiser, reproducible to the bit.
//!    Derivatives of the features are analytic:
//!    `∂ₜφ = b(1−φ²)`, `∂ₓφ = a(1−φ²)`, `∂ₓₓφ = −2a²φ(1−φ²)`.
//!
//! 2. **Inverse source estimation** ([`estimate_source`]).  Given the linear
//!    forward map `y = A f + η`, minimise
//!    `‖A f − y‖²_{Σ⁻¹} + β fᵀ D^{1/2} Δ D^{1/2} f + γ‖f‖²`
//!    where Δ is the normalised hypergraph Laplacian of the sewage network.
//!    Theorem 7.2: for γ > 0 the minimiser is unique and equal to
//!    `(AᵀΣ⁻¹A + βM + γI)⁻¹ AᵀΣ⁻¹y`, `M = D^{1/2} Δ D^{1/2}`.
//!
//! 3. **Analytic references** ([`Adr::gaussian_reference`]) and the residual
//!    stability constant of Theorem 7.1 ([`Adr::kappa`],
//!    [`Adr::stability_bound`]).
//!
//! This crate works on **linear** concentration (the PDE is linear in `c`, not
//! in `log₁₀ c`; see theory §2.2).

use nalgebra::{DMatrix, DVector};
use thiserror::Error;

// ── Errors ───────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ShpinnError {
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("dimension mismatch: {0}")]
    Dim(String),
    #[error("linear solve failed: {0}")]
    Solve(String),
}

pub type Result<T> = std::result::Result<T, ShpinnError>;

// ── Physics ──────────────────────────────────────────────────────────────────

/// Constant-coefficient advection–dispersion–decay reach on `[0, length]`
/// over `[0, horizon]`.
#[derive(Debug, Clone, Copy)]
pub struct Adr {
    /// Mean velocity v (length / time).
    pub v: f64,
    /// Axial dispersion D > 0 (length² / time).
    pub d: f64,
    /// First-order decay rate k ≥ 0 (1 / time) — the ontology `decay_rate_k`.
    pub k: f64,
    pub length: f64,
    pub horizon: f64,
}

impl Adr {
    pub fn validate(&self) -> Result<()> {
        if !(self.d > 0.0) { return Err(ShpinnError::Config("D must be > 0".into())); }
        if !(self.k >= 0.0) { return Err(ShpinnError::Config("k must be >= 0".into())); }
        if !(self.length > 0.0 && self.horizon > 0.0) {
            return Err(ShpinnError::Config("length and horizon must be > 0".into()));
        }
        Ok(())
    }

    /// Half-life ln2/k (`None` for k = 0).
    pub fn half_life(&self) -> Option<f64> {
        if self.k > 0.0 { Some(std::f64::consts::LN_2 / self.k) } else { None }
    }

    /// Contraction rate of Theorem 7.1: `κ = k + D π² / L²`.
    pub fn kappa(&self) -> f64 {
        self.k + self.d * std::f64::consts::PI.powi(2) / (self.length * self.length)
    }

    /// Theorem 7.1: with hard-constrained boundary/initial data and residual
    /// `R`, `‖c_θ(T) − c(T)‖²_{L²} ≤ (1 − e^{−κT}) / κ² · sup‖R‖²`.
    /// `sup_residual_l2` is `sup_t ‖R(t)‖_{L²(0,L)}`; returns the bound on the
    /// **squared** L² error.
    pub fn stability_bound(&self, sup_residual_l2: f64) -> f64 {
        let kap = self.kappa();
        (1.0 - (-kap * self.horizon).exp()) / (kap * kap) * sup_residual_l2 * sup_residual_l2
    }

    /// Exact solution on the unbounded line for the Gaussian initial condition
    /// `c(x,0) = exp(−(x−x0)²/(2 s0²))`:
    /// `s0/√(s0²+2Dt) · exp(−(x−x0−vt)²/(2(s0²+2Dt))) · e^{−kt}`.
    pub fn gaussian_reference(&self, x0: f64, s0: f64, x: f64, t: f64) -> f64 {
        let var = s0 * s0 + 2.0 * self.d * t;
        s0 / var.sqrt() * (-(x - x0 - self.v * t).powi(2) / (2.0 * var)).exp() * (-self.k * t).exp()
    }
}

// ── Forward PINN (linear-feature) ────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct Feature {
    a: f64,
    b: f64,
    /// Activation centre (x, t): φ = tanh(a (x − xc) + b (t − tc)).
    xc: f64,
    tc: f64,
}

impl Feature {
    #[inline]
    fn phi(&self, x: f64, t: f64) -> f64 {
        (self.a * (x - self.xc) + self.b * (t - self.tc)).tanh()
    }
}

/// A scattered measurement `c(x, t) ≈ value` (linear concentration).
#[derive(Debug, Clone, Copy)]
pub struct Measurement {
    pub x: f64,
    pub t: f64,
    pub value: f64,
    /// Standard deviation of the measurement (sets the row weight 1/σ).
    pub sigma: f64,
}

#[derive(Debug, Clone)]
pub struct PielmConfig {
    pub n_features: usize,
    /// Interior collocation grid (nx × nt).
    pub nx: usize,
    pub nt: usize,
    /// Points on the initial line and on each boundary.
    pub n_ic: usize,
    pub n_bc: usize,
    /// Smallest spatial feature scale (feature slope ≈ 2.5 / length_scale).
    pub length_scale: f64,
    /// Smallest temporal feature scale.
    pub time_scale: f64,
    /// Row weights (physics-informed loss coefficients).
    pub w_pde: f64,
    pub w_ic: f64,
    pub w_bc: f64,
    /// Tikhonov ridge on the feature weights.
    pub ridge: f64,
    pub seed: u64,
}

impl PielmConfig {
    /// Reasonable defaults for a reach: feature scales from the reach itself.
    pub fn for_adr(adr: &Adr, length_scale: f64) -> Self {
        let time_scale = (length_scale / adr.v.abs().max(1e-9)).min(adr.horizon);
        Self {
            n_features: 220,
            nx: 44,
            nt: 34,
            n_ic: 160,
            n_bc: 60,
            length_scale,
            time_scale,
            w_pde: 1.0,
            w_ic: 30.0,
            w_bc: 30.0,
            ridge: 1e-9,
            seed: 0x5eed_5eed_5eed,
        }
    }
}

/// Deterministic LCG uniform in [0,1).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Trained forward solution `c(x,t) = Σ wⱼ φⱼ(x,t)`.
#[derive(Debug, Clone)]
pub struct PielmSolution {
    adr: Adr,
    feats: Vec<Feature>,
    w: DVector<f64>,
}

impl PielmSolution {
    pub fn eval(&self, x: f64, t: f64) -> f64 {
        self.feats.iter().zip(self.w.iter()).map(|(f, w)| w * f.phi(x, t)).sum()
    }

    /// PDE residual `∂ₜc + v∂ₓc − D∂ₓₓc + kc − f` at `(x,t)` (analytic).
    pub fn residual(&self, x: f64, t: f64, source: &dyn Fn(f64, f64) -> f64) -> f64 {
        let Adr { v, d, k, .. } = self.adr;
        let mut r = -source(x, t);
        for (f, w) in self.feats.iter().zip(self.w.iter()) {
            let p = f.phi(x, t);
            let s = 1.0 - p * p;
            r += w * (s * (f.b + v * f.a + 2.0 * d * f.a * f.a * p) + k * p);
        }
        r
    }

    /// Max |c_θ − reference| on an `nx × nt` grid over the whole domain.
    pub fn max_abs_error(&self, reference: &dyn Fn(f64, f64) -> f64, nx: usize, nt: usize) -> f64 {
        let mut m = 0.0f64;
        for i in 0..=nx {
            for j in 0..=nt {
                let x = self.adr.length * i as f64 / nx as f64;
                let t = self.adr.horizon * j as f64 / nt as f64;
                m = m.max((self.eval(x, t) - reference(x, t)).abs());
            }
        }
        m
    }

    /// Discrete `sup_t ‖R(t)‖_{L²(0,L)}` of the PDE residual on a grid.
    pub fn sup_residual_l2(&self, source: &dyn Fn(f64, f64) -> f64, nx: usize, nt: usize) -> f64 {
        let dx = self.adr.length / nx as f64;
        (0..=nt).map(|j| {
            let t = self.adr.horizon * j as f64 / nt as f64;
            let s: f64 = (0..=nx).map(|i| {
                let x = self.adr.length * i as f64 / nx as f64;
                self.residual(x, t, source).powi(2) * dx
            }).sum();
            s.sqrt()
        }).fold(0.0, f64::max)
    }

    pub fn n_features(&self) -> usize { self.feats.len() }
}

/// Solve the forward ADR problem with initial condition `ic(x)`, Dirichlet
/// data `bc_left(t)`, `bc_right(t)`, source `source(x,t)` and optional
/// measurements, by minimising the (linear) physics-informed loss
///
/// `w_pde ΣR² + w_ic Σ(c(x,0)−ic)² + w_bc Σ(c−bc)² + Σ((c−y)/σ)² + ridge‖w‖²`.
pub fn solve_forward(
    adr:      &Adr,
    source:   &dyn Fn(f64, f64) -> f64,
    ic:       &dyn Fn(f64) -> f64,
    bc_left:  &dyn Fn(f64) -> f64,
    bc_right: &dyn Fn(f64) -> f64,
    data:     &[Measurement],
    cfg:      &PielmConfig,
) -> Result<PielmSolution> {
    adr.validate()?;
    if cfg.n_features < 4 || cfg.nx < 2 || cfg.nt < 2 {
        return Err(ShpinnError::Config("need n_features>=4, nx>=2, nt>=2".into()));
    }

    // Random features (deterministic).
    let mut rng = Lcg(cfg.seed);
    let a_max = 2.5 / cfg.length_scale;
    let b_max = 2.5 / cfg.time_scale;
    let feats: Vec<Feature> = (0..cfg.n_features).map(|_| Feature {
        a:  (2.0 * rng.next() - 1.0) * a_max,
        b:  (2.0 * rng.next() - 1.0) * b_max,
        xc: rng.next() * adr.length,
        tc: rng.next() * adr.horizon,
    }).collect();
    let n = feats.len();

    let n_rows = cfg.nx * cfg.nt + cfg.n_ic + 2 * cfg.n_bc + data.len();
    let mut a = DMatrix::<f64>::zeros(n_rows, n);
    let mut rhs = DVector::<f64>::zeros(n_rows);
    let mut r = 0usize;

    let Adr { v, d, k, length, horizon } = *adr;

    // PDE rows on a cell-centred interior grid.
    let sw = cfg.w_pde.sqrt();
    for i in 0..cfg.nx {
        for j in 0..cfg.nt {
            let x = length * (i as f64 + 0.5) / cfg.nx as f64;
            let t = horizon * (j as f64 + 0.5) / cfg.nt as f64;
            for (c, f) in feats.iter().enumerate() {
                let p = f.phi(x, t);
                let s = 1.0 - p * p;
                a[(r, c)] = sw * (s * (f.b + v * f.a + 2.0 * d * f.a * f.a * p) + k * p);
            }
            rhs[r] = sw * source(x, t);
            r += 1;
        }
    }
    // Initial condition.
    let sw = cfg.w_ic.sqrt();
    for i in 0..cfg.n_ic {
        let x = length * i as f64 / (cfg.n_ic - 1).max(1) as f64;
        for (c, f) in feats.iter().enumerate() { a[(r, c)] = sw * f.phi(x, 0.0); }
        rhs[r] = sw * ic(x);
        r += 1;
    }
    // Boundary conditions.
    let sw = cfg.w_bc.sqrt();
    for i in 0..cfg.n_bc {
        let t = horizon * i as f64 / (cfg.n_bc - 1).max(1) as f64;
        for (c, f) in feats.iter().enumerate() { a[(r, c)] = sw * f.phi(0.0, t); }
        rhs[r] = sw * bc_left(t);
        r += 1;
        for (c, f) in feats.iter().enumerate() { a[(r, c)] = sw * f.phi(length, t); }
        rhs[r] = sw * bc_right(t);
        r += 1;
    }
    // Measurements.
    for m in data {
        if !(m.sigma > 0.0) { return Err(ShpinnError::Config("measurement sigma must be > 0".into())); }
        let sw = 1.0 / m.sigma;
        for (c, f) in feats.iter().enumerate() { a[(r, c)] = sw * f.phi(m.x, m.t); }
        rhs[r] = sw * m.value;
        r += 1;
    }
    debug_assert_eq!(r, n_rows);

    // Ridge normal equations (Cholesky); SVD fallback.
    let at = a.transpose();
    let mut ata = &at * &a;
    for i in 0..n { ata[(i, i)] += cfg.ridge; }
    let atb = &at * &rhs;
    let w = match ata.clone().cholesky() {
        Some(ch) => ch.solve(&atb),
        None => ata
            .svd(true, true)
            .solve(&atb, 1e-12)
            .map_err(|e| ShpinnError::Solve(e.to_string()))?,
    };
    Ok(PielmSolution { adr: *adr, feats, w })
}

// ── Inverse source estimation with the hypergraph prior ─────────────────────

/// `M = D_v^{1/2} Δ D_v^{1/2}` — the hypergraph smoothness prior on a source
/// field `f` (`fᵀ M f = gᵀΔg`, `g = D_v^{1/2} f`).  Its kernel is `span(1)` for
/// a connected hypergraph: uniform mixing is the zero-energy source.
pub fn prior_matrix(laplacian: &DMatrix<f64>, sqrt_deg: &[f64]) -> Result<DMatrix<f64>> {
    let n = laplacian.nrows();
    if laplacian.ncols() != n || sqrt_deg.len() != n {
        return Err(ShpinnError::Dim("laplacian must be n×n and sqrt_deg length n".into()));
    }
    let s = DMatrix::from_diagonal(&DVector::from_column_slice(sqrt_deg));
    Ok(&s * laplacian * &s)
}

/// Inputs of the regularised inverse problem `y = A f + η`.
pub struct SourceProblem<'a> {
    /// Forward map (measurements × sources).
    pub a: &'a DMatrix<f64>,
    pub y: &'a DVector<f64>,
    /// Per-measurement noise variance (diagonal of Σ).
    pub noise_var: &'a DVector<f64>,
    /// Prior matrix from [`prior_matrix`].
    pub prior: &'a DMatrix<f64>,
}

impl SourceProblem<'_> {
    fn check(&self) -> Result<()> {
        let (m, n) = self.a.shape();
        if self.y.len() != m || self.noise_var.len() != m {
            return Err(ShpinnError::Dim("y and noise_var must have A.nrows() entries".into()));
        }
        if self.prior.nrows() != n || self.prior.ncols() != n {
            return Err(ShpinnError::Dim("prior must be n×n with n = A.ncols()".into()));
        }
        if self.noise_var.iter().any(|&v| !(v > 0.0)) {
            return Err(ShpinnError::Config("noise variances must be > 0".into()));
        }
        Ok(())
    }

    /// `J(f) = ‖Af−y‖²_{Σ⁻¹} + β fᵀ M f + γ‖f‖²`.
    pub fn objective(&self, f: &DVector<f64>, beta: f64, gamma: f64) -> f64 {
        let r = self.a * f - self.y;
        let data: f64 = r.iter().zip(self.noise_var.iter()).map(|(e, v)| e * e / v).sum();
        data + beta * f.dot(&(self.prior * f)) + gamma * f.dot(f)
    }
}

/// Theorem 7.2: `f* = (AᵀΣ⁻¹A + βM + γI)⁻¹ AᵀΣ⁻¹ y`.  Unique when `γ > 0`;
/// for `γ = 0` uniqueness needs `ker A ∩ ker M = {0}` and the system is
/// solved by SVD (minimum-norm on any residual null space).
pub fn estimate_source(p: &SourceProblem<'_>, beta: f64, gamma: f64) -> Result<DVector<f64>> {
    p.check()?;
    if beta < 0.0 || gamma < 0.0 {
        return Err(ShpinnError::Config("beta and gamma must be >= 0".into()));
    }
    let n = p.a.ncols();
    let winv = DVector::from_iterator(p.noise_var.len(), p.noise_var.iter().map(|v| 1.0 / v));
    let mut aw = p.a.clone();
    for (i, mut row) in aw.row_iter_mut().enumerate() { row *= winv[i]; }
    let mut h = p.a.transpose() * &aw + p.prior * beta;
    for i in 0..n { h[(i, i)] += gamma; }
    let g = aw.transpose() * p.y;
    match h.clone().cholesky() {
        Some(ch) => Ok(ch.solve(&g)),
        None => h.svd(true, true).solve(&g, 1e-12).map_err(|e| ShpinnError::Solve(e.to_string())),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use ww_detection::SewageNetwork;

    fn belize() -> SewageNetwork {
        let s = |x: &str| x.to_string();
        let catchments = vec![
            (s("trunk"), vec![s("bcity_north"), s("bcity_south"), s("bcity_wwtp")]),
            (s("belmopan"), vec![s("belmopan_core"), s("belmopan_ind")]),
        ];
        let extra: Vec<String> = ["bcity_north", "bcity_south", "bcity_wwtp", "belmopan_core",
            "belmopan_ind", "orange_walk", "san_ignacio", "dangriga"].iter().map(|x| s(x)).collect();
        SewageNetwork::build(&catchments, &extra).unwrap()
    }

    /// The forward PINN reproduces the analytic Gaussian advection–dispersion–
    /// decay solution (Prop 2.1 / heat-kernel form) to a few percent of peak.
    #[test]
    fn forward_matches_analytic_solution() {
        let adr = Adr { v: 1.0, d: 0.05, k: 0.5, length: 6.0, horizon: 1.5 };
        let (x0, s0) = (1.5, 0.5);
        let reference = |x: f64, t: f64| adr.gaussian_reference(x0, s0, x, t);
        let cfg = PielmConfig::for_adr(&adr, 0.5);
        let sol = solve_forward(
            &adr,
            &|_, _| 0.0,
            &|x| reference(x, 0.0),
            &|t| reference(0.0, t),
            &|t| reference(6.0, t),
            &[],
            &cfg,
        ).unwrap();
        let err = sol.max_abs_error(&reference, 60, 30);
        assert!(err < 0.03, "max abs error {err}");
        assert_eq!(sol.n_features(), cfg.n_features);
    }

    /// Mass decays as e^{−kt}: the integral of the PINN solution follows it.
    #[test]
    fn total_mass_decays_at_rate_k() {
        let adr = Adr { v: 1.0, d: 0.05, k: 0.5, length: 6.0, horizon: 1.5 };
        let reference = |x: f64, t: f64| adr.gaussian_reference(1.5, 0.5, x, t);
        let cfg = PielmConfig::for_adr(&adr, 0.5);
        let sol = solve_forward(&adr, &|_, _| 0.0, &|x| reference(x, 0.0),
            &|t| reference(0.0, t), &|t| reference(6.0, t), &[], &cfg).unwrap();
        let mass = |t: f64| -> f64 {
            let n = 400;
            (0..n).map(|i| sol.eval(6.0 * (i as f64 + 0.5) / n as f64, t) * 6.0 / n as f64).sum()
        };
        let ratio = mass(1.5) / mass(0.0);
        let expect = (-adr.k * 1.5f64).exp();
        assert!((ratio - expect).abs() < 0.03, "mass ratio {ratio} vs {expect}");
    }

    /// Measurements pull the solution towards data where the physics is
    /// mis-specified (wrong k): data-assimilation sanity check.
    #[test]
    fn measurements_reduce_error_under_wrong_decay() {
        let truth = Adr { v: 1.0, d: 0.05, k: 0.5, length: 6.0, horizon: 1.5 };
        let wrong = Adr { k: 0.0, ..truth };
        let reference = |x: f64, t: f64| truth.gaussian_reference(1.5, 0.5, x, t);
        let cfg = PielmConfig::for_adr(&wrong, 0.5);
        let plain = solve_forward(&wrong, &|_, _| 0.0, &|x| reference(x, 0.0),
            &|t| reference(0.0, t), &|t| reference(6.0, t), &[], &cfg).unwrap();
        let mut data = Vec::new();
        for i in 0..8 { for j in 1..7 {
            let (x, t) = (0.5 + 0.7 * i as f64, 0.25 * j as f64);
            data.push(Measurement { x, t, value: reference(x, t), sigma: 0.01 });
        } }
        let assim = solve_forward(&wrong, &|_, _| 0.0, &|x| reference(x, 0.0),
            &|t| reference(0.0, t), &|t| reference(6.0, t), &data, &cfg).unwrap();
        let e0 = plain.max_abs_error(&reference, 40, 20);
        let e1 = assim.max_abs_error(&reference, 40, 20);
        assert!(e1 < e0, "assimilated {e1} should beat model-only {e0}");
    }

    #[test]
    fn stability_constants() {
        let adr = Adr { v: 1.0, d: 0.1, k: 0.5, length: 5.0, horizon: 2.0 };
        let kap = 0.5 + 0.1 * std::f64::consts::PI.powi(2) / 25.0;
        assert!((adr.kappa() - kap).abs() < 1e-12);
        // Decaying analytes are more stable than persistent ones.
        let toxic = Adr { k: 0.005, ..adr };
        assert!(adr.stability_bound(1.0) < toxic.stability_bound(1.0));
        assert!((adr.half_life().unwrap() - 2f64.ln() / 0.5).abs() < 1e-12);
        assert!(Adr { k: 0.0, ..adr }.half_life().is_none());
        assert!(Adr { d: 0.0, ..adr }.validate().is_err());
    }

    /// Theorem 7.2: the closed form is the unique stationary point.
    #[test]
    fn source_estimate_is_stationary_and_optimal() {
        let net = belize();
        let m = prior_matrix(net.laplacian(), net.sqrt_degrees()).unwrap();
        // Ill-conditioned forward map (smoothing).
        let a = DMatrix::from_fn(8, 8, |i, j| (-((i as f64 - j as f64).powi(2)) / 4.0).exp());
        let truth = DVector::from_vec(vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
        let y = &a * &truth;
        let nv = DVector::from_element(8, 0.01);
        let p = SourceProblem { a: &a, y: &y, noise_var: &nv, prior: &m };
        let (beta, gamma) = (0.5, 1e-3);
        let f = estimate_source(&p, beta, gamma).unwrap();

        // Gradient 2[AᵀΣ⁻¹(Af−y) + βMf + γf] = 0
        let r = &a * &f - &y;
        let w = DVector::from_iterator(8, r.iter().zip(nv.iter()).map(|(e, v)| e / v));
        let grad = a.transpose() * w + (&m * &f) * beta + &f * gamma;
        assert!(grad.norm() < 1e-8, "gradient norm {}", grad.norm());

        // No perturbation improves the objective (strict convexity).
        let j0 = p.objective(&f, beta, gamma);
        for k in 0..8 {
            for s in [-1e-2, 1e-2] {
                let mut g = f.clone();
                g[k] += s;
                assert!(p.objective(&g, beta, gamma) > j0);
            }
        }
    }

    /// The hypergraph prior recovers a spatially uniform source from noisy,
    /// ill-conditioned data better than an unregularised solve, and a large β
    /// drives the estimate towards the prior's kernel span(1).
    #[test]
    fn hypergraph_prior_stabilises_inversion() {
        let net = belize();
        let m = prior_matrix(net.laplacian(), net.sqrt_degrees()).unwrap();
        let a = DMatrix::from_fn(8, 8, |i, j| (-((i as f64 - j as f64).powi(2)) / 4.0).exp());
        let truth = DVector::from_element(8, 2.0);
        let mut rng = Lcg(7);
        let noise: Vec<f64> = (0..8).map(|_| (rng.next() - 0.5) * 0.2).collect();
        let y = &a * &truth + DVector::from_vec(noise);
        let nv = DVector::from_element(8, 0.01);
        let p = SourceProblem { a: &a, y: &y, noise_var: &nv, prior: &m };

        let raw = estimate_source(&p, 0.0, 1e-9).unwrap();
        let reg = estimate_source(&p, 5.0, 1e-6).unwrap();
        let e_raw = (&raw - &truth).norm();
        let e_reg = (&reg - &truth).norm();
        assert!(e_reg < e_raw, "regularised {e_reg} vs raw {e_raw}");

        let huge = estimate_source(&p, 1e6, 1e-9).unwrap();
        let mean = huge.mean();
        assert!(huge.iter().all(|v| (v - mean).abs() < 1e-3 * mean.abs().max(1.0)),
            "large beta must flatten the estimate: {huge}");
    }

    #[test]
    fn dimension_errors_are_reported() {
        let net = belize();
        let m = prior_matrix(net.laplacian(), net.sqrt_degrees()).unwrap();
        let a = DMatrix::<f64>::identity(8, 8);
        let y = DVector::from_element(7, 1.0);
        let nv = DVector::from_element(8, 1.0);
        let p = SourceProblem { a: &a, y: &y, noise_var: &nv, prior: &m };
        assert!(matches!(estimate_source(&p, 1.0, 1.0), Err(ShpinnError::Dim(_))));
        assert!(prior_matrix(net.laplacian(), &[1.0]).is_err());
        let _ = HashMap::<u8, u8>::new();
    }
}
