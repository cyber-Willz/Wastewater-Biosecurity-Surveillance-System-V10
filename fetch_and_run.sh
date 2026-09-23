#!/usr/bin/env bash
# End-to-end live run: fetches the live Public Health Scotland national
# SARS-CoV-2 wastewater surveillance dataset (BioRDM/COVID-Wastewater-Scotland
# on GitHub) and runs it through the real ww_detection engine.
#
# Usage:
#   ./fetch_and_run.sh [min_weeks] [raw|normalized] [alpha]
#
# Outputs (in the current directory):
#   scotland_raw.csv     - the live dataset, as downloaded
#   scotland_alerts.csv  - every AMBER+ alert the detector raised
#   scotland_trace.csv   - the full weekly z-score trace for every site,
#                           whether it alerted or not
set -euo pipefail

DATA_URL="https://raw.githubusercontent.com/BioRDM/COVID-Wastewater-Scotland/master/data/SARS-Cov2_RNA_monitoring_ww_scotland.csv"
MIN_WEEKS="${1:-8}"
METRIC="${2:-raw}"
ALPHA="${3:-}"

echo "==> Fetching live dataset from $DATA_URL"
curl -sL -o scotland_raw.csv "$DATA_URL"
n_lines=$(wc -l < scotland_raw.csv)
echo "    downloaded scotland_raw.csv ($n_lines lines)"

echo "==> Building ww_eval (release)"
cargo build --release -p ww_eval --bin scotland_e2e

echo "==> Running end-to-end pipeline (min_weeks=$MIN_WEEKS metric=$METRIC alpha=${ALPHA:-shipped default})"
./target/release/scotland_e2e scotland_raw.csv scotland_alerts.csv scotland_trace.csv \
  "$MIN_WEEKS" "$METRIC" "$ALPHA"

echo "==> Done. See scotland_alerts.csv and scotland_trace.csv"
