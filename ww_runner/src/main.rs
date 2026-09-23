//! `ww_biosec` — Wastewater Biosecurity Surveillance System (Multi-Target)
//!
//! Entry point for the Belize national wastewater-epidemiology simulation.
//! Demonstrates the full pipeline across a 18-analyte × 8-site × 15-day run:
//!
//! 1. Schema registration  (`ww_domain`)
//! 2. Site and flow-network setup
//! 3. Multi-target simulation — EWMA + spectral anomaly detection (`ww_detection`)
//! 4. **AI alert-triage agent** (`ww_alert_agent`) — autonomous first-pass review
//! 5. Analyst review session (`ww_audit`) — human confirmation / override
//! 6. Evidence-chain reporting, per-category summary, and audit tail

mod scenario;

use ontology_engine::prelude::OntologyEngine;

use ww_alert_agent::{AgentConfig, AlertAgent};
use ww_audit::{AuditAction, AuditLog, ReportGenerator, ReviewOutcome, ReviewSession};
use ww_detection::AnomalyDetector;
use ww_domain::{
    AnalyteCategory,
    query::{list_all_alerts, list_open_alerts},
    register_biosec_schema,
};

fn main() {
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║  WASTEWATER BIOSECURITY SURVEILLANCE SYSTEM  v0.3               ║");
    println!("║  Multi-Target Panel  •  Island Carib  •  Belize National Network ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();
    println!("  Analyte categories:");
    println!("    [PATH] Infectious pathogens   — temperature-dependent viral decay");
    println!("    [PHRM] Pharmaceutical / AMR   — chemical half-life & hydrolysis");
    println!("    [ILCT] Illicit substances      — metabolite stability kinetics");
    println!("    [INDX] Industrial toxicants    — near-zero / adsorption decay");
    println!("    [EXPX] Explosive precursors    — energetics / transformation products");
    println!();

    // ── 1. Initialise shared state ────────────────────────────────────────────
    let engine = OntologyEngine::new();
    let audit  = AuditLog::new();

    // ── 2. Register domain schema ─────────────────────────────────────────────
    print!("▶ Registering biosurveillance schema (v0.2 + analyte_category, decay_rate_k)... ");
    register_biosec_schema(&engine).expect("schema registration failed");
    audit.record("system", AuditAction::SchemaRegistered, "ww_biosec",
        "5 object types, 4 link types, 5-category multi-target analyte schema (+ explosive_precursor)");
    println!("done.\n");

    // ── 3. Build detector and run simulation ──────────────────────────────────
    let mut detector = AnomalyDetector::new();

    println!("▶ Building Belize sewage network and running 15-day multi-target simulation...\n");
    let _network = scenario::run_belize(&engine, &audit, &mut detector)
        .expect("simulation failed");

    // ── 3b. SHPINN transport check (per-category decay constants) ─────────────
    println!("\n▶ SHPINN transport check (linear-feature PINN vs analytic ADR solution):\n");
    for cat in [
        AnalyteCategory::InfectiousPathogen,
        AnalyteCategory::PharmaceuticalAMR,
        AnalyteCategory::IllicitSubstance,
        AnalyteCategory::IndustrialToxicant,
        AnalyteCategory::ExplosivePrecursor,
    ] {
        if let Some(a) = ww_domain::analytes_by_category(&cat).next() {
            let adr = ww_shpinn::Adr { v: 1.0, d: 0.05, k: a.decay_rate_k, length: 6.0, horizon: 1.5 };
            let reference = |x: f64, t: f64| adr.gaussian_reference(1.5, 0.5, x, t);
            let cfg = ww_shpinn::PielmConfig::for_adr(&adr, 0.5);
            match ww_shpinn::solve_forward(
                &adr, &|_, _| 0.0, &|x| reference(x, 0.0),
                &|t| reference(0.0, t), &|t| reference(6.0, t), &[], &cfg,
            ) {
                Ok(sol) => println!(
                    "    {:22} k={:.3}  κ={:.3}  max|c_θ−c|={:.4}",
                    a.name, a.decay_rate_k, adr.kappa(),
                    sol.max_abs_error(&reference, 60, 30),
                ),
                Err(e) => println!("    {:22} SHPINN solve failed: {e}", a.name),
            }
        }
    }

    // ── 4. AI ALERT-TRIAGE AGENT ──────────────────────────────────────────────
    println!("\n══════════════════════ AI ALERT-TRIAGE AGENT ═══════════════════════\n");

    let agent_config = AgentConfig::from_env();
    let is_dry_run   = agent_config.api_key.is_none();

    if is_dry_run {
        println!("  ℹ  ANTHROPIC_API_KEY not set — agent running in dry-run mode.");
        println!("     Set the env var to enable live AI triage.\n");
    } else {
        println!("  ✓  Anthropic API key present — live AI triage enabled.\n");
    }

    let agent         = AlertAgent::new(agent_config);
    let pre_triage    = list_open_alerts(&engine);

    println!("  {} open alert(s) queued for AI triage.\n", pre_triage.len());
    audit.record("ai_agent", AuditAction::ManualAnnotation, "ww_biosec",
        &format!("AI triage agent invoked on {} open alerts", pre_triage.len()));

    // Call the agent — returns decisions (or DryRun placeholders)
    let agent_decisions = agent
        .triage(&pre_triage)
        .expect("AI triage agent failed");

    // Apply decisions to a dedicated agent review session
    let agent_session = ReviewSession::new(&engine, &audit, "AI-agent/ww_alert_agent-v0.1");

    println!("  AI triage decisions:\n");
    for decision in &agent_decisions {
        println!("  {:>12}  {}", decision.label(), decision.alert_id());
        match decision {
            ww_alert_agent::AgentDecision::Confirm  { reasoning, .. } =>
                println!("               reasoning: {}", truncate(reasoning, 90)),
            ww_alert_agent::AgentDecision::Dismiss  { reasoning, .. } =>
                println!("               reasoning: {}", truncate(reasoning, 90)),
            ww_alert_agent::AgentDecision::Escalate { reasoning, to_team, .. } =>
                println!("               reasoning: {}  → {}", truncate(reasoning, 70), to_team),
            ww_alert_agent::AgentDecision::DryRun   { .. } =>
                println!("               [no API call made]"),
        }
    }
    println!();

    // Apply — skips DryRun entries automatically
    if !is_dry_run {
        agent
            .apply(&agent_decisions, &agent_session)
            .expect("agent apply failed");
        println!("  ✓  AI decisions written to ontology + audit log.\n");
    } else {
        println!("  ℹ  Dry-run: decisions not written to ontology.\n");
    }

    // ── 5. Analyst review session (human override layer) ─────────────────────
    println!("\n══════════════════════ ANALYST REVIEW ══════════════════════════\n");
    let session = ReviewSession::new(&engine, &audit, "Dr. E. Martinez (BPHS)");
    let pending = session.pending_alerts();
    println!("  {} alert(s) remaining for human review (critical-first order)\n", pending.len());

    for (i, alert) in pending.iter().enumerate() {
        println!(
            "  [{:>2}] {}  {:<10}  {:<6}  {:<25}  site={}",
            i + 1,
            alert.severity.emoji(),
            alert.severity.as_str(),
            alert.analyte_category.tag(),
            alert.pathogen,
            alert.site_id,
        );
    }
    println!();

    // Apply analyst decisions by category + analyte
    for alert in &pending {
        let outcome = match (alert.analyte_category.as_str(), alert.pathogen.as_str(), alert.site_id.as_str()) {

            // Infectious pathogen responses
            ("infectious_pathogen", "SARS-CoV-2", "bcity_south") =>
                ReviewOutcome::Confirm {
                    notes: "Clinically corroborated: ER triage up 18 % week-on-week. \
                            MOH rapid-response coordinated.".into(),
                },
            ("infectious_pathogen", "SARS-CoV-2", _) =>
                ReviewOutcome::Confirm {
                    notes: "Part of confirmed Belize City SARS-CoV-2 cluster.".into(),
                },
            ("infectious_pathogen", "Mpox", _) =>
                ReviewOutcome::Escalate {
                    to_team: "CARPHA / PAHO Regional Lab".into(),
                    notes:   "Requesting confirmatory WGS sequencing. \
                              Port-of-entry contact tracing initiated.".into(),
                },
            ("infectious_pathogen", "Norovirus_GII", _) =>
                ReviewOutcome::Confirm {
                    notes: "Community gastroenteritis cluster confirmed by GP syndromic data.".into(),
                },

            // Illicit substance responses
            ("illicit_substance", "Fentanyl", _) =>
                ReviewOutcome::Escalate {
                    to_team: "Belize National Drug Abuse Control Council / Police".into(),
                    notes:   "Fentanyl spike correlates with 3 ER overdose presentations \
                              on day 9–10. Inverse source localisation initiated for \
                              distribution network mapping.".into(),
                },
            ("illicit_substance", "Cocaine", "orange_walk") =>
                ReviewOutcome::Confirm {
                    notes: "Weekend surge pattern consistent with event-based use. \
                            Notifying Orange Walk police for public-safety awareness.".into(),
                },
            ("illicit_substance", "Methamphetamine", _) =>
                ReviewOutcome::Confirm {
                    notes: "MAMP elevation shared across Belmopan sites; \
                            consistent with supply-side event. Ongoing monitoring.".into(),
                },

            // Pharmaceutical / AMR responses
            ("pharmaceutical_amr", "Amoxicillin", _) | ("pharmaceutical_amr", "AMR_blaTEM", _) =>
                ReviewOutcome::Escalate {
                    to_team: "Ministry of Health AMR Focal Point".into(),
                    notes:   "Co-elevation of Amoxicillin and blaTEM AMR gene signals \
                              at Belmopan Core is consistent with community antibiotic \
                              over-prescription. Requesting pharmacy dispensing audit.".into(),
                },
            ("pharmaceutical_amr", "Ciprofloxacin", _) =>
                ReviewOutcome::Confirm {
                    notes: "Ciprofloxacin elevation at San Ignacio likely related to \
                            precautionary Vibrio cholerae prescription. Monitoring.".into(),
                },

            // Industrial toxicant responses
            ("industrial_toxicant", "Lead_Pb", "dangriga") | ("industrial_toxicant", "Mercury_Hg", "dangriga") =>
                ReviewOutcome::Escalate {
                    to_team: "Belize Environmental Protection Agency / WASA".into(),
                    notes:   "Co-elevation of Lead and Mercury at Dangriga suggests \
                              point-source discharge (pipe rupture or illegal workshop). \
                              Emergency sampling and site inspection requested.".into(),
                },
            ("industrial_toxicant", "Trichloroethylene", _) =>
                ReviewOutcome::Confirm {
                    notes: "TCE pulse at Belmopan Industrial Zone. \
                            Environmental inspection of upstream industrial lots initiated.".into(),
                },

            // Explosive precursor responses — escalate to security agencies
            ("explosive_precursor", "RDX", _) | ("explosive_precursor", "HMX", _) |
            ("explosive_precursor", "TNT_aminoproducts", _) =>
                ReviewOutcome::Escalate {
                    to_team: "Belize National Security Council / Police Special Branch".into(),
                    notes:   "Co-elevation of RDX, HMX, and TNT transformation products at \
                              Belmopan Industrial Zone is consistent with energetic material \
                              storage or improvised manufacturing. Joint multi-agency \
                              investigation requested; physical site inspection prioritised.".into(),
                },
            ("explosive_precursor", "TATP_DHPP", _) =>
                ReviewOutcome::Escalate {
                    to_team: "Belize Police Department Counter-Terrorism Unit".into(),
                    notes:   "TATP hydroperoxide product spike at Belize City South. \
                              Short duration consistent with TATP preparation or volatile \
                              precursor handling. Urgent scene assessment and collection \
                              of confirmatory grab samples requested.".into(),
                },
            ("explosive_precursor", "Perchlorate", _) | ("explosive_precursor", "AN_nitrate_excess", _) =>
                ReviewOutcome::Confirm {
                    notes: "Perchlorate / AN-nitrate co-elevation at Orange Walk consistent \
                            with agricultural-grade ammonium nitrate sourcing or unlicensed \
                            pyrotechnic storage. Notifying Pesticides Control Board and \
                            Police for licence audit.".into(),
                },

            // Default: dismiss marginal detections
            _ => ReviewOutcome::Dismiss {
                reason: "Within 2-σ on clinical/environmental syndromic data; \
                         likely baseline fluctuation.".into(),
            },
        };

        let outcome_label = match &outcome {
            ReviewOutcome::Confirm  { .. } => "CONFIRMED",
            ReviewOutcome::Dismiss  { .. } => "DISMISSED",
            ReviewOutcome::Escalate { .. } => "ESCALATED",
        };

        session.review(&alert.alert_id, outcome).expect("review failed");

        println!(
            "  {:>10}  {}  {} ({}  [{}])",
            outcome_label,
            alert.severity.emoji(),
            alert.alert_id,
            alert.pathogen,
            alert.analyte_category.display_label(),
        );
    }

    // ── 6. Per-category alert summary ────────────────────────────────────────
    println!("\n══════════════════ MULTI-TARGET CATEGORY SUMMARY ═══════════════════\n");
    for cat in &[
        AnalyteCategory::InfectiousPathogen,
        AnalyteCategory::PharmaceuticalAMR,
        AnalyteCategory::IllicitSubstance,
        AnalyteCategory::IndustrialToxicant,
        AnalyteCategory::ExplosivePrecursor,
    ] {
        let cat_all = list_all_alerts(&engine)
            .into_iter()
            .filter(|a| &a.analyte_category == cat)
            .collect::<Vec<_>>();
        let total = cat_all.len();
        let open  = cat_all.iter().filter(|a| matches!(a.status,
            ww_domain::query::AlertStatus::Open | ww_domain::query::AlertStatus::Escalated
        )).count();
        println!(
            "  {:22}  {:3} total alerts  {:2} still open/escalated",
            cat.display_label(), total, open,
        );
    }

    // ── 7. Daily summary report ───────────────────────────────────────────────
    println!("\n════════════════════════ DAILY REPORT ══════════════════════════════\n");
    let reporter = ReportGenerator::new(&engine, &audit);
    print!("{}", reporter.render_summary());

    // ── 8. Evidence chains (top 3 alerts by severity) ────────────────────────
    println!("\n══════════════════════ EVIDENCE CHAINS ══════════════════════════════\n");
    let mut top: Vec<_> = list_all_alerts(&engine);
    top.sort_by(|a, b| b.severity.cmp(&a.severity));
    for alert in top.into_iter().take(3) {
        let chain = reporter.evidence_chain(alert);
        print!("{}", chain.render());
    }

    // ── 9. Audit log tail ─────────────────────────────────────────────────────
    println!("══════════════════════ AUDIT LOG (last 20) ══════════════════════════\n");
    for entry in audit.recent(20) {
        println!(
            "  [{}]  {:22}  actor={:<32}  target={}",
            entry.timestamp, entry.action, entry.actor, entry.target_id
        );
    }

    // ── 10. Final statistics ───────────────────────────────────────────────────
    println!("\n  Analyte panel        : {} targets × {} sites = {} signals/day",
        ww_domain::ANALYTE_CATALOG.len(), 8,
        ww_domain::ANALYTE_CATALOG.len() * 8);
    println!("  Total audit entries  : {}", audit.len());
    println!("  Total instances      : {}", engine.instance_count());
    println!("  Total links          : {}", engine.link_count());
    println!("  Open / escalated     : {}", list_open_alerts(&engine).len());
    println!("  AI triage decisions  : {}", agent_decisions.len());
    println!("\n▶ Simulation complete.\n");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}
