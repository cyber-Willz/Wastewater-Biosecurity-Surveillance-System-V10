//! `ww_detection` — Signal detection for wastewater biosurveillance.
//!
//! Two complementary detection strategies are combined:
//!
//! 1. **Statistical** (`baseline`, `detector`): per-`(site, analyte)` EWMA
//!    baseline with Welford online variance.  An anomaly fires when the
//!    z-score of an incoming concentration measurement exceeds a configurable,
//!    per-analyte threshold.
//!
//! 2. **Spectral** (`spectral`): the sewage network is modelled as a
//!    [`spectral_hypergraph::SpectralHypergraph`] where monitoring sites are
//!    vertices and catchment zones are hyperedges.  The normalized hypergraph
//!    Laplacian Δ encodes the expected spatial smoothness of any normally-
//!    distributed analyte field.  Two scores are computed per observation round:
//!
//!    * **Rayleigh quotient** `R(u) = uᵀΔu / uᵀu` — high value means the
//!      concentration field is spatially rough.
//!    * **High-frequency energy fraction** — fraction of the field's L² energy
//!      in Laplacian eigenvectors beyond the two smoothest modes.
//!
//!    The composite spectral score upgrades the statistical severity tier when
//!    an anomaly is simultaneously local (high z-score) *and* network-wide
//!    (high spectral score), the earliest detectable signature of a spreading
//!    outbreak or discharge event.
//!
//! ## v0.2 — Multi-target surveillance
//!
//! The EWMA smoothing factor α is now per-analyte, derived from the first-order
//! decay rate k (d⁻¹) via `α = clamp(1 − e^{−k}, 0.10, 0.40)`.  Detection
//! thresholds are also per-analyte:
//!
//! | Category               | k range   | α range     | z threshold |
//! |------------------------|-----------|-------------|-------------|
//! | Infectious pathogen    | 0.30–0.55 | 0.26–0.40   | 2.5 σ       |
//! | Illicit substance      | 0.20–0.28 | 0.18–0.24   | 2.8 σ       |
//! | Pharmaceutical / AMR   | 0.05–0.20 | 0.10–0.18   | 3.0 σ       |
//! | Industrial toxicant    | 0.002–0.04| 0.10 (floor)| 3.5 σ       |
//!
//! The `SewageNetwork` spectral model is unchanged — the physical transport
//! equation structure is the same across all analyte categories; only k and f
//! differ, which are handled at the EWMA and scenario layers respectively.

pub mod baseline;
pub mod detector;
pub mod site_weight;
pub mod spectral;
pub mod spectral_mode;

pub use baseline::EwmaBaseline;
pub use detector::{AnomalyDetector, AnomalyEvent, DetectionSeverity};
pub use site_weight::{SiteWeight, SiteWeightRegistry};
pub use spectral::SewageNetwork;
pub use spectral_mode::SpectralMode;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DetectionError {
    #[error("spectral hypergraph error: {0}")]
    Hypergraph(#[from] spectral_hypergraph::HypergraphError),

    #[error("baseline has only {n} observations for site '{site}' / analyte '{pathogen}' (need {need})")]
    InsufficientHistory {
        site:     String,
        pathogen: String,
        n:        usize,
        need:     usize,
    },
}

pub type Result<T> = std::result::Result<T, DetectionError>;
