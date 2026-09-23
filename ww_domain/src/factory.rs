//! Typed factory helpers for creating domain instances.
//!
//! Each factory function constructs the raw [`ObjectInstance`] property map,
//! registers it with the engine, and (where applicable) creates the
//! corresponding link to its parent object.  Callers work with plain Rust
//! structs; the ontology layer is an implementation detail.
//!
//! ## v0.2 — Multi-target fields
//!
//! [`SignalParams`] and [`AlertParams`] carry two new fields:
//! * `analyte_category` — lowercase string tag for the [`AnalyteCategory`].
//! * `decay_rate_k`     — first-order decay constant (d⁻¹) for SHPINN.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use ontology_engine::prelude::*;

use crate::{schema::*, Result};

// ── Monotonic ID generator ───────────────────────────────────────────────────

static ID_CTR: AtomicU64 = AtomicU64::new(1);

fn next_id(prefix: &str) -> String {
    let n = ID_CTR.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{n:06}")
}

fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ── Parameter structs ────────────────────────────────────────────────────────

/// Parameters for creating a monitoring site.
pub struct SiteParams {
    pub site_id: Option<String>,
    pub name: String,
    pub region: String,
    pub catchment_pop: i64,
    pub lat: f64,
    pub lon: f64,
}

/// Parameters for a wastewater sample collection event.
pub struct SampleParams {
    pub sample_id: Option<String>,
    pub site_id: String,
    pub flow_liters: f64,
    pub qc_passed: bool,
    pub notes: String,
}

/// Parameters for a single analyte signal detected in a sample.
///
/// `pathogen` holds the analyte name (e.g., `"Fentanyl"`, `"PFAS_PFOA"`).
/// The term is kept for backward compatibility with the ontology property name.
pub struct SignalParams {
    pub signal_id: Option<String>,
    pub sample_id: String,
    /// Analyte name (pathogen, drug, or toxicant).
    pub pathogen: String,
    /// Detection target: gene, metabolite marker, compound, isotope, etc.
    pub target_gene: String,
    /// log₁₀ copies/L (or µg/L for chemicals) — flow-normalised.
    pub log10_copies_per_l: f64,
    /// Analytical method string.
    pub method: String,
    // ── v0.2 multi-target fields ─────────────────────────────────────────────
    /// Lowercase `AnalyteCategory::as_str()` tag for this signal.
    pub analyte_category: String,
    /// First-order decay rate constant k (d⁻¹) from the SHPINN PDE.
    pub decay_rate_k: f64,
}

/// Parameters for a biosurveillance alert.
pub struct AlertParams {
    pub alert_id: Option<String>,
    pub site_id: String,
    pub pathogen: String,
    /// Severity string: `"GREEN"` | `"AMBER"` | `"RED"` | `"CRITICAL"`.
    pub severity: String,
    pub signal_id: String,
    pub z_score: f64,
    pub spectral_score: f64,
    // ── v0.2 ────────────────────────────────────────────────────────────────
    pub analyte_category: String,
}

// ── Factory ──────────────────────────────────────────────────────────────────

pub struct DomainFactory;

impl DomainFactory {
    /// Create and persist a [`MonitoringSite`].  Returns the assigned `site_id`.
    pub fn create_site(engine: &OntologyEngine, p: SiteParams) -> Result<String> {
        let id = p.site_id.unwrap_or_else(|| next_id("site"));
        engine.create_object_instance(ObjectInstance::new(
            &id,
            MONITORING_SITE,
            HashMap::from([
                ("site_id".into(),       PropertyValue::String(id.clone())),
                ("name".into(),          PropertyValue::String(p.name)),
                ("region".into(),        PropertyValue::String(p.region)),
                ("catchment_pop".into(), PropertyValue::Integer(p.catchment_pop)),
                ("lat".into(),           PropertyValue::Float(p.lat)),
                ("lon".into(),           PropertyValue::Float(p.lon)),
                ("active".into(),        PropertyValue::Boolean(true)),
            ]),
        ))?;
        Ok(id)
    }

    /// Create and persist a [`WastewaterSample`], automatically linking it to
    /// its collection site.  Returns the assigned `sample_id`.
    pub fn create_sample(engine: &OntologyEngine, p: SampleParams) -> Result<String> {
        let id = p.sample_id.unwrap_or_else(|| next_id("smp"));
        engine.create_object_instance(ObjectInstance::new(
            &id,
            WASTEWATER_SAMPLE,
            HashMap::from([
                ("sample_id".into(),    PropertyValue::String(id.clone())),
                ("site_id".into(),      PropertyValue::String(p.site_id.clone())),
                ("collected_at".into(), PropertyValue::String(now_iso())),
                ("flow_liters".into(),  PropertyValue::Float(p.flow_liters)),
                ("qc_passed".into(),    PropertyValue::Boolean(p.qc_passed)),
                ("notes".into(),        PropertyValue::String(p.notes)),
            ]),
        ))?;
        engine.create_link(LinkInstance::new(SAMPLE_AT_SITE, &id, &p.site_id))?;
        Ok(id)
    }

    /// Create and persist a [`PathogenSignal`] (any analyte category),
    /// automatically linking it to its parent sample.
    /// Returns the assigned `signal_id`.
    pub fn create_signal(engine: &OntologyEngine, p: SignalParams) -> Result<String> {
        let id = p.signal_id.unwrap_or_else(|| next_id("sig"));
        engine.create_object_instance(ObjectInstance::new(
            &id,
            PATHOGEN_SIGNAL,
            HashMap::from([
                ("signal_id".into(),          PropertyValue::String(id.clone())),
                ("sample_id".into(),          PropertyValue::String(p.sample_id.clone())),
                ("pathogen".into(),           PropertyValue::String(p.pathogen)),
                ("target_gene".into(),        PropertyValue::String(p.target_gene)),
                ("log10_copies_per_l".into(), PropertyValue::Float(p.log10_copies_per_l)),
                ("method".into(),             PropertyValue::String(p.method)),
                // v0.2 fields
                ("analyte_category".into(),   PropertyValue::String(p.analyte_category)),
                ("decay_rate_k".into(),       PropertyValue::Float(p.decay_rate_k)),
            ]),
        ))?;
        engine.create_link(LinkInstance::new(SIGNAL_FROM_SAMPLE, &id, &p.sample_id))?;
        Ok(id)
    }

    /// Create and persist a [`SurveillanceAlert`], automatically linking it to
    /// its triggering signal.  Returns the assigned `alert_id`.
    pub fn create_alert(engine: &OntologyEngine, p: AlertParams) -> Result<String> {
        let id = p.alert_id.unwrap_or_else(|| next_id("alrt"));
        engine.create_object_instance(ObjectInstance::new(
            &id,
            SURVEILLANCE_ALERT,
            HashMap::from([
                ("alert_id".into(),         PropertyValue::String(id.clone())),
                ("site_id".into(),          PropertyValue::String(p.site_id)),
                ("pathogen".into(),         PropertyValue::String(p.pathogen)),
                ("analyte_category".into(), PropertyValue::String(p.analyte_category)),
                ("severity".into(),         PropertyValue::String(p.severity)),
                ("signal_id".into(),        PropertyValue::String(p.signal_id.clone())),
                ("detected_at".into(),      PropertyValue::String(now_iso())),
                ("status".into(),           PropertyValue::String("OPEN".into())),
                ("z_score".into(),          PropertyValue::Float(p.z_score)),
                ("spectral_score".into(),   PropertyValue::Float(p.spectral_score)),
                ("analyst_notes".into(),    PropertyValue::String(String::new())),
            ]),
        ))?;
        engine.create_link(LinkInstance::new(ALERT_FROM_SIGNAL, &id, &p.signal_id))?;
        Ok(id)
    }

    /// Register a directional flow relationship between two monitoring sites.
    /// `upstream_id` drains into `downstream_id`.
    pub fn add_flow_link(
        engine: &OntologyEngine,
        upstream_id: &str,
        downstream_id: &str,
    ) -> Result<()> {
        engine.create_link(LinkInstance::new(SITE_FLOWS_TO, upstream_id, downstream_id))?;
        Ok(())
    }
}
