//! Analyst review workflow: confirm, dismiss, or escalate open alerts.
//!
//! Every [`ReviewSession::review`] call atomically:
//! 1. Updates the alert's `status` and `analyst_notes` in the ontology engine.
//! 2. Appends a timestamped record to the [`AuditLog`] (not cryptographically signed).
//!
//! The session does not own the engine; it borrows it for the duration of the
//! review.  Multiple sessions may review different alerts concurrently (the
//! engine's internal lock guards individual instance updates).

use std::collections::HashMap;

use ontology_engine::prelude::*;

use ww_domain::query::{list_open_alerts, SurveillanceAlert};

use crate::log::{AuditAction, AuditLog};

/// The action an analyst takes on a single open alert.
#[derive(Debug, Clone)]
pub enum ReviewOutcome {
    /// The signal is clinically credible; notify public-health authorities.
    Confirm { notes: String },
    /// The signal is a lab artefact, duplicate, or within known baseline.
    Dismiss { reason: String },
    /// The signal needs additional laboratory or epidemiological intelligence
    /// before a final classification.
    Escalate { to_team: String, notes: String },
}

/// A single analyst review session.
///
/// Create one per review round; it captures the analyst's identity and provides
/// a typed interface over the raw ontology update.
pub struct ReviewSession<'e> {
    engine:  &'e OntologyEngine,
    audit:   &'e AuditLog,
    analyst: String,
}

impl<'e> ReviewSession<'e> {
    pub fn new(engine: &'e OntologyEngine, audit: &'e AuditLog, analyst: &str) -> Self {
        Self {
            engine,
            audit,
            analyst: analyst.to_string(),
        }
    }

    /// All alerts awaiting a review decision, sorted critical-first.
    pub fn pending_alerts(&self) -> Vec<SurveillanceAlert> {
        list_open_alerts(self.engine)
    }

    /// Apply `outcome` to the alert identified by `alert_id`.
    ///
    /// Writes back to the ontology engine (status + analyst_notes) and appends
    /// to the audit log in the same call.
    pub fn review(
        &self,
        alert_id: &str,
        outcome:  ReviewOutcome,
    ) -> ontology_engine::error::Result<()> {
        let (new_status, notes, action) = match &outcome {
            ReviewOutcome::Confirm { notes } => (
                "CONFIRMED",
                notes.clone(),
                AuditAction::AlertConfirmed,
            ),
            ReviewOutcome::Dismiss { reason } => (
                "DISMISSED",
                reason.clone(),
                AuditAction::AlertDismissed,
            ),
            ReviewOutcome::Escalate { to_team, notes } => (
                "ESCALATED",
                format!("[→ {}] {}", to_team, notes),
                AuditAction::AlertEscalated,
            ),
        };

        self.engine.update_object_properties(
            alert_id,
            HashMap::from([
                ("status".into(),        PropertyValue::String(new_status.into())),
                ("analyst_notes".into(), PropertyValue::String(notes.clone())),
            ]),
        )?;

        self.audit.record(&self.analyst, action, alert_id, &notes);

        Ok(())
    }
}
