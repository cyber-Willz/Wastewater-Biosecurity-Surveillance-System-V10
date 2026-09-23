//! Core alert-triage agent.
//!
//! Calls the Anthropic API via a Python bridge subprocess (`agent_bridge.py`)
//! which uses the `anthropic` Python SDK.  The Rust binary serialises alerts
//! to JSON on the bridge's stdin and reads a decisions JSON object from stdout.
//!
//! Fallback: if no API key is available (dry-run) or the bridge fails, a
//! conservative heuristic is applied locally.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::{json, Value};

use ww_audit::ReviewOutcome;
use ww_audit::ReviewSession;
use ww_domain::query::SurveillanceAlert;

use crate::error::AgentError;

// ── Config ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub api_key:     Option<String>,
    pub model:       String,
    pub max_tokens:  u32,
    pub verbose:     bool,
    /// Path to agent_bridge.py; defaults to alongside the binary.
    pub bridge_path: Option<PathBuf>,
}

impl AgentConfig {
    pub fn from_env() -> Self {
        Self {
            api_key:     std::env::var("ANTHROPIC_API_KEY").ok(),
            model:       "claude-sonnet-4-6".into(),
            max_tokens:  4096,
            verbose:     false,
            bridge_path: None,
        }
    }
    pub fn dry_run() -> Self { Self { api_key: None, ..Self::from_env() } }
    pub fn with_verbose(mut self) -> Self { self.verbose = true; self }
}

// ── Decision types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum AgentDecision {
    Confirm  { alert_id: String, reasoning: String, notes: String },
    Dismiss  { alert_id: String, reasoning: String, reason: String },
    Escalate { alert_id: String, reasoning: String, to_team: String, escalate_notes: String },
    DryRun   { alert_id: String },
}

impl AgentDecision {
    pub fn alert_id(&self) -> &str {
        match self {
            AgentDecision::Confirm  { alert_id, .. } => alert_id,
            AgentDecision::Dismiss  { alert_id, .. } => alert_id,
            AgentDecision::Escalate { alert_id, .. } => alert_id,
            AgentDecision::DryRun   { alert_id }     => alert_id,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            AgentDecision::Confirm  { .. } => "AI-CONFIRM",
            AgentDecision::Dismiss  { .. } => "AI-DISMISS",
            AgentDecision::Escalate { .. } => "AI-ESCALATE",
            AgentDecision::DryRun   { .. } => "DRY-RUN",
        }
    }
    pub fn to_review_outcome(&self) -> Option<ReviewOutcome> {
        match self {
            AgentDecision::Confirm { notes, reasoning, .. } =>
                Some(ReviewOutcome::Confirm {
                    notes: format!("[AI] {reasoning} — {notes}"),
                }),
            AgentDecision::Dismiss { reason, reasoning, .. } =>
                Some(ReviewOutcome::Dismiss {
                    reason: format!("[AI] {reasoning} — {reason}"),
                }),
            AgentDecision::Escalate { to_team, escalate_notes, reasoning, .. } =>
                Some(ReviewOutcome::Escalate {
                    to_team: to_team.clone(),
                    notes:   format!("[AI] {reasoning} — {escalate_notes}"),
                }),
            AgentDecision::DryRun { .. } => None,
        }
    }
}

// ── Internal deserialisation ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawDecision {
    alert_id:       String,
    action:         String,
    reasoning:      String,
    notes:          Option<String>,
    escalate_to:    Option<String>,
    escalate_notes: Option<String>,
}

// ── Agent ───────────────────────────────────────────────────────────────────

pub struct AlertAgent {
    pub config: AgentConfig,
}

impl AlertAgent {
    pub fn new(config: AgentConfig) -> Self { Self { config } }

    pub fn triage(&self, alerts: &[SurveillanceAlert]) -> Result<Vec<AgentDecision>, AgentError> {
        if alerts.is_empty() { return Ok(vec![]); }

        let Some(api_key) = &self.config.api_key else {
            println!("[AlertAgent] No ANTHROPIC_API_KEY — dry-run mode.");
            return Ok(alerts.iter()
                .map(|a| AgentDecision::DryRun { alert_id: a.alert_id.clone() })
                .collect());
        };

        // Serialise alerts to JSON for the bridge
        let alert_payload = self.serialise_alerts(alerts);
        let payload_str = serde_json::to_string(&alert_payload)?;

        if self.config.verbose {
            println!("[AlertAgent] Sending {} alerts to agent bridge...", alerts.len());
        }

        // Locate bridge script
        let bridge = self.resolve_bridge_path();

        // Spawn Python bridge
        let mut child = Command::new("python3")
            .arg(&bridge)
            .arg(api_key)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AgentError::BridgeSpawn(format!("python3 {bridge:?}: {e}")))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(payload_str.as_bytes())
                 .map_err(|e| AgentError::BridgeSpawn(e.to_string()))?;
        }

        let output = child.wait_with_output()
                         .map_err(|e| AgentError::BridgeSpawn(e.to_string()))?;

        let stderr_str = String::from_utf8_lossy(&output.stderr);
        if !stderr_str.is_empty() && self.config.verbose {
            eprintln!("[AlertAgent bridge stderr]:\n{stderr_str}");
        }

        if !output.status.success() {
            eprintln!("[AlertAgent] bridge exited with error:\n{stderr_str}");
            println!("[AlertAgent] WARN: bridge failed — applying conservative fallback.");
            return Ok(self.fallback_decisions(alerts));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if self.config.verbose {
            println!("[AlertAgent] Bridge response ({} bytes)", stdout.len());
        }

        // Parse response
        let response: Value = serde_json::from_str(&stdout)
            .map_err(|e| AgentError::Json(e))?;

        // Check for error key from bridge
        if let Some(err) = response.get("error") {
            println!("[AlertAgent] Bridge returned error: {err} — fallback applied.");
            return Ok(self.fallback_decisions(alerts));
        }

        let known: HashMap<&str, &SurveillanceAlert> = alerts.iter()
            .map(|a| (a.alert_id.as_str(), a)).collect();

        self.decisions_from_input(response, &known)
    }

    pub fn apply(&self, decisions: &[AgentDecision], session: &ReviewSession<'_>) -> Result<(), AgentError> {
        for d in decisions {
            let Some(outcome) = d.to_review_outcome() else { continue; };
            session.review(d.alert_id(), outcome)
                   .map_err(|e| AgentError::ReviewFailed(format!("{e}")))?;
        }
        Ok(())
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn serialise_alerts(&self, alerts: &[SurveillanceAlert]) -> Value {
        let alert_list: Vec<Value> = alerts.iter().map(|a| json!({
            "alert_id":         a.alert_id,
            "site_id":          a.site_id,
            "pathogen":         a.pathogen,
            "severity":         a.severity.as_str(),
            "z_score":          a.z_score,
            "spectral_score":   a.spectral_score,
            "analyte_category": a.analyte_category.as_str(),
            "analyte_tag":      a.analyte_category.tag(),
            "detected_at":      a.detected_at,
        })).collect();
        json!({ "alerts": alert_list })
    }

    fn resolve_bridge_path(&self) -> PathBuf {
        if let Some(p) = &self.config.bridge_path {
            return p.clone();
        }
        // Try next to the binary, then relative to cwd
        let exe = std::env::current_exe().unwrap_or_default();
        let sibling = exe.parent().unwrap_or(std::path::Path::new("."))
            .join("agent_bridge.py");
        if sibling.exists() { return sibling; }

        // Workspace-relative fallback (dev mode)
        let dev = PathBuf::from("ww_alert_agent/agent_bridge.py");
        if dev.exists() { return dev; }

        PathBuf::from("agent_bridge.py")
    }

    fn decisions_from_input(&self, input: Value, known: &HashMap<&str, &SurveillanceAlert>) -> Result<Vec<AgentDecision>, AgentError> {
        let raw: Vec<RawDecision> = serde_json::from_value(
            input.get("decisions").cloned().ok_or(AgentError::MalformedToolInput)?
        )?;

        let mut out = Vec::with_capacity(raw.len());
        for rd in raw {
            if !known.contains_key(rd.alert_id.as_str()) {
                println!("[AlertAgent] WARN: unknown alert_id '{}' skipped.", rd.alert_id);
                continue;
            }
            out.push(match rd.action.as_str() {
                "Confirm" => AgentDecision::Confirm {
                    alert_id:  rd.alert_id,
                    reasoning: rd.reasoning,
                    notes:     rd.notes.unwrap_or_default(),
                },
                "Dismiss" => AgentDecision::Dismiss {
                    alert_id:  rd.alert_id,
                    reasoning: rd.reasoning,
                    reason:    rd.notes.unwrap_or_default(),
                },
                "Escalate" => AgentDecision::Escalate {
                    alert_id:       rd.alert_id,
                    reasoning:      rd.reasoning,
                    to_team:        rd.escalate_to.unwrap_or_else(|| "BPHS / MOH".into()),
                    escalate_notes: rd.escalate_notes.unwrap_or_default(),
                },
                other => {
                    println!("[AlertAgent] WARN: unknown action '{other}' → Confirm.");
                    AgentDecision::Confirm {
                        alert_id:  rd.alert_id,
                        reasoning: rd.reasoning,
                        notes:     rd.notes.unwrap_or_default(),
                    }
                }
            });
        }
        Ok(out)
    }

    fn fallback_decisions(&self, alerts: &[SurveillanceAlert]) -> Vec<AgentDecision> {
        use ww_domain::query::Severity;
        alerts.iter().map(|a| match a.severity {
            Severity::Critical | Severity::Red => AgentDecision::Escalate {
                alert_id:       a.alert_id.clone(),
                reasoning:      format!("Fallback: {} severity auto-escalated.", a.severity.as_str()),
                to_team:        "BPHS / MOH Rapid Response".into(),
                escalate_notes: "AI bridge unavailable; conservative escalation applied.".into(),
            },
            Severity::Amber if a.z_score < 2.8 && a.spectral_score < 0.15 => AgentDecision::Dismiss {
                alert_id:  a.alert_id.clone(),
                reasoning: "Fallback: marginal AMBER (z<2.8, spectral<0.15).".into(),
                reason:    "Within expected noise floor — monitor next cycle.".into(),
            },
            _ => AgentDecision::Confirm {
                alert_id:  a.alert_id.clone(),
                reasoning: "Fallback: AMBER above noise floor — confirmed for monitoring.".into(),
                notes:     "Review at next analyst session.".into(),
            },
        }).collect()
    }
}
