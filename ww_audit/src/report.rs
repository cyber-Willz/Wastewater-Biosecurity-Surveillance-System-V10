//! Reporting layer: evidence chains and daily aggregate summaries.
//!
//! [`EvidenceChain`] reconstructs the full provenance path for a single alert:
//!
//! ```text
//! SurveillanceAlert
//!   └─► PathogenSignal  (via alert_from_signal)
//!         └─► WastewaterSample  (via signal_from_sample)
//!               └─► MonitoringSite  (via sample_at_site)
//! ```
//!
//! [`DailySummary`] aggregates counts across the whole instance store for
//! operational dashboards and regulatory filings.

use std::collections::HashMap;

use ontology_engine::prelude::*;

use ww_domain::{
    query::{
        list_all_alerts, list_sites,
        signal_for_alert, sample_for_signal, site_for_sample,
        AlertStatus, MonitoringSite, PathogenSignal, SurveillanceAlert, WastewaterSample,
    },
    schema::{WASTEWATER_SAMPLE, PATHOGEN_SIGNAL},
};

use crate::log::AuditLog;

// ── Evidence chain ────────────────────────────────────────────────────────────

/// Full provenance chain from a [`SurveillanceAlert`] back to its physical
/// collection site, including every audit-trail entry related to the alert.
#[derive(Debug)]
pub struct EvidenceChain {
    pub alert:       SurveillanceAlert,
    pub signal:      Option<PathogenSignal>,
    pub sample:      Option<WastewaterSample>,
    pub site:        Option<MonitoringSite>,
    /// Audit-trail entries for `alert.alert_id`, formatted for display.
    pub audit_trail: Vec<String>,
}

impl EvidenceChain {
    /// Build a chain for `alert`, walking the graph links and pulling audit
    /// records from `audit_log`.
    pub fn build(
        engine:    &OntologyEngine,
        alert:     SurveillanceAlert,
        audit_log: &AuditLog,
    ) -> Self {
        let signal      = signal_for_alert(engine, &alert);
        let sample      = signal.as_ref().and_then(|s| sample_for_signal(engine, s));
        let site        = sample.as_ref().and_then(|s| site_for_sample(engine, s));
        let audit_trail = audit_log
            .entries_for(&alert.alert_id)
            .into_iter()
            .map(|e| {
                format!(
                    "[{}] {} by {} — {}",
                    e.timestamp, e.action, e.actor, e.details
                )
            })
            .collect();

        Self { alert, signal, sample, site, audit_trail }
    }

    /// Render as a human-readable block suitable for terminal output.
    pub fn render(&self) -> String {
        let a = &self.alert;
        let mut out = String::new();

        out.push_str(&format!(
            "╔══ EVIDENCE CHAIN: {} ══╗\n",
            a.alert_id
        ));
        out.push_str(&format!(
            "  Alert      : {}  severity={}  status={}\n",
            a.alert_id,
            a.severity.as_str(),
            a.status.as_str(),
        ));
        out.push_str(&format!(
            "  Pathogen   : {}   detected {}\n",
            a.pathogen, a.detected_at
        ));
        out.push_str(&format!(
            "  z-score    : {:.2}   spectral score: {:.3}\n",
            a.z_score, a.spectral_score
        ));

        if let Some(sig) = &self.signal {
            out.push_str(&format!(
                "  Signal     : {}  target={}  {:.2} log₁₀ copies/L  method={}\n",
                sig.signal_id, sig.target_gene, sig.log10_copies_per_l, sig.method
            ));
        }
        if let Some(smp) = &self.sample {
            out.push_str(&format!(
                "  Sample     : {}  collected={}  flow={:.0} L  QC={}\n",
                smp.sample_id,
                smp.collected_at,
                smp.flow_liters,
                if smp.qc_passed { "PASS" } else { "FAIL" }
            ));
        }
        if let Some(site) = &self.site {
            out.push_str(&format!(
                "  Site       : {} ({})  region={}  catchment pop={}\n",
                site.name, site.site_id, site.region, site.catchment_pop
            ));
        }

        if !a.analyst_notes.is_empty() {
            out.push_str(&format!("  Notes      : {}\n", a.analyst_notes));
        }

        if !self.audit_trail.is_empty() {
            out.push_str("  Audit trail:\n");
            for line in &self.audit_trail {
                out.push_str(&format!("    {}\n", line));
            }
        }
        out.push_str("╚══════════════════════════════╝\n");
        out
    }
}

// ── Daily summary ─────────────────────────────────────────────────────────────

/// Aggregate counts across the instance store, suitable for a shift-handover
/// briefing or regulatory report.
#[derive(Debug, Default)]
pub struct DailySummary {
    pub total_samples:      usize,
    pub total_signals:      usize,
    pub open_alerts:        usize,
    pub alerts_by_severity: HashMap<String, usize>,
    pub alerts_by_pathogen: HashMap<String, usize>,
    pub alerts_by_region:   HashMap<String, usize>,
}

// ── Report generator ──────────────────────────────────────────────────────────

/// Thin façade that combines the engine and audit log into formatted reports.
pub struct ReportGenerator<'e> {
    engine: &'e OntologyEngine,
    audit:  &'e AuditLog,
}

impl<'e> ReportGenerator<'e> {
    pub fn new(engine: &'e OntologyEngine, audit: &'e AuditLog) -> Self {
        Self { engine, audit }
    }

    /// Compute the daily summary.
    pub fn daily_summary(&self) -> DailySummary {
        let mut s = DailySummary::default();

        s.total_samples = self.engine.list_instances_by_type(WASTEWATER_SAMPLE).len();
        s.total_signals = self.engine.list_instances_by_type(PATHOGEN_SIGNAL).len();

        // Build site_id → region lookup
        let regions: HashMap<String, String> = list_sites(self.engine)
            .into_iter()
            .map(|site| (site.site_id, site.region))
            .collect();

        for alert in list_all_alerts(self.engine) {
            *s.alerts_by_severity
                .entry(alert.severity.as_str().to_string())
                .or_insert(0) += 1;
            *s.alerts_by_pathogen
                .entry(alert.pathogen.clone())
                .or_insert(0) += 1;
            if let Some(region) = regions.get(&alert.site_id) {
                *s.alerts_by_region
                    .entry(region.clone())
                    .or_insert(0) += 1;
            }
            if alert.status == AlertStatus::Open || alert.status == AlertStatus::Escalated {
                s.open_alerts += 1;
            }
        }
        s
    }

    /// Build a full evidence chain for an alert.
    pub fn evidence_chain(&self, alert: SurveillanceAlert) -> EvidenceChain {
        EvidenceChain::build(self.engine, alert, self.audit)
    }

    /// Render the daily summary as a formatted string.
    pub fn render_summary(&self) -> String {
        let s = self.daily_summary();
        let mut out = String::new();

        out.push_str("╔═════════════════════════════════════════╗\n");
        out.push_str("║   WASTEWATER BIOSURVEILLANCE SUMMARY    ║\n");
        out.push_str("╚═════════════════════════════════════════╝\n");
        out.push_str(&format!("  Samples processed  : {}\n", s.total_samples));
        out.push_str(&format!("  Pathogen signals   : {}\n", s.total_signals));
        out.push_str(&format!("  Open / escalated   : {}\n\n", s.open_alerts));

        out.push_str("  ── Alerts by Severity ──────────────────\n");
        for sev in &["CRITICAL", "RED", "AMBER", "GREEN"] {
            let n   = s.alerts_by_severity.get(*sev).copied().unwrap_or(0);
            let bar = "█".repeat(n.min(30));
            out.push_str(&format!("  {:8}  {:30}  {}\n", sev, bar, n));
        }

        if !s.alerts_by_pathogen.is_empty() {
            out.push_str("\n  ── Alerts by Pathogen ──────────────────\n");
            let mut rows: Vec<_> = s.alerts_by_pathogen.iter().collect();
            rows.sort_by(|a, b| b.1.cmp(a.1));
            for (p, n) in rows {
                out.push_str(&format!("  {:<28}  {}\n", p, n));
            }
        }

        if !s.alerts_by_region.is_empty() {
            out.push_str("\n  ── Alerts by Region ────────────────────\n");
            let mut rows: Vec<_> = s.alerts_by_region.iter().collect();
            rows.sort_by(|a, b| b.1.cmp(a.1));
            for (r, n) in rows {
                out.push_str(&format!("  {:<28}  {}\n", r, n));
            }
        }

        out.push_str(&format!("\n  Audit log entries  : {}\n", self.audit.len()));
        out
    }
}
