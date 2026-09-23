# ww_api — Axum + PostgreSQL service for ww_biosec

REST service around the `ww_detection` engine. PostgreSQL is the system of
record; the EWMA + spectral-hypergraph detector runs in-process and is rebuilt
from the database by replay, so a restart loses no state.

## Quick start

```bash
export DATABASE_URL=postgres://user:pw@127.0.0.1/ww_biosec
export WW_API_TOKEN=change-me            # required (or WW_ALLOW_ANONYMOUS=1 to disable auth)
cargo run --release -p ww_api            # migrates + seeds analytes on startup
curl -H "Authorization: Bearer $WW_API_TOKEN" localhost:8080/v1/summary

curl -sL -o scotland_raw.csv "https://raw.githubusercontent.com/BioRDM/COVID-Wastewater-Scotland/master/data/SARS-Cov2_RNA_monitoring_ww_scotland.csv"
wc -l scotland_raw.csv
10317 scotland_raw.csv
```

| Env var | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | – (required) | Postgres connection URL |
| `WW_API_TOKEN` | – | Bearer token for every `/v1` route |
| `WW_ALLOW_ANONYMOUS` | unset | `1` = run without auth (loudly logged) |
| `WW_BIND` | `127.0.0.1:8080` | Listen address |
| `WW_PG_SCHEMA` | unset | Run inside this schema (created if missing) |

## Endpoints

| Method & path | Purpose |
|---|---|
| `GET /healthz` | Liveness + DB ping (no auth) |
| `PUT /v1/network` | Declare sites + catchments (hyperedges); rebuilds spectral model, null threshold and detector |
| `GET /v1/network` | Topology + Fiedler value + spread threshold |
| `POST /v1/rounds` | Ingest one analyte on one date across sites; returns alerts raised |
| `GET /v1/observations` | Filter by `site_id`, `analyte`, `from`, `to` |
| `GET /v1/alerts`, `/v1/alerts/:id` | Filter by `status`, `severity`, `analyte`, `site_id`; critical-first |
| `GET /v1/alerts/:id/evidence` | alert → observation → site → catchments → round → audit trail |
| `POST /v1/alerts/:id/review` | `confirm` / `dismiss` (reason required) / `escalate` (`to_team` required) |
| `GET /v1/summary`, `/v1/analytes` | Counts; analyte catalog |
| `GET /v1/audit`, `/v1/audit/export` | Audit entries; NDJSON export (same fields as `ww_audit::AuditEntry`) |

`POST /v1/rounds` body:

```json
{"analyte":"SARS-CoV-2","observed_on":"2024-01-08",
 "observations":[{"site_id":"s1","log10_conc":4.02,"n_samples":1}],
 "source":"lims","actor":"ingest-job"}
```

## Semantics worth knowing

* **Order matters.** EWMA is order-dependent, so per analyte, rounds must arrive
  in strictly increasing date order (otherwise `409`). Late-arriving results
  cannot be inserted without a replay/recompute path (not implemented).
* **Atomic rounds.** Round, observations, alerts and audit rows commit in one
  transaction. If persistence fails, in-memory state is marked dirty and rebuilt
  from PostgreSQL before the next request. A Postgres advisory lock plus a
  check of the database's latest round guards against a second writer.
* **Topology changes** (`PUT /v1/network`) replace catchments; a site with
  observations cannot be removed (`409`, full rollback).
* **Review** only applies to `OPEN` or `ESCALATED` alerts; status change and
  audit record are one transaction.
* **Audit log** is append-only, enforced by database triggers (UPDATE / DELETE /
  TRUNCATE rejected). It is not hash-chained or signed, and a table owner could
  still drop the triggers. Audit rows are written per round, per alert and per
  review/topology change, not per observation.
* **Auth** is one shared bearer token; the `analyst` name in a review is
  self-declared, not authenticated.
* Vertex order for the Laplacian uses byte-order (`COLLATE "C"`) sorting so the
  numerics match `ww_eval::scotland_e2e` exactly.

## Tests

```bash
WW_TEST_DATABASE_URL=postgres://user:pw@127.0.0.1/ww_biosec cargo test -p ww_api
```

Without the variable, the database test is skipped. It uses a throw-away schema
and checks auth, validation, alerting, review, audit immutability, evidence
chains, topology rollback, and that a detector rebuilt from PostgreSQL is
bit-identical to the live one.

## Live end-to-end run

```bash
DATABASE_URL=postgres://user:pw@127.0.0.1/ww_biosec ./live_run_pg.sh
```

Fetches the live Public Health Scotland SARS-CoV-2 dataset, starts the server in
an isolated schema, runs the in-memory reference (`scotland_e2e`), drives the
same data through HTTP into PostgreSQL, and diffs the alerts row for row. It
then restarts the server, proves the baselines survived, and exercises the
review workflow and audit immutability. It injects one synthetic spike
(`source = 'synthetic-spike'`) in that schema; the schema is left in place for
inspection. Reference result (2026-09-21 data): 141 sites, 89 rounds, 5,458
observations, 81 alerts, identical to the reference.

Requires Rust ≥ 1.85 (built with 1.85.1; `Cargo.lock` pins `idna_adapter` 1.2.0
to avoid a 1.88 requirement), `psql`, `curl`, `python3`.
