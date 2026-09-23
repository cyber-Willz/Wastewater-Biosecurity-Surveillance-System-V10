//! `ww_audit` — Human-in-the-loop audit capabilities for wastewater
//! biosurveillance.
//!
//! Three sub-systems:
//!
//! * **`log`** — append-only, thread-safe [`AuditLog`] recording every system
//!   event (ingestion, detection, review decisions).  Exportable as
//!   newline-delimited JSON for regulatory submission.
//!
//! * **`review`** — [`ReviewSession`] gives an analyst the ability to
//!   confirm, dismiss, or escalate open alerts.  Every decision is written
//!   back to the ontology engine (updating `status` and `analyst_notes`) and
//!   appended to the audit log in the same operation.
//!
//! * **`report`** — [`ReportGenerator`] builds [`EvidenceChain`]s (full
//!   provenance from alert → signal → sample → site) and [`DailySummary`]
//!   aggregates for human-readable terminal output or downstream reporting.

pub mod log;
pub mod report;
pub mod review;

pub use log::{AuditAction, AuditEntry, AuditLog};
pub use report::{DailySummary, EvidenceChain, ReportGenerator};
pub use review::{ReviewOutcome, ReviewSession};
