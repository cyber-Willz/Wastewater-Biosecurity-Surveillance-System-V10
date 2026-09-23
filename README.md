# ww_biosec — Wastewater Biosecurity Surveillance System

A Rust workspace implementing an end-to-end **wastewater epidemiological
surveillance** pipeline for early outbreak detection.  Wastewater monitoring
can detect pathogen signals **3–7 days before clinical symptoms** appear in a
population, providing critical lead-time for a public-health response.

---

## Architecture

```
ww_biosec/
├── vendor/
│   ├── ontology_engine/        # in-memory knowledge graph (Object/Link model)
│   └── spectral_hypergraph/    # hypergraph Laplacian + spectral analysis
├── ww_domain/                  # Domain schema, typed factory + query layer
├── ww_detection/               # EWMA baseline + spectral network anomaly detection
├── ww_audit/                   # Append-only audit log + analyst review workflow
├── ww_alert_agent/             # LLM/rule-based alert triage bridge
├── ww_shpinn/                  # PDE-informed regression: forward ADR solver + hypergraph inverse-source (v0.4)
├── ww_api/                     # Axum REST service + PostgreSQL persistence (v0.5)
├── docs/ww_biosec_theory.md    # Full mathematical / physical / spectral-hypergraph / PINN treatment
└── ww_runner/                  # Simulation binary (ww_biosec)
```

### `ww_domain`

Wraps `OntologyEngine` with a typed domain schema:

| Object type        | Primary key  | Description |
|--------------------|-------------|-------------|
| `MonitoringSite`   | `site_id`   | A physical wastewater sampling point |
| `WastewaterSample` | `sample_id` | A collected grab/composite sample |
| `PathogenSignal`   | `signal_id` | log₁₀ copies/L for one pathogen in one sample |
| `SurveillanceAlert`| `alert_id`  | Machine-generated biosurveillance alert |
| `AuditRecord`      | `audit_id`  | Immutable audit trail entry |

Link types encode the provenance chain:
```
SurveillanceAlert –[alert_from_signal]–► PathogenSignal
PathogenSignal    –[signal_from_sample]–► WastewaterSample
WastewaterSample  –[sample_at_site]–► MonitoringSite
MonitoringSite    –[site_flows_to]–► MonitoringSite  (sewer topology)
```

### `ww_detection`

Two complementary detection engines work in tandem:

**Statistical detection** (`EwmaBaseline` + `AnomalyDetector`):

- Per-`(site, pathogen)` EWMA baseline with Welford online variance (α from the analyte's decay rate, `α = clamp(1 − e^{−k}, 0.10, 0.40)`; for unclamped k the EWMA is the exact discretisation of first-order relaxation at rate k).
- **v0.4:** robust update — the EWMA sees deviations winsorised to ±½·z_thr·s and Welford excludes observations beyond 1.5·z_thr·s, so an outbreak cannot inflate the spread it is scored against; first observation is counted once (warm-up = 7 prior observations).
- Anomaly fires when z-score ≥ per-analyte threshold (2.5σ pathogens … 3.5σ toxicants). Note: with an estimated variance the z-score is Student-t-like, so "2.5σ" is ≈2–3 % per test, not 0.6 %.
- Severity: AMBER → RED (3.5σ) → CRITICAL (5.0σ). For toxicants (z_thr = 3.5) AMBER is unreachable.

**Spectral network detection** (`SewageNetwork`):

The sewage network is modelled as a `SpectralHypergraph` where:
- **Vertices** = monitoring sites
- **Hyperedges** = catchment zones (+ one weak background edge, weight 0.05)

The normalized hypergraph Laplacian Δ (Zhou et al.) has spectrum in [0, 1]; on the Belize network it is exactly {0, 1/21, 9/14, 1×5}. Its null vector is D^{1/2}·1 (not the constant vector), so the score is applied to the mixing-scaled standardised residual `u_v = √d_v · z_v` (v0.4):

```
Rayleigh quotient (norm.)  RQ  = uᵀΔu / (uᵀu · λ_max)      ∈ [0, 1]
High-freq energy fraction  HFF = 1 − Σ_{k<2}(vₖᵀu)²/uᵀu    ∈ [0, 1]
Composite score            S   = 0.6·RQ + 0.4·HFF          ∈ [0, 1]
```

The upgrade threshold is an empirical null quantile of S (`SewageNetwork::null_threshold`, q95 = 0.966 on the Belize network) instead of a fixed 0.40.
See `docs/ww_biosec_theory.md` §4 and Appendix C: on the shipped scenario the calibrated network score does not fire, i.e. the statistic is high-pass and shape-only; it is retained as a hook, not as a proven discriminator.

**Severity matrix:**

| z-score | spectral score (> threshold) | Severity  |
|---------|---------------|-----------|
| ≥ 5.0   | any           | CRITICAL  |
| ≥ 3.5   | any           | RED       |
| ≥ 3.5   | above thr.    | CRITICAL  |
| ≥ 2.5   | any           | AMBER     |
| ≥ 2.5   | above thr.    | RED       |
| < 2.5   | any           | (no alert)|

### `ww_audit`

Human-in-the-loop capabilities:

- **`AuditLog`** — thread-safe, append-only (by API) in-memory event log, hash-chained since v0.6
  (`prev_hash`/`entry_hash` SHA-256, `verify_chain()`; not signed — see
  Appendix D of `docs/ww_biosec_theory.md` for what the chain does and does
  not guarantee).  Every ingestion, detection, and review decision is
  recorded.  Export as NDJSON for regulatory submission via `audit.export_ndjson()`.
- **`ReviewSession`** — analyst confirms / dismisses / escalates open alerts,
  writing decisions back to the ontology and the audit log atomically.
- **`ReportGenerator`** — builds full `EvidenceChain`s (provenance from alert
  → signal → sample → site → audit trail) and `DailySummary` aggregates.

---

## Belize Simulation Scenario

The `ww_biosec` binary runs a 15-day simulation of 8 Belizean monitoring sites
across Belize City, Belmopan, Orange Walk, San Ignacio, and Dangriga.

Three pathogens are monitored: SARS-CoV-2 (N1 gene, qPCR), Mpox (E6L gene,
ddPCR), and *Vibrio cholerae* (ctxA gene, qPCR).

v0.3 adds an **Explosive Precursor** category (7 analytes) for IED manufacturing
detection and post-blast contamination mapping (see `docs/ww_biosec_theory.md` §6).

Simulated outbreak events:
- **Days 8–12**: SARS-CoV-2 cluster at Belize City South, spreading through
  the shared trunk sewer to North Side and the WWTP inlet.
- **Days 11–13**: Mpox traveller import at Belmopan Urban Core.

The system raises severity-graded alerts on 14 of the 15 injected pulses (see `CHANGELOG.md` for measured detection/false-alarm counts; there is no clinical-presentation model), and walks through an analyst review session.

---

## Building

```bash
# Prerequisites: Rust 1.75+ (rustup recommended)
cargo build --release -p ww_runner

# Run the simulation
cargo run --release --bin ww_biosec
```

---

## Extending

| Task | Where |
|------|-------|
| Add a new pathogen / analyte | `ww_domain/src/analyte.rs` → `ANALYTE_CATALOG` |
| Add a monitoring site | `scenario.rs` → `SITES` + update `catchments` |
| Change alert thresholds | `ww_detection/src/detector.rs` → `DEFAULT_Z_THRESHOLD` |
| Plug in real LIMS data | Replace `DomainFactory::create_sample/signal` calls |
| Connect to a PostgreSQL store | Done in `ww_api` (relational schema in `ww_api/migrations/`); `ww_runner` still uses the in-memory `OntologyEngine` |
| REST API | `ww_api/src/` (Axum 0.7); see `ww_api/README.md` |

---

## SHPINN (`ww_shpinn`, v0.4)

Implements the solvable core of the spectral-hypergraph PINN of the theory document:

- **Forward:** `∂ₜc + v∂ₓc − D∂ₓₓc + kc = f` on a reach, represented by fixed random `tanh` features so the physics-informed loss (PDE residual + IC + BC + measurements) is a *linear* least-squares problem — exact, deterministic, no optimiser. Validated against the analytic Gaussian solution (max error ≈ 0.004–0.005 for peak 1.0).
- **Inverse:** hypergraph-regularised source estimation `min ‖Af−y‖²_{Σ⁻¹} + β fᵀD^{1/2}ΔD^{1/2}f + γ‖f‖²` (unique for γ>0, closed form).
- Works on **linear** concentration (the PDE is linear in c, not in log₁₀ c).
- Not yet: junction networks, log-space measurement model, learning k.

```bash
cargo test --workspace
cargo run --release -p ww_shpinn --example verify_forward
```

---

## REST API + PostgreSQL (`ww_api`, v0.5)

An Axum service that persists sites, catchments, observations, alerts, analyst
reviews and the audit log in PostgreSQL and runs the `ww_detection` engine
in-process (detector state is rebuilt from the database by replay). See
[`ww_api/README.md`](ww_api/README.md) for endpoints and semantics.

```bash
DATABASE_URL=postgres://user:pw@127.0.0.1/ww_biosec ./live_run_pg.sh   # full live run
```

---

## Regulatory alignment

- Alert evidence chains list the provenance path alert → signal → sample → site plus the audit trail.
- The audit log is append-only by API (in `ww_api`: enforced by PostgreSQL triggers) and timestamped to ISO-8601 UTC. Since v0.6 it is hash-chained (`prev_hash`/`entry_hash`, DB trigger rejects chain forks on insert, `GET /v1/audit/verify` checks the full chain); it is **not** cryptographically signed by an external authority — see Appendix D of `docs/ww_biosec_theory.md` for the exact guarantee.
- The NDJSON export is a plain dump of `AuditEntry`. No mapping to WHO IHR / ECDC schemas is implemented; treat any such alignment as future work.
