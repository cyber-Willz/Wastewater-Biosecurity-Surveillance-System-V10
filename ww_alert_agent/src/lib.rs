//! `ww_alert_agent` — Agentic AI alert-triage agent.
//!
//! Calls the Anthropic API (claude-sonnet-4-6) via `agent_bridge.py`
//! to produce autonomous triage decisions for detected anomalies.

pub mod agent;
pub mod error;

pub use agent::{AgentConfig, AgentDecision, AlertAgent};
pub use error::AgentError;
