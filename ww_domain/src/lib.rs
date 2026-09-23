//! `ww_domain` — Domain schema, typed factory helpers, and typed query wrappers
//! for the wastewater biosurveillance ontology.
//!
//! All persistent state lives in an [`ontology_engine::engine::OntologyEngine`]
//! that is shared (behind `Arc`) across every crate.  This layer gives the
//! rest of the system strongly-typed domain structs rather than raw
//! [`ontology_engine::types::ObjectInstance`] maps.
//!
//! ## v0.2 — Multi-target surveillance
//!
//! The new `analyte` module defines the [`AnalyteCategory`] enum and the
//! [`AnalyteProfile`] struct (including the full 18-analyte [`ANALYTE_CATALOG`]).
//! Every signal and alert now stores an `analyte_category` property, enabling
//! category-filtered queries and SHPINN downstream regression.

pub mod analyte;
pub mod factory;
pub mod query;
pub mod schema;

pub use analyte::{AnalyteCategory, AnalyteProfile, ANALYTE_CATALOG,
                  analyte_by_name, analytes_by_category};
pub use factory::{AlertParams, DomainFactory, SampleParams, SignalParams, SiteParams};
pub use query::{
    AlertStatus, MonitoringSite, PathogenSignal, Severity, SurveillanceAlert,
    WastewaterSample, list_alerts_by_category, list_signals_by_category,
};
pub use schema::register_biosec_schema;

use thiserror::Error;

/// Unified error type for domain-layer operations.
#[derive(Debug, Error)]
pub enum DomainError {
    #[error("ontology error: {0}")]
    Ontology(#[from] ontology_engine::error::OntologyError),

    #[error("missing property '{0}' on instance '{1}'")]
    MissingProperty(String, String),

    #[error("type conversion error: {0}")]
    Conversion(String),
}

pub type Result<T> = std::result::Result<T, DomainError>;
