//! Domain-wide ontology schema.
//!
//! Object types and link types are registered once at startup via
//! [`register_biosec_schema`].  All name constants are exported so other
//! crates can address the schema without hard-coding strings.
//!
//! ## Multi-target extensions (v0.2)
//!
//! `PathogenSignal` now carries two additional properties:
//!
//! * `analyte_category` — the [`AnalyteCategory`] tag as a lowercase string
//!   (e.g., `"illicit_substance"`), enabling category-filtered queries and
//!   dashboard grouping.
//! * `decay_rate_k` — the analyte's first-order decay constant (d⁻¹) from
//!   the SHPINN transport PDE.  Downstream regression pipelines read this
//!   directly from the ontology rather than re-deriving it from the name.

use ontology_engine::prelude::*;
use crate::Result;

// ── Object-type name constants ───────────────────────────────────────────────

/// A physical wastewater sampling point in the sewage network.
pub const MONITORING_SITE: &str = "MonitoringSite";
/// A grab- or composite-sample collected at a monitoring site.
pub const WASTEWATER_SAMPLE: &str = "WastewaterSample";
/// A single analyte signal detected in a sample (pathogen, drug, or toxicant).
pub const PATHOGEN_SIGNAL: &str = "PathogenSignal";
/// A machine-generated or analyst-escalated biosurveillance alert.
pub const SURVEILLANCE_ALERT: &str = "SurveillanceAlert";
/// An audit record written by any system actor (append-only by API).
pub const AUDIT_RECORD: &str = "AuditRecord";

// ── Link-type name constants ─────────────────────────────────────────────────

/// WastewaterSample → MonitoringSite  (where the sample was collected).
pub const SAMPLE_AT_SITE: &str = "sample_at_site";
/// PathogenSignal → WastewaterSample  (which sample the signal came from).
pub const SIGNAL_FROM_SAMPLE: &str = "signal_from_sample";
/// SurveillanceAlert → PathogenSignal  (which signal triggered the alert).
pub const ALERT_FROM_SIGNAL: &str = "alert_from_signal";
/// MonitoringSite → MonitoringSite  (upstream site drains into downstream site).
pub const SITE_FLOWS_TO: &str = "site_flows_to";

// ── Schema registration ──────────────────────────────────────────────────────

/// Register every object type and link type required by the biosurveillance
/// system.  Must be called once before any factory or query function is used.
pub fn register_biosec_schema(engine: &OntologyEngine) -> Result<()> {
    // ── MonitoringSite ───────────────────────────────────────────────────────
    let site_type = ObjectTypeBuilder::new(MONITORING_SITE)
        .primary_key("site_id")
        .property("site_id",       PropertyType::String)
        .property("name",          PropertyType::String)
        .property("region",        PropertyType::String)
        .property("catchment_pop", PropertyType::Integer)
        .property("lat",           PropertyType::Float)
        .property("lon",           PropertyType::Float)
        .property("active",        PropertyType::Boolean)
        .build()
        .map_err(|e| ontology_engine::error::OntologyError::EmptyPrimaryKey { name: e })?;

    // ── WastewaterSample ─────────────────────────────────────────────────────
    let sample_type = ObjectTypeBuilder::new(WASTEWATER_SAMPLE)
        .primary_key("sample_id")
        .property("sample_id",    PropertyType::String)
        .property("site_id",      PropertyType::String)
        .property("collected_at", PropertyType::String)
        .property("flow_liters",  PropertyType::Float)
        .property("qc_passed",    PropertyType::Boolean)
        .property("notes",        PropertyType::String)
        .build()
        .map_err(|e| ontology_engine::error::OntologyError::EmptyPrimaryKey { name: e })?;

    // ── PathogenSignal ───────────────────────────────────────────────────────
    // v0.2: two new properties support multi-target surveillance:
    //   analyte_category — categorical tag for dashboard grouping + queries
    //   decay_rate_k     — k parameter for SHPINN PDE regression
    let signal_type = ObjectTypeBuilder::new(PATHOGEN_SIGNAL)
        .primary_key("signal_id")
        .property("signal_id",          PropertyType::String)
        .property("sample_id",          PropertyType::String)
        .property("pathogen",           PropertyType::String)   // analyte name
        .property("target_gene",        PropertyType::String)   // target_marker
        .property("log10_copies_per_l", PropertyType::Float)
        .property("method",             PropertyType::String)
        // ── NEW v0.2 ─────────────────────────────────────────────────────────
        .property("analyte_category",   PropertyType::String)   // AnalyteCategory::as_str()
        .property("decay_rate_k",       PropertyType::Float)    // k (d⁻¹) for SHPINN
        .build()
        .map_err(|e| ontology_engine::error::OntologyError::EmptyPrimaryKey { name: e })?;

    // ── SurveillanceAlert ────────────────────────────────────────────────────
    let alert_type = ObjectTypeBuilder::new(SURVEILLANCE_ALERT)
        .primary_key("alert_id")
        .property("alert_id",         PropertyType::String)
        .property("site_id",          PropertyType::String)
        .property("pathogen",         PropertyType::String)
        .property("analyte_category", PropertyType::String)   // NEW v0.2
        .property("severity",         PropertyType::String)
        .property("signal_id",        PropertyType::String)
        .property("detected_at",      PropertyType::String)
        .property("status",           PropertyType::String)
        .property("z_score",          PropertyType::Float)
        .property("spectral_score",   PropertyType::Float)
        .property("analyst_notes",    PropertyType::String)
        .build()
        .map_err(|e| ontology_engine::error::OntologyError::EmptyPrimaryKey { name: e })?;

    // ── AuditRecord ──────────────────────────────────────────────────────────
    let audit_type = ObjectTypeBuilder::new(AUDIT_RECORD)
        .primary_key("audit_id")
        .property("audit_id",  PropertyType::String)
        .property("timestamp", PropertyType::String)
        .property("actor",     PropertyType::String)
        .property("action",    PropertyType::String)
        .property("target_id", PropertyType::String)
        .property("details",   PropertyType::String)
        .build()
        .map_err(|e| ontology_engine::error::OntologyError::EmptyPrimaryKey { name: e })?;

    engine.register_object_type(site_type)?;
    engine.register_object_type(sample_type)?;
    engine.register_object_type(signal_type)?;
    engine.register_object_type(alert_type)?;
    engine.register_object_type(audit_type)?;

    // ── Link types ───────────────────────────────────────────────────────────
    engine.register_link_type(LinkType::new(SAMPLE_AT_SITE,    WASTEWATER_SAMPLE,  MONITORING_SITE))?;
    engine.register_link_type(LinkType::new(SIGNAL_FROM_SAMPLE, PATHOGEN_SIGNAL,   WASTEWATER_SAMPLE))?;
    engine.register_link_type(LinkType::new(ALERT_FROM_SIGNAL,  SURVEILLANCE_ALERT, PATHOGEN_SIGNAL))?;
    engine.register_link_type(LinkType::new(SITE_FLOWS_TO,      MONITORING_SITE,   MONITORING_SITE))?;

    Ok(())
}
