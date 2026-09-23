//! Spectral-term gate: explicit enable/disable for the network-spread score.
//!
//! ## Problem
//!
//! The hypergraph spectral score is only meaningful when the catchment
//! topology reflects real sewer connectivity.  When the `catchments` table is
//! empty, or populated only with health-board proxies (as in the `real_data`
//! evaluation), the score is computed from an arbitrary grouping that has no
//! physical basis — it adds noise rather than signal.
//!
//! ## Solution
//!
//! [`SpectralMode`] wraps the scoring call:
//!
//! * **`Enabled`** — the spectral score is computed and used for severity
//!   upgrades exactly as before.  Intended for deployments where real
//!   catchment/sewer connectivity data has been loaded via `PUT /v1/network`
//!   and the catchments contain ≥ 2 real sites each.
//!
//! * **`Gated { min_real_catchments }`** — the spectral score is computed
//!   (so diagnostics are still available) but is **not** used for severity
//!   upgrades unless the network has at least `min_real_catchments` catchments
//!   with ≥ 2 members.  The score field in `AnomalyEvent` is still populated so
//!   the API response and audit trail retain it for retrospective analysis.
//!
//! * **`Disabled`** — the spectral score is always zero; the network is not
//!   built.  Useful during initial deployment when no connectivity data
//!   exists at all.
//!
//! The default for a freshly initialised [`Engine`] is
//! `Gated { min_real_catchments: 2 }` — conservative until real topology is
//! registered.

/// Gate controlling whether the spectral network score influences alert severity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpectralMode {
    /// Spectral score always influences severity upgrades.
    Enabled,
    /// Spectral score is computed but only influences severity when the network
    /// has at least `min_real_catchments` catchments with ≥ 2 members.
    Gated { min_real_catchments: usize },
    /// Spectral score is always zero; no network is needed.
    Disabled,
}

impl SpectralMode {
    /// Default: gated on ≥ 2 real catchments.
    pub const DEFAULT: Self = Self::Gated { min_real_catchments: 2 };

    /// Returns `true` if the spectral score from a network with
    /// `n_real_catchments` real catchments (each with ≥ 2 members) should
    /// be used for severity upgrades.
    pub fn is_active(&self, n_real_catchments: usize) -> bool {
        match self {
            Self::Enabled                        => true,
            Self::Gated { min_real_catchments }  => n_real_catchments >= *min_real_catchments,
            Self::Disabled                       => false,
        }
    }

    /// Applies the gate: returns `effective_spectral` for the severity call,
    /// i.e. the raw score if active, else 0.0.
    pub fn effective_score(&self, raw_score: f64, n_real_catchments: usize) -> f64 {
        if self.is_active(n_real_catchments) { raw_score } else { 0.0 }
    }
}

impl Default for SpectralMode {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl std::fmt::Display for SpectralMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enabled                       => write!(f, "enabled"),
            Self::Gated { min_real_catchments } => write!(f, "gated(>={min_real_catchments}_catchments)"),
            Self::Disabled                      => write!(f, "disabled"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_always_passes() {
        assert!(SpectralMode::Enabled.is_active(0));
        assert!(SpectralMode::Enabled.is_active(100));
    }

    #[test]
    fn disabled_never_passes() {
        assert!(!SpectralMode::Disabled.is_active(0));
        assert!(!SpectralMode::Disabled.is_active(100));
    }

    #[test]
    fn gated_threshold() {
        let m = SpectralMode::Gated { min_real_catchments: 2 };
        assert!(!m.is_active(0));
        assert!(!m.is_active(1));
        assert!(m.is_active(2));
        assert!(m.is_active(99));
    }

    #[test]
    fn effective_score_zeros_when_inactive() {
        let m = SpectralMode::Gated { min_real_catchments: 3 };
        assert_eq!(m.effective_score(0.75, 2), 0.0);
        assert_eq!(m.effective_score(0.75, 3), 0.75);
    }

    #[test]
    fn default_is_gated() {
        assert!(matches!(SpectralMode::DEFAULT, SpectralMode::Gated { .. }));
        assert!(!SpectralMode::DEFAULT.is_active(1));
        assert!(SpectralMode::DEFAULT.is_active(2));
    }

    #[test]
    fn display() {
        assert_eq!(SpectralMode::Enabled.to_string(), "enabled");
        assert_eq!(SpectralMode::Disabled.to_string(), "disabled");
        assert_eq!(SpectralMode::Gated { min_real_catchments: 2 }.to_string(), "gated(>=2_catchments)");
    }
}
