//! Typed query wrappers over the raw [`OntologyEngine`] API.
//!
//! Every public struct here is a strongly-typed *view* over an
//! [`ObjectInstance`].  The conversion is infallible at the type level
//! (instances with wrong schemas are filtered out rather than propagated as
//! errors) because the engine's schema validation guarantees that every
//! successfully-created instance is schema-conformant.
//!
//! ## v0.2 — Multi-target query additions
//!
//! * [`PathogenSignal`] carries `analyte_category` and `decay_rate_k`.
//! * [`SurveillanceAlert`] carries `analyte_category`.
//! * [`list_alerts_by_category`] enables category-filtered dashboards.
//! * [`list_signals_by_category`] supports per-category trend analysis.

use ontology_engine::prelude::*;
use crate::{analyte::AnalyteCategory, DomainError, Result, schema::*};

// ── Value extraction helpers ─────────────────────────────────────────────────

fn str_prop(inst: &ObjectInstance, prop: &str) -> Result<String> {
    match inst.properties.get(prop) {
        Some(PropertyValue::String(s)) => Ok(s.clone()),
        _ => Err(DomainError::MissingProperty(prop.into(), inst.id.clone())),
    }
}

fn int_prop(inst: &ObjectInstance, prop: &str) -> Result<i64> {
    match inst.properties.get(prop) {
        Some(PropertyValue::Integer(i)) => Ok(*i),
        _ => Err(DomainError::MissingProperty(prop.into(), inst.id.clone())),
    }
}

fn float_prop(inst: &ObjectInstance, prop: &str) -> Result<f64> {
    match inst.properties.get(prop) {
        Some(PropertyValue::Float(f)) => Ok(*f),
        _ => Err(DomainError::MissingProperty(prop.into(), inst.id.clone())),
    }
}

fn bool_prop(inst: &ObjectInstance, prop: &str) -> Result<bool> {
    match inst.properties.get(prop) {
        Some(PropertyValue::Boolean(b)) => Ok(*b),
        _ => Err(DomainError::MissingProperty(prop.into(), inst.id.clone())),
    }
}

// ── Domain types ─────────────────────────────────────────────────────────────

/// Alert severity tier — mirrors the `severity` string stored in the ontology.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Green,
    Amber,
    Red,
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Green    => "GREEN",
            Severity::Amber    => "AMBER",
            Severity::Red      => "RED",
            Severity::Critical => "CRITICAL",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "AMBER"    => Severity::Amber,
            "RED"      => Severity::Red,
            "CRITICAL" => Severity::Critical,
            _          => Severity::Green,
        }
    }

    /// Traffic-light emoji for terminal output.
    pub fn emoji(&self) -> &'static str {
        match self {
            Severity::Green    => "🟢",
            Severity::Amber    => "🟡",
            Severity::Red      => "🔴",
            Severity::Critical => "🚨",
        }
    }
}

/// Lifecycle state of a [`SurveillanceAlert`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertStatus {
    Open,
    Confirmed,
    Dismissed,
    Escalated,
}

impl AlertStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertStatus::Open      => "OPEN",
            AlertStatus::Confirmed => "CONFIRMED",
            AlertStatus::Dismissed => "DISMISSED",
            AlertStatus::Escalated => "ESCALATED",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "CONFIRMED" => AlertStatus::Confirmed,
            "DISMISSED" => AlertStatus::Dismissed,
            "ESCALATED" => AlertStatus::Escalated,
            _           => AlertStatus::Open,
        }
    }
}

// ── View structs ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MonitoringSite {
    pub site_id:       String,
    pub name:          String,
    pub region:        String,
    pub catchment_pop: i64,
    pub lat:           f64,
    pub lon:           f64,
    pub active:        bool,
}

#[derive(Debug, Clone)]
pub struct WastewaterSample {
    pub sample_id:    String,
    pub site_id:      String,
    pub collected_at: String,
    pub flow_liters:  f64,
    pub qc_passed:    bool,
    pub notes:        String,
}

/// A single analyte measurement in a sample.
///
/// The `pathogen` field holds the analyte name regardless of category
/// (preserved for ontology backward-compat).  Use `analyte_category` to
/// distinguish pathogens, drugs, pharmaceuticals, and toxicants.
#[derive(Debug, Clone)]
pub struct PathogenSignal {
    pub signal_id:          String,
    pub sample_id:          String,
    /// Analyte name (e.g., `"SARS-CoV-2"`, `"Fentanyl"`, `"PFAS_PFOA"`).
    pub pathogen:           String,
    /// Detection target marker (gene, metabolite, compound, isotope).
    pub target_gene:        String,
    /// log₁₀ concentration (copies/L or µg/L), flow-normalised.
    pub log10_copies_per_l: f64,
    pub method:             String,
    // ── v0.2 ────────────────────────────────────────────────────────────────
    /// Surveillance category for this analyte.
    pub analyte_category:   AnalyteCategory,
    /// First-order decay rate k (d⁻¹) for SHPINN regression.
    pub decay_rate_k:       f64,
}

#[derive(Debug, Clone)]
pub struct SurveillanceAlert {
    pub alert_id:         String,
    pub site_id:          String,
    pub pathogen:         String,
    pub severity:         Severity,
    pub signal_id:        String,
    pub detected_at:      String,
    pub status:           AlertStatus,
    pub z_score:          f64,
    pub spectral_score:   f64,
    pub analyst_notes:    String,
    // ── v0.2 ────────────────────────────────────────────────────────────────
    pub analyte_category: AnalyteCategory,
}

// ── TryFrom conversions ──────────────────────────────────────────────────────

impl TryFrom<ObjectInstance> for MonitoringSite {
    type Error = DomainError;
    fn try_from(i: ObjectInstance) -> Result<Self> {
        Ok(Self {
            site_id:       str_prop(&i, "site_id")?,
            name:          str_prop(&i, "name")?,
            region:        str_prop(&i, "region")?,
            catchment_pop: int_prop(&i, "catchment_pop")?,
            lat:           float_prop(&i, "lat")?,
            lon:           float_prop(&i, "lon")?,
            active:        bool_prop(&i, "active")?,
        })
    }
}

impl TryFrom<ObjectInstance> for WastewaterSample {
    type Error = DomainError;
    fn try_from(i: ObjectInstance) -> Result<Self> {
        Ok(Self {
            sample_id:    str_prop(&i, "sample_id")?,
            site_id:      str_prop(&i, "site_id")?,
            collected_at: str_prop(&i, "collected_at")?,
            flow_liters:  float_prop(&i, "flow_liters")?,
            qc_passed:    bool_prop(&i, "qc_passed")?,
            notes:        str_prop(&i, "notes")?,
        })
    }
}

impl TryFrom<ObjectInstance> for PathogenSignal {
    type Error = DomainError;
    fn try_from(i: ObjectInstance) -> Result<Self> {
        Ok(Self {
            signal_id:          str_prop(&i, "signal_id")?,
            sample_id:          str_prop(&i, "sample_id")?,
            pathogen:           str_prop(&i, "pathogen")?,
            target_gene:        str_prop(&i, "target_gene")?,
            log10_copies_per_l: float_prop(&i, "log10_copies_per_l")?,
            method:             str_prop(&i, "method")?,
            analyte_category:   AnalyteCategory::from_str(
                                    &str_prop(&i, "analyte_category")
                                    .unwrap_or_default()
                                ),
            decay_rate_k:       float_prop(&i, "decay_rate_k").unwrap_or(0.0),
        })
    }
}

impl TryFrom<ObjectInstance> for SurveillanceAlert {
    type Error = DomainError;
    fn try_from(i: ObjectInstance) -> Result<Self> {
        Ok(Self {
            alert_id:         str_prop(&i, "alert_id")?,
            site_id:          str_prop(&i, "site_id")?,
            pathogen:         str_prop(&i, "pathogen")?,
            severity:         Severity::from_str(&str_prop(&i, "severity")?),
            signal_id:        str_prop(&i, "signal_id")?,
            detected_at:      str_prop(&i, "detected_at")?,
            status:           AlertStatus::from_str(&str_prop(&i, "status")?),
            z_score:          float_prop(&i, "z_score")?,
            spectral_score:   float_prop(&i, "spectral_score")?,
            analyst_notes:    str_prop(&i, "analyst_notes")?,
            analyte_category: AnalyteCategory::from_str(
                                  &str_prop(&i, "analyte_category")
                                  .unwrap_or_default()
                              ),
        })
    }
}

// ── Query functions ──────────────────────────────────────────────────────────

pub fn list_sites(engine: &OntologyEngine) -> Vec<MonitoringSite> {
    engine
        .list_instances_by_type(MONITORING_SITE)
        .into_iter()
        .filter_map(|i| MonitoringSite::try_from(i).ok())
        .collect()
}

pub fn list_samples(engine: &OntologyEngine) -> Vec<WastewaterSample> {
    engine
        .list_instances_by_type(WASTEWATER_SAMPLE)
        .into_iter()
        .filter_map(|i| WastewaterSample::try_from(i).ok())
        .collect()
}

pub fn list_signals(engine: &OntologyEngine) -> Vec<PathogenSignal> {
    engine
        .list_instances_by_type(PATHOGEN_SIGNAL)
        .into_iter()
        .filter_map(|i| PathogenSignal::try_from(i).ok())
        .collect()
}

/// All signals belonging to a specific analyte category.
///
/// Use this for category-specific trend dashboards (e.g., illicit-substance
/// time-series, industrial toxicant heat-maps).
pub fn list_signals_by_category(
    engine: &OntologyEngine,
    category: &AnalyteCategory,
) -> Vec<PathogenSignal> {
    list_signals(engine)
        .into_iter()
        .filter(|s| &s.analyte_category == category)
        .collect()
}

pub fn list_all_alerts(engine: &OntologyEngine) -> Vec<SurveillanceAlert> {
    engine
        .list_instances_by_type(SURVEILLANCE_ALERT)
        .into_iter()
        .filter_map(|i| SurveillanceAlert::try_from(i).ok())
        .collect()
}

/// Only alerts whose status is `OPEN` or `ESCALATED`, sorted critical-first.
pub fn list_open_alerts(engine: &OntologyEngine) -> Vec<SurveillanceAlert> {
    let mut v: Vec<SurveillanceAlert> = engine
        .list_instances_by_type(SURVEILLANCE_ALERT)
        .into_iter()
        .filter_map(|i| SurveillanceAlert::try_from(i).ok())
        .filter(|a| {
            a.status == AlertStatus::Open || a.status == AlertStatus::Escalated
        })
        .collect();
    v.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.detected_at.cmp(&b.detected_at)));
    v
}

/// Open alerts filtered to a specific analyte category.
pub fn list_alerts_by_category(
    engine: &OntologyEngine,
    category: &AnalyteCategory,
) -> Vec<SurveillanceAlert> {
    list_open_alerts(engine)
        .into_iter()
        .filter(|a| &a.analyte_category == category)
        .collect()
}

// ── Graph traversal helpers ──────────────────────────────────────────────────

/// Resolve the [`PathogenSignal`] that triggered `alert`.
pub fn signal_for_alert(
    engine: &OntologyEngine,
    alert: &SurveillanceAlert,
) -> Option<PathogenSignal> {
    engine
        .traverse(&alert.alert_id, ALERT_FROM_SIGNAL, Direction::Outgoing)
        .into_iter()
        .next()
        .and_then(|i| PathogenSignal::try_from(i).ok())
}

/// Resolve the [`WastewaterSample`] that produced `signal`.
pub fn sample_for_signal(
    engine: &OntologyEngine,
    signal: &PathogenSignal,
) -> Option<WastewaterSample> {
    engine
        .traverse(&signal.signal_id, SIGNAL_FROM_SAMPLE, Direction::Outgoing)
        .into_iter()
        .next()
        .and_then(|i| WastewaterSample::try_from(i).ok())
}

/// Resolve the [`MonitoringSite`] where `sample` was collected.
pub fn site_for_sample(
    engine: &OntologyEngine,
    sample: &WastewaterSample,
) -> Option<MonitoringSite> {
    engine
        .traverse(&sample.sample_id, SAMPLE_AT_SITE, Direction::Outgoing)
        .into_iter()
        .next()
        .and_then(|i| MonitoringSite::try_from(i).ok())
}

/// Sites immediately downstream of `site_id` in the sewer network.
pub fn downstream_sites(engine: &OntologyEngine, site_id: &str) -> Vec<MonitoringSite> {
    engine
        .traverse(site_id, SITE_FLOWS_TO, Direction::Outgoing)
        .into_iter()
        .filter_map(|i| MonitoringSite::try_from(i).ok())
        .collect()
}

/// All samples collected at `site_id`.
pub fn samples_at_site(engine: &OntologyEngine, site_id: &str) -> Vec<WastewaterSample> {
    engine
        .traverse(site_id, SAMPLE_AT_SITE, Direction::Incoming)
        .into_iter()
        .filter_map(|i| WastewaterSample::try_from(i).ok())
        .collect()
}
