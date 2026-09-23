//! Outbound webhook dispatcher for n8n (and compatible webhook endpoints).
//!
//! # What fires
//!
//! | Event              | Trigger                                    | n8n node                  |
//! |--------------------|--------------------------------------------|---------------------------|
//! | `alert.raised`     | New alert in `POST /v1/rounds`             | Webhook → alert-triage WF |
//! | `alert.reviewed`   | `confirm` / `dismiss` / `escalate`         | Webhook → notify WF       |
//! | `round.ingested`   | Every successful round ingest              | Webhook → dashboard WF    |
//! | `network.updated`  | `PUT /v1/network`                          | Webhook → topology WF     |
//!
//! # Configuration (env vars)
//!
//! | Variable                  | Example                                     | Meaning |
//! |---------------------------|---------------------------------------------|---------|
//! | `WW_N8N_BASE_URL`         | `http://n8n:5678`                           | n8n base URL |
//! | `WW_N8N_ALERT_PATH`       | `/webhook/ww-alert-raised`                  | Path for `alert.raised` |
//! | `WW_N8N_REVIEW_PATH`      | `/webhook/ww-alert-reviewed`                | Path for `alert.reviewed` |
//! | `WW_N8N_ROUND_PATH`       | `/webhook/ww-round-ingested`                | Path for `round.ingested` |
//! | `WW_N8N_NETWORK_PATH`     | `/webhook/ww-network-updated`               | Path for `network.updated` |
//! | `WW_N8N_TOKEN`            | `supersecrettoken`                          | Bearer token sent to n8n (opt.) |
//! | `WW_N8N_TIMEOUT_MS`       | `3000`                                      | Per-request timeout (default 3 s) |
//! | `WW_N8N_RETRY`            | `2`                                         | Retries on transient error (default 2) |
//!
//! Any path left unset disables that event type. Setting `WW_N8N_BASE_URL`
//! alone with default paths enables all four events.
//!
//! # Reliability contract
//!
//! Webhook calls are **fire-and-forget**: a delivery failure is logged as a
//! warning but never propagates an error to the caller. Database writes always
//! succeed first; the hook fires after commit. This means the API is safe even
//! if n8n is down or slow.

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde_json::Value;
use tokio::time::timeout;
use tracing::{debug, warn};

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub base_url: Option<String>,
    pub alert_path: String,
    pub review_path: String,
    pub round_path: String,
    pub network_path: String,
    pub token: Option<String>,
    pub timeout_ms: u64,
    pub retries: u32,
}

impl WebhookConfig {
    pub fn from_env() -> Self {
        Self {
            base_url: std::env::var("WW_N8N_BASE_URL").ok().filter(|s| !s.is_empty()),
            alert_path: std::env::var("WW_N8N_ALERT_PATH")
                .unwrap_or_else(|_| "/webhook/ww-alert-raised".into()),
            review_path: std::env::var("WW_N8N_REVIEW_PATH")
                .unwrap_or_else(|_| "/webhook/ww-alert-reviewed".into()),
            round_path: std::env::var("WW_N8N_ROUND_PATH")
                .unwrap_or_else(|_| "/webhook/ww-round-ingested".into()),
            network_path: std::env::var("WW_N8N_NETWORK_PATH")
                .unwrap_or_else(|_| "/webhook/ww-network-updated".into()),
            token: std::env::var("WW_N8N_TOKEN").ok().filter(|s| !s.is_empty()),
            timeout_ms: std::env::var("WW_N8N_TIMEOUT_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3_000),
            retries: std::env::var("WW_N8N_RETRY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
        }
    }

    /// Returns `None` if base_url is unset (webhooks disabled globally).
    pub fn url_for(&self, path: &str) -> Option<String> {
        self.base_url
            .as_deref()
            .map(|base| format!("{}{}", base.trim_end_matches('/'), path))
    }
}

// ── Dispatcher ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebhookDispatcher {
    client: Client,
    config: Arc<WebhookConfig>,
}

impl WebhookDispatcher {
    pub fn new(config: WebhookConfig) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms * (config.retries as u64 + 1)))
            .build()
            .expect("reqwest client");
        Self { client, config: Arc::new(config) }
    }

    /// Disabled dispatcher — does nothing. Used when `WW_N8N_BASE_URL` is unset.
    pub fn disabled() -> Self {
        Self::new(WebhookConfig {
            base_url: None,
            alert_path: String::new(),
            review_path: String::new(),
            round_path: String::new(),
            network_path: String::new(),
            token: None,
            timeout_ms: 3_000,
            retries: 0,
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.config.base_url.is_some()
    }

    // ── Public fire methods (all async, all fire-and-forget) ─────────────────

    /// Fire `alert.raised` for each new alert in a round.
    pub fn fire_alerts_raised(&self, round_id: i64, alerts: &[Value]) {
        if alerts.is_empty() {
            return;
        }
        if let Some(url) = self.config.url_for(&self.config.alert_path) {
            let payload = serde_json::json!({
                "event": "alert.raised",
                "round_id": round_id,
                "alerts": alerts,
            });
            self.spawn_fire(url, payload);
        }
    }

    /// Fire `alert.reviewed` after a confirm / dismiss / escalate.
    pub fn fire_alert_reviewed(&self, alert: &Value, outcome: &str, analyst: &str) {
        if let Some(url) = self.config.url_for(&self.config.review_path) {
            let payload = serde_json::json!({
                "event": "alert.reviewed",
                "outcome": outcome,
                "analyst": analyst,
                "alert": alert,
            });
            self.spawn_fire(url, payload);
        }
    }

    /// Fire `round.ingested` after every successful round.
    pub fn fire_round_ingested(&self, round: &Value) {
        if let Some(url) = self.config.url_for(&self.config.round_path) {
            let payload = serde_json::json!({
                "event": "round.ingested",
                "round": round,
            });
            self.spawn_fire(url, payload);
        }
    }

    /// Fire `network.updated` after topology change.
    pub fn fire_network_updated(&self, network: &Value) {
        if let Some(url) = self.config.url_for(&self.config.network_path) {
            let payload = serde_json::json!({
                "event": "network.updated",
                "network": network,
            });
            self.spawn_fire(url, payload);
        }
    }

    // ── Internals ─────────────────────────────────────────────────────────────

    fn spawn_fire(&self, url: String, payload: Value) {
        let client = self.client.clone();
        let token = self.config.token.clone();
        let timeout_ms = self.config.timeout_ms;
        let retries = self.config.retries;
        tokio::spawn(async move {
            fire_with_retry(&client, &url, payload, token.as_deref(), timeout_ms, retries).await;
        });
    }
}

async fn fire_with_retry(
    client: &Client,
    url: &str,
    payload: Value,
    token: Option<&str>,
    timeout_ms: u64,
    retries: u32,
) {
    let dur = Duration::from_millis(timeout_ms);
    for attempt in 0..=retries {
        let mut req = client.post(url).json(&payload);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        match timeout(dur, req.send()).await {
            Ok(Ok(resp)) => {
                let status = resp.status();
                if status.is_success() || status == StatusCode::NO_CONTENT {
                    debug!(url, attempt, "webhook delivered");
                    return;
                }
                // n8n returns 404 when a workflow is paused — treat as non-retryable
                if status == StatusCode::NOT_FOUND {
                    warn!(url, %status, "webhook 404 — workflow paused or path wrong (no retry)");
                    return;
                }
                warn!(url, %status, attempt, "webhook non-2xx");
            }
            Ok(Err(e)) => warn!(url, %e, attempt, "webhook send error"),
            Err(_) => warn!(url, attempt, timeout_ms, "webhook timeout"),
        }
        if attempt < retries {
            tokio::time::sleep(Duration::from_millis(200 * 2_u64.pow(attempt))).await;
        }
    }
    warn!(url, "webhook delivery failed after all retries");
}
