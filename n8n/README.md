# n8n Integration for `ww_biosec`

This directory contains everything needed to connect `ww_api` to an n8n
workflow engine: outbound webhook dispatch from Rust, four ready-to-import
workflow JSON files, and a Docker Compose overlay.

---

## Architecture

```
ww_api (Axum)
  │  POST /v1/rounds       ──after commit──►  webhook/mod.rs  ──► n8n /webhook/ww-alert-raised
  │  POST /v1/alerts/:id/review              (fire-and-forget) ──► n8n /webhook/ww-alert-reviewed
  │  PUT  /v1/network                                          ──► n8n /webhook/ww-network-updated
  │  POST /v1/rounds (every round)                            ──► n8n /webhook/ww-round-ingested
  │
  └─ all fires: async tokio::spawn, retried up to WW_N8N_RETRY times,
     timeout WW_N8N_TIMEOUT_MS ms. Never blocks a request. DB writes always commit first.

n8n
  ├── ww_alert_triage.workflow.json       Severity routing → auto-confirm/escalate via /v1/alerts/:id/review
  ├── ww_alert_reviewed.workflow.json     Slack notifications on confirm/escalate/dismiss
  ├── ww_round_ingested.workflow.json     Per-round Slack summary + Grafana annotation
  ├── ww_network_updated.workflow.json    Topology-change audit notification to #biosec-ops
  └── ww_lims_ingest.workflow.json        Scheduled LIMS poll → POST /v1/rounds (active=false; configure before activating)
```

---

## Rust changes (in `ww_api`)

| File | Change |
|---|---|
| `src/webhook/mod.rs` | New module: `WebhookConfig` (env-driven), `WebhookDispatcher` (async fire-and-forget with retry) |
| `src/lib.rs` | `pub mod webhook`; `AppState.hooks: WebhookDispatcher`; wired into `AppState::init` |
| `src/rounds.rs` | Fires `round.ingested` and `alert.raised` after successful `persist_round` commit |
| `src/handlers.rs` | Fires `alert.reviewed` after successful `review_alert` commit |
| `src/network.rs` | Fires `network.updated` after successful topology rebuild |
| `Cargo.toml` | `reqwest` upgraded to include `rustls-tls` feature (for HTTPS n8n targets) |

The dispatcher is entirely additive. With `WW_N8N_BASE_URL` unset (the default
in tests and bare local runs), `WebhookConfig::from_env` leaves `base_url = None`
and every `fire_*` call short-circuits immediately. No requests are made, no
tasks are spawned. The existing test suite passes unchanged.

---

## Environment variables

### ww_api

| Variable | Default | Description |
|---|---|---|
| `WW_N8N_BASE_URL` | *(unset — webhooks off)* | n8n base URL, e.g. `http://n8n:5678` |
| `WW_N8N_ALERT_PATH` | `/webhook/ww-alert-raised` | Path for `alert.raised` events |
| `WW_N8N_REVIEW_PATH` | `/webhook/ww-alert-reviewed` | Path for `alert.reviewed` events |
| `WW_N8N_ROUND_PATH` | `/webhook/ww-round-ingested` | Path for `round.ingested` events |
| `WW_N8N_NETWORK_PATH` | `/webhook/ww-network-updated` | Path for `network.updated` events |
| `WW_N8N_TOKEN` | *(unset)* | Bearer token sent to n8n (set the matching credential in n8n) |
| `WW_N8N_TIMEOUT_MS` | `3000` | Per-attempt timeout in milliseconds |
| `WW_N8N_RETRY` | `2` | Number of retries on non-404 failures (exponential backoff: 200ms, 400ms) |

### n8n (via docker-compose.n8n.yml)

| Variable | Description |
|---|---|
| `WW_API_BASE_URL` | ww_api base URL seen from n8n container, e.g. `http://ww_api:8080` |
| `WW_DASHBOARD_URL` | Dashboard base URL for deep links in Slack messages |
| `GRAFANA_URL` | Grafana base URL for annotation POSTs |
| `LIMS_BASE_URL` | LIMS API base URL (enables the ingest scheduler workflow) |

---

## Quickstart

### 1. Deploy

```bash
# Add webhook env vars to ww_api, then start the stack with n8n overlay:
docker compose -f docker-compose.yml -f n8n/docker-compose.n8n.yml up -d

# Import all four workflows:
docker compose exec n8n n8n import:workflow --separate --input=/n8n-workflows
```

### 2. Configure credentials in n8n UI (`http://localhost:5678`)

| Credential type | Name (suggested) | Value |
|---|---|---|
| HTTP Bearer Auth | `ww_api` | `$WW_API_TOKEN` |
| Slack OAuth2 | `biosec-slack` | your workspace OAuth app |
| HTTP Basic Auth | `grafana` | user `api`, password = Grafana service account token |

### 3. Activate workflows

Open each workflow in the n8n editor and toggle it to **Active**.  
Leave `ww_lims_ingest` inactive until `LIMS_BASE_URL` is set.

### 4. Smoke test

```bash
# Ingest a round — should fire ww-round-ingested and (if alerts) ww-alert-raised:
curl -X POST -H "Authorization: Bearer $WW_API_TOKEN" \
     -H "Content-Type: application/json" \
     -d '{"analyte":"SARS-CoV-2","observed_on":"2026-01-15",
          "observations":[{"site_id":"BZC_SOUTH","log10_conc":6.5}],
          "source":"smoke-test","actor":"dev"}' \
     http://localhost:8080/v1/rounds

# Check n8n execution log at http://localhost:5678
```

---

## Webhook payload shapes

### `alert.raised`

```json
{
  "event": "alert.raised",
  "round_id": 42,
  "alerts": [
    {
      "alert_id": 7,
      "site_id": "BZC_SOUTH",
      "severity": "RED",
      "z_score": 4.12,
      "spectral_score": 0.031,
      "ewma": 3.8,
      "log10_conc": 5.9,
      "n_obs": 12
    }
  ]
}
```

### `alert.reviewed`

```json
{
  "event": "alert.reviewed",
  "outcome": "ESCALATED",
  "analyst": "dr.santos",
  "alert": { /* full alert record — same shape as GET /v1/alerts/:id */ }
}
```

### `round.ingested`

```json
{
  "event": "round.ingested",
  "round": {
    "round_id": 42,
    "analyte": "SARS-CoV-2",
    "observed_on": "2026-01-15",
    "n_observations": 8,
    "n_warm": 6,
    "spectral_score": 0.021,
    "spread_threshold": 0.966,
    "alerts": [ /* same as alert.raised alerts array */ ]
  }
}
```

### `network.updated`

```json
{
  "event": "network.updated",
  "network": {
    "sites": 8,
    "catchments": 5,
    "sites_added": 1,
    "sites_removed": 0,
    "fiedler_value": 0.047619,
    "spread_threshold": 0.9662
  }
}
```

---

## Reliability notes

- **Fires after commit.** n8n never sees an event for data that didn't make it
  to PostgreSQL. The event is always a consequence of a durable state change.

- **Fire-and-forget.** A down or slow n8n never delays an API response. The
  dispatcher spawns a `tokio::spawn` task; the HTTP handler returns immediately.

- **404 is not retried.** n8n returns 404 when a workflow is paused or the path
  is wrong. Retrying would just accumulate noise. Fix the path or re-activate
  the workflow in n8n.

- **No delivery guarantee.** If the ww_api process crashes between commit and
  the spawn completing, the event is lost. For guaranteed delivery, add a
  `pending_webhooks` table and a background drainer — the architecture is ready
  for it (the payload is already a self-contained JSON value).

- **Idempotency.** n8n workflows that call back to `/v1/alerts/:id/review` will
  get a `409 Conflict` if the alert is already reviewed. Handle this in the
  workflow's error branch (ignore 409, alert all other errors).

---

## Extending

| Task | Where |
|---|---|
| Add a new event type | Add a `fire_*` method to `WebhookDispatcher`, call it after the relevant commit, add a new `WW_N8N_*_PATH` env var |
| Per-severity Slack channels | Extend the switch node in `ww_alert_triage` |
| PagerDuty on-call escalation | Replace the Slack escalation node with the n8n PagerDuty node |
| Email digest | Add a cron trigger + HTTP GET `/v1/alerts?status=OPEN` + Email node |
| Guaranteed delivery | Add `webhooks_pending` Postgres table; drain with a background task in `ww_api` |
| LIMS CSV over SFTP | Replace "Fetch LIMS results" in `ww_lims_ingest` with an SFTP Read node |
