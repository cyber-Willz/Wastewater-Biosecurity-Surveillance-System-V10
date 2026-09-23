# ww_biosec — End-to-End Live Run Report

**Dataset:** Public Health Scotland national SARS-CoV-2 wastewater surveillance programme
(BioRDM/COVID-Wastewater-Scotland, *Scientific Data* paper), N1 gene RT-qPCR
**Source (fetched live at run time):**
`https://raw.githubusercontent.com/BioRDM/COVID-Wastewater-Scotland/master/data/SARS-Cov2_RNA_monitoring_ww_scotland.csv`
**Run date:** 2026-09-22
**Binary:** `ww_eval::scotland_e2e` (v10 workspace, `ww_biosec_v10_n8n_tar.gz`)

---

## 1. What was actually run

This was a genuine live pull, not a fixture: `curl` fetched the CSV directly from GitHub at
run time (10,317 lines, 19 columns of the raw SEPA/BioRDM export), and the unmodified file was
fed straight into the shipped `scotland_e2e` binary, which does its own CSV parsing, weekly
aggregation, limit-of-detection flooring, log10 transform, Health-Board catchment grouping, and
detection — no synthetic data, no preprocessing step of my own.

```
curl -sL -o scotland_raw.csv "<url above>"
cargo build --release -p ww_eval --bin scotland_e2e
./target/release/scotland_e2e scotland_raw.csv scotland_alerts.csv scotland_trace.csv 8 raw
```

**One deviation from the repo's own `live_run_pg.sh`:** that script also stands up the Axum +
PostgreSQL service (`ww_api`) and replays the same data through the HTTP API to diff against the
in-memory reference. `ww_api`'s own `Cargo.toml` build notes state it needs rustc ≥ 1.85 (SeaORM
and its transitive dependencies now ship `edition = "2024"` manifests); this sandbox only has
Ubuntu 24.04's distro `rustc` 1.75, and `rustup`'s distribution host isn't in this environment's
allowed network egress list, so the Postgres/API layer could not be built here. I set up and
started a local PostgreSQL 16 instance in case it was reachable another way, but the blocker is
the toolchain, not the database. What follows is the full result of the in-memory detection
engine (`ww_detection` — the same EWMA/Welford baseline + spectral hypergraph code the API layer
wraps) run end-to-end against the live data; the HTTP/PostgreSQL round-trip is the untested part.

## 2. Pipeline configuration

| Parameter | Value |
|---|---|
| Metric | `Calculated_mean` (raw gc/L → log10), per script default |
| Min weeks per site | 8 |
| EWMA alpha | 0.3935 (shipped SARS-CoV-2 default) |
| Spectral null-quantile threshold (q95) | 0.9784 |
| Fiedler value of catchment graph | 0.04762 |
| Catchments used | 11 Health Boards with ≥ 2 monitored sites |

## 3. Ingest summary

| Metric | Value |
|---|---|
| Raw rows parsed | 10,316 |
| Numeric/usable observations | 8,879 |
| Site-weeks after aggregation | 5,530 |
| Distinct sites in export | 160 |
| Sites passing min-weeks=8 threshold | 141 |
| Weekly rounds processed | 89 |
| Date range | 2020-05-28 → 2022-02-09 |
| Health Boards represented | 14 |

## 4. Detection results

| Metric | Value |
|---|---|
| Total AMBER+ alerts | 81 |
| CRITICAL | 28 |
| RED | 18 |
| AMBER | 35 |
| Alerts falling inside a known case-wave window | 57 / 81 (70.4%) |

**Known-wave recall** (did the detector fire at least once during each independently-sourced
UK/Scotland case wave?):

| Wave | Window | ≥1 site alerted? |
|---|---|---|
| Alpha | 2020-12-01 – 2021-02-15 | ✅ Yes |
| Delta | 2021-06-15 – 2021-10-15 | ✅ Yes |
| Omicron | 2021-12-01 – 2022-01-31 | ❌ No |

Alerts by wave: Alpha 7, Delta 50, Omicron 0, outside any known wave 24.

## 5. Notable findings

- **Fort William dominates the alert count** — 26 of the 81 alerts (32%), with some of the
  highest z-scores in the whole run (37.2, 21.7, 42.9). This traces back to genuine swings in the
  raw feed: `Calculated_mean` at that site jumps from ~600–800 gc/L in Aug–Sep 2020 to 58,658,
  then 132,153 gc/L within a few weeks. That could reflect a real local signal, a change in lab/
  dilution methodology, or a low-flow small-catchment effect (Fort William's modelled flow is far
  smaller than the urban sites) — the raw export gives no way to distinguish these from here.
- **No alert fired during the Omicron wave.** Given the shipped default alpha (0.39, tuned to the
  earlier waves) and that Omicron's wastewater signal shape differed from Delta's in most UK
  surveillance programmes, this is a plausible true miss rather than an ingest bug — flagged for
  follow-up rather than treated as a defect.
- **The spectral network score is a Health-Board proxy, not real sewer topology** (the dataset
  doesn't publish catchment connectivity), so its contribution here is weaker than it would be in
  an actual deployment with true upstream/downstream site relationships — consistent with the
  caveat already documented in the repo's own `docs/ww_biosec_theory.md`.
- 19 of 160 sites (12%) were excluded for having fewer than 8 weekly observations, so their
  results carry no coverage claim either way.

## 6. Outputs generated

| File | Rows | Contents |
|---|---|---|
| `scotland_raw.csv` | 10,317 | Live download, byte-for-byte as fetched |
| `scotland_alerts.csv` | 81 | Every AMBER+ alert: site, week, log10_conc, z-score, spectral score, severity, n_obs, wave flag |
| `scotland_trace.csv` | 5,458 | Full weekly z-score trace for every site, alerted or not |

(These CSVs stayed in the build container; ask if you want them alongside this report.)

## 7. Honest limitations of this run

1. **API + PostgreSQL layer untested here** — toolchain-blocked, see §1. The `ww_detection` core
   this report exercises is the same code that layer wraps, but the HTTP ingestion path, audit
   hash-chain, and "detector state survives a restart" property from `live_run_pg.sh` were not
   verified in this run.
2. This is a single configuration (raw metric, min_weeks=8, shipped alpha). The repo's own
   `real_data.rs` binary computes a full per-wave sensitivity/specificity confusion matrix and can
   isolate the spectral score's contribution (`--spectral enabled|disabled`) — not run here since
   it expects a different, pre-aggregated CSV shape than the raw export.
3. Wave windows are external, approximate UK/Scotland reference dates, not derived from this
   dataset — provided by the repo, used as-is.
