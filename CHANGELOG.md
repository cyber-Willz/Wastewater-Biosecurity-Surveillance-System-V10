# Changelog

## v0.7 — SeaORM entity layer

Adds SeaORM as a typed entity/query layer for `ww_api`, alongside the
existing `tokio-postgres`/`deadpool-postgres` driver.

* New `ww_api::orm` module: `DeriveEntityModel` structs for all six tables
  (`analytes`, `monitoring_sites`, `catchments`, `catchment_members`,
  `rounds`, `observations`, `alerts`, `audit_log`), with full `Relation`
  definitions wired between them.
* `orm::connect(database_url)` opens a SeaORM `DatabaseConnection`
  (sqlx-postgres backend) sized conservatively (max 4 connections) — the
  `deadpool-postgres` pool continues to own the bulk of connection capacity
  and every PostgreSQL-specific operation (advisory locks, `UNNEST` bulk
  inserts, `nextval`-based round IDs).
* `AppState` now carries both `pool: deadpool_postgres::Pool` and
  `db: sea_orm::DatabaseConnection`. `AppState::init_with_url(pool, token,
  database_url)` is the new preferred constructor; `AppState::init` is kept
  as a thin wrapper that reads `DATABASE_URL` from the environment.
* Four new routes under `/v1/orm/*` (`analytes`, `sites`, `summary`,
  `alerts`) demonstrate the entity layer end-to-end, returning the same JSON
  shapes as their `tokio-postgres` counterparts for parity checking.
* Typed query helpers in `orm::` (`list_analytes`, `list_sites`,
  `summary_counts`, `list_alerts_paged`, `observations_for_round`,
  `audit_for_targets`, `find_alert`, `list_catchment_members`,
  `list_audit_entries`) cover the read paths a UI or reporting job would
  need.

**Build note:** requires a current Rust toolchain (`rustup update stable`,
roughly ≥ 1.85). Recent releases across this dependency graph
(`crypto-common`, `cpufeatures`, `time-core`, `getrandom`,
`deadpool-runtime`, and others) declare `edition = "2024"` in their
manifests; an old distro-packaged `rustc` (e.g. Ubuntu 24.04's apt `rustc`
1.75) cannot even parse those during dependency resolution.

## v0.6 — backtesting rigor, site reliability, spectral gate, hash-chained audit

Closes four gaps flagged against v0.5: coarse per-wave hit/miss backtesting, no
per-site reliability weighting (low-density sites could dominate the alert
queue), a spectral severity term computed from health-board proxies rather
than real sewer connectivity, and an audit trail that was append-only by DB
trigger but not tamper-evident.

### Backtesting (`ww_eval/src/bin/real_data.rs`)
* **Rewrite:** single hit/miss-per-wave boolean replaced with a full
  sensitivity/specificity/PPV confusion matrix per `(wave, severity_floor)`
  cell, for three severity floors (AMBER+, RED+, CRITICAL+) across all three
  wave windows. TP/FP/TN/FN defined at `(site, week)` granularity.
* Per-site table: first-alert date per wave (lead time), total false-alarm
  count, and observation count — surfaces low-density sites directly instead
  of hiding them behind an aggregate pass/fail.
* `--spectral enabled|disabled|gated` flag reruns the same matrix with the
  spectral term forced on/off/gated, so its net contribution to sensitivity
  and specificity can be read off directly rather than assumed.
* `n_samples` is now read from the input CSV (column 5, defaults to 1) and
  passed through to the detector's density gate.

### Site reliability (`ww_detection::site_weight`, new module)
* **New:** `SiteWeight { weight: f64, min_samples: u32 }` and
  `SiteWeightRegistry`. `weight ∈ (0, 1]` raises the effective z-threshold as
  `z_eff = z_threshold / √weight` (standard-error-of-weighted-mean argument —
  see `docs/ww_biosec_theory.md` Appendix D); `min_samples` gates alert generation
  (not baseline updates) below a minimum round sample count.
* Unregistered sites default to `weight = 1.0, min_samples = 1` — exact
  pre-v0.6 behaviour, zero breaking change for existing deployments.
* `AnomalyDetector::observe_analyte_weighted` is the new primary entry point;
  `observe_analyte` now delegates to it with passthrough defaults
  (`n_samples = 1`, gate always open) for backward compatibility.
* `ww_api`: `PUT /v1/network/site-weights` (replace registry),
  `GET /v1/network/site-weights` (list, sorted by site_id, includes computed
  `z_factor = 1/√weight` for operator visibility). Weights are in-memory
  engine configuration, not persisted to Postgres — re-applied on restart by
  the deploying script/environment, not lost on a detector rebuild.

### Spectral gate (`ww_detection::spectral_mode`, new module)
* **New:** `SpectralMode::{Enabled, Gated { min_real_catchments }, Disabled}`.
  Default is `Gated { min_real_catchments: 2 }`: the spectral score is still
  computed and stored on every `AnomalyEvent` for diagnostics, but does not
  influence severity upgrades unless the network has at least that many
  catchments with ≥ 2 real member sites.
* `SewageNetwork::build` now counts and exposes `n_real_catchments()`.
* `ww_api`: `PUT /v1/network/spectral-mode` to flip modes at runtime once real
  sewer connectivity is loaded via `PUT /v1/network`; change is logged via
  `tracing::info!` and recorded in the round's audit JSON (`"spectral_mode"`
  field) so every alert's evidence chain shows whether the term was live.
* `real_data.rs` now runs with the gated default against health-board
  proxies, making explicit in its output that the spectral contribution shown
  there is a lower bound on what real catchment topology would provide.

### Audit trail (`ww_audit::log`, `ww_api::audit`)
* **New:** hash-chained `AuditEntry` — `prev_hash` (previous entry's
  `entry_hash`, or `"GENESIS"`) and `entry_hash` =
  `SHA-256(audit_id\ntimestamp\nactor\naction\ntarget_id\ndetails\nprev_hash)`.
  `AuditLog::verify_chain()` / `ww_api::audit::verify_chain()` walk the full
  chain and report the first broken link, if any.
* Migration `0002_audit_chain.sql`: adds `prev_hash`/`entry_hash` columns and
  an `audit_chain_check` BEFORE INSERT trigger that rejects any row whose
  `prev_hash` does not match the current chain tip — a direct-DML fork attempt
  is rejected at the database level, not just detectable after the fact.
* `ww_api::audit::record` now unconditionally takes `db::ADVISORY_KEY` as its
  first statement, closing a read-then-write race: without serialising every
  writer (not just `rounds`/`network`, which already held it — `review_alert`
  did not), two concurrent callers could each read the same chain tip under
  READ COMMITTED and insert two rows that both claim to extend it, which the
  BEFORE INSERT trigger cannot always catch on its own.
* `ww_api`: `GET /v1/audit/verify` runs a full-table chain verification
  (`{"intact": bool, "entries_checked": N, "first_break_audit_id": null|N}`).
  `GET /v1/audit` and `/v1/audit/export` now include `prev_hash`/`entry_hash`
  in every record.
* **Explicitly out of scope:** this closes integrity (tamper-evidence,
  ordering), not non-repudiation. A party able to rewrite the full sequence
  can re-derive a consistent chain; closing that requires periodically signing
  `AuditLog::chain_tip()` with an HSM or submitting it to an external RFC 3161
  timestamping authority, which is a deployment decision, not a code change.
  See `ww_audit/src/log.rs` module doc.

## v0.5 — Axum + PostgreSQL service (`ww_api`)

* **New crate `ww_api`:** Axum 0.7 REST service; PostgreSQL (tokio-postgres + deadpool) is the system of record for sites, catchments, observations, alerts, reviews and audit. Embedded, advisory-locked migrations; analyte catalog mirrored into the database.
* Detector state is derived: rebuilt from `observations` by replay at startup / after topology change / after any failed write. Detection path is the unchanged `ww_detection` engine (same peek-z → residual spectral score → observe sequence as `scotland_e2e`).
* Atomic round ingest (round + observations + alerts + audit in one transaction), strict per-analyte date ordering, multi-writer guard.
* Analyst review workflow with evidence chains; audit log append-only via database triggers; NDJSON export.
* Bearer-token auth on `/v1` (server refuses to start without a token unless `WW_ALLOW_ANONYMOUS=1`).
* `live_run_pg.sh`, `scotland_live` client and `ww_api/scripts/compare_alerts.py`: live PHS Scotland data through HTTP into PostgreSQL, diffed row-for-row against `scotland_e2e` (81/81 identical on the 2026-09-21 snapshot).
* PostgreSQL integration test (`WW_TEST_DATABASE_URL`).
* **Fix:** `ww_domain/src/analyte.rs` had a stray `];` after `ANALYTE_CATALOG` (crate did not compile).
* `Cargo.lock`: `idna_adapter` pinned to 1.2.0 to keep Rust 1.85 sufficient.
* Not changed: `ww_runner` / Belize simulation still use the in-memory `OntologyEngine`; the audit log is not hash-chained or signed.

## v0.4 — corrections + SHPINN (this archive)

Derived from the formal analysis in `docs/ww_biosec_theory.md`; measured effect in its Appendix C.

### Detection (`ww_detection`)
* **Fix:** the first observation of a `(site, analyte)` key was counted twice (`n = 2` after one sample). Warm-up is now exactly 7 prior observations.
* **Fix:** outbreaks inflated the variance they were scored against (self-masking). New `EwmaBaseline::ingest_robust` (winsorised EWMA, outlier-excluded Welford variance); `AnomalyDetector::observe_analyte` uses it with `clip_z = z_threshold`.
* **Fix:** `SewageNetwork` Rayleigh quotient normalised by `λ_max` (was `/2`, which only reached `[0, ½]`).
* **New:** `spectral_score_residual` (baseline-invariant network score on the mixing-scaled standardised residual), `residual_field`, `null_threshold` (deterministic empirical-null quantile), `laplacian()`, `sqrt_degrees()`.
* **New:** `AnomalyDetector::peek_z`, `with_spread_threshold` / `set_spread_threshold`, `DetectionSeverity::from_scores_with`.
* `spectral_score` (raw log₁₀ field) is kept for compatibility but documented as not baseline-invariant.
* 12 unit tests (exact Belize spectrum `{0, 1/21, 9/14, 1×5}`, score decomposition, warm-up count, robustness, severity table).

### Simulation (`ww_runner`)
* Network score computed from peeked z-scores (no raw-level input); upgrade threshold = null q95 of the score.
* Prints an SHPINN transport check per analyte category.

### New crate `ww_shpinn`
* Linear-feature physics-informed solver for `∂ₜc + v∂ₓc − D∂ₓₓc + kc = f` (exact least-squares, deterministic), data assimilation of measurements, analytic Gaussian reference, Theorem 7.1 constants (`kappa`, `stability_bound`).
* Hypergraph-regularised inverse source estimation (`prior_matrix`, `estimate_source`; Theorem 7.2 closed form).
* 7 unit tests, `examples/verify_forward.rs`.

### Docs
* README, doc comments (`lib.rs`, `analyte.rs`, `review.rs`, `log.rs`) corrected; `docs/ww_biosec_theory.md` added.

### Not changed
* The hypergraph is still built from catchments only; directed `site_flows_to` links are not used by the spectral model.
* `flow_liters` / `catchment_pop` are stored but do not enter detection.
* The audit log is an in-memory append-only vector (no hash chain / signatures).

### Measured effect on the shipped Belize scenario
| | v0.3 | v0.4 |
|---|---|---|
| alerts on pulse pairs / no-pulse pairs | 25 / 26 | 43 / 19 |
| injected pulses alerted (of 15) | 13 | 14 |
| spectral tier upgrades | 8 | 0 (calibrated threshold 0.966 never exceeded) |

## v0.3 — Explosive Precursor & Manufacturing Residue Surveillance

### New analyte category: `ExplosivePrecursor` (`[EXPX]`)

Seven analytes added to `ANALYTE_CATALOG` covering the primary wastewater-
detectable markers for energetic-material manufacture and post-blast
contamination:

| Analyte             | Marker        | Method     | k (d⁻¹) | z_thr |
|---------------------|---------------|------------|---------|-------|
| `RDX`               | RDX-LC-MSMS   | LC-MS/MS   | 0.02    | 3.5   |
| `HMX`               | HMX-LC-MSMS   | LC-MS/MS   | 0.01    | 3.5   |
| `TNT_aminoproducts` | 24ADNT-LC-MSMS| LC-MS/MS   | 0.08    | 3.5   |
| `PETN_penta`        | PENTA-LC-MSMS | LC-MS/MS   | 0.12    | 3.5   |
| `TATP_DHPP`         | DHPP-LC-MSMS  | LC-MS/MS   | 0.28    | 3.5   |
| `AN_nitrate_excess` | NO3-IC        | Ion Chrom. | 0.05    | 3.5   |
| `Perchlorate`       | ClO4-IC-MSMS  | IC-MS/MS   | 0.004   | 3.5   |

All use `z_threshold = 3.5` (AMBER unreachable; direct RED/CRITICAL entry),
consistent with the conservative false-positive policy for security referrals.

### Simulation scenario extensions

Three new outbreak pulses added to the Belize 15-day simulation:

* Days 10–13: RDX + HMX + TNT amino-products co-elevation at Belmopan
  Industrial Zone — simulated clandestine manufacturing cluster.
* Day 11: TATP-DHPP sharp spike at Belize City South — simulated
  improvised peroxide preparation event.
* Day 12: Perchlorate + AN excess at Orange Walk — simulated ANFO
  oxidiser sourcing.

### Analyst review escalation

New `ReviewOutcome::Escalate` arms added for all three explosive event types,
routing to:
- Belize National Security Council / Police Special Branch (RDX/HMX/TNT)
- Police Counter-Terrorism Unit (TATP)
- Pesticides Control Board + Police licence audit (AN/perchlorate)

### Theory document

`docs/ww_biosec_theory.md` §6 added covering:
- Compound selection and analytical methods
- Transport PDE kinetics (hydrolysis, biodegradation, sorption)
- Detection threshold rationale
- Co-elevation signature table for improved attribution
- Response escalation chain
- Limitations (no synthesis-route inference, matrix interference)

### Breaking changes

None. The `AnalyteCategory::from_str` fall-through default is
`InfectiousPathogen`, so old serialised data without `"explosive_precursor"`
round-trips correctly.
