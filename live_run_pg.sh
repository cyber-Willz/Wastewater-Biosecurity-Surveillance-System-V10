#!/usr/bin/env bash
# End-to-end LIVE run of the Axum + PostgreSQL service.
#
#   1. builds ww_api, scotland_live and the reference scotland_e2e
#   2. fetches the live Public Health Scotland SARS-CoV-2 wastewater dataset
#   3. starts the Axum server against PostgreSQL (isolated schema per run)
#   4. runs the reference pipeline, then drives the SAME data through the HTTP API
#   5. diffs the alerts stored in PostgreSQL against the reference, row for row
#   6. restarts the server (detector state must be rebuilt from PostgreSQL) and
#      proves the baselines survived, then exercises review + audit trail
#
# Requires: DATABASE_URL (e.g. postgres://user:pw@127.0.0.1/ww_biosec), psql, curl, python3.
# Usage:    DATABASE_URL=... ./live_run_pg.sh [min_weeks]
set -euo pipefail

: "${DATABASE_URL:?set DATABASE_URL, e.g. postgres://ww:pw@127.0.0.1/ww_biosec}"
MIN_WEEKS="${1:-8}"
PORT="${WW_PORT:-8080}"
export WW_BIND="127.0.0.1:${PORT}"
export WW_API_URL="http://${WW_BIND}"
export WW_API_TOKEN="${WW_API_TOKEN:-$(head -c 18 /dev/urandom | base64 | tr -dc 'A-Za-z0-9')}"
export WW_PG_SCHEMA="${WW_PG_SCHEMA:-ww_live_$(date +%Y%m%d_%H%M%S)}"
DATA_URL="https://raw.githubusercontent.com/BioRDM/COVID-Wastewater-Scotland/master/data/SARS-Cov2_RNA_monitoring_ww_scotland.csv"
OUT="${OUT_DIR:-live_run_out}"; mkdir -p "$OUT"
SRV_PID=""

say()  { printf '\n==> %s\n' "$*"; }
api()  { curl -sS -H "Authorization: Bearer $WW_API_TOKEN" -H 'Content-Type: application/json' "$@"; }
psqlx(){ PGOPTIONS="-c search_path=$WW_PG_SCHEMA" psql "$DATABASE_URL" -v ON_ERROR_STOP=1 "$@"; }
start_server() {
  ./target/release/ww_api >>"$OUT/server.log" 2>&1 &
  SRV_PID=$!
  for _ in $(seq 1 50); do curl -sf "$WW_API_URL/healthz" >/dev/null && return 0; sleep 0.2; done
  echo "server failed to start; see $OUT/server.log" >&2; exit 1
}
stop_server()  { [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null && wait "$SRV_PID" 2>/dev/null || true; SRV_PID=""; }
trap stop_server EXIT

say "Building (release)"
cargo build --release -p ww_api -p ww_eval 2>&1 | grep -E 'error|warning: unused|Finished' || true

say "Fetching live dataset"
curl -sSL -o "$OUT/scotland_raw.csv" "$DATA_URL"
echo "    $(wc -l < "$OUT/scotland_raw.csv") lines"

say "Starting Axum server (schema: $WW_PG_SCHEMA)"
: >"$OUT/server.log"; start_server
api "$WW_API_URL/v1/summary"; echo

say "Reference run (in-memory scotland_e2e)"
./target/release/scotland_e2e "$OUT/scotland_raw.csv" "$OUT/ref_alerts.csv" "$OUT/ref_trace.csv" "$MIN_WEEKS" raw 2>&1 | grep -E 'parsed|sites used|rounds|total AMBER'

say "Live run: same data through HTTP -> Axum -> PostgreSQL"
./target/release/scotland_live "$OUT/scotland_raw.csv" "$OUT/api_alerts.csv" "$MIN_WEEKS" 2>&1 | grep -v '^$'

say "Diff: alerts stored in PostgreSQL vs reference"
python3 ww_api/scripts/compare_alerts.py "$OUT/ref_alerts.csv" "$OUT/api_alerts.csv"

say "Restart server: detector must be rebuilt by replaying PostgreSQL"
stop_server; start_server
grep 'engine rebuilt' "$OUT/server.log" | tail -1 | sed 's/\x1b\[[0-9;]*m//g'

# Post one more (synthetic) week for the site with the longest history, with a
# large spike. A warm-baseline alert (n_obs ~ history length, not ~0) proves the
# EWMA state survived the restart.
say "Post-restart spike on the best-observed site"
read -r SITE NEXT_WEEK EWMA_HINT < <(psqlx -Atc "
  SELECT o.site_id, (max(o.observed_on) + 7)::text, round(avg(o.log10_conc)::numeric,3)
  FROM observations o GROUP BY o.site_id ORDER BY count(*) DESC, o.site_id LIMIT 1" | tr '|' ' ')
HISTORY=$(psqlx -Atc "SELECT count(*) FROM observations WHERE site_id='$SITE'")
echo "    site=$SITE history=$HISTORY obs, next week=$NEXT_WEEK, mean log10=$EWMA_HINT"
SPIKE=$(python3 -c "print(float('$EWMA_HINT')+2.5)")
api -X POST "$WW_API_URL/v1/rounds" -d "{\"analyte\":\"SARS-CoV-2\",\"observed_on\":\"$NEXT_WEEK\",
  \"observations\":[{\"site_id\":\"$SITE\",\"log10_conc\":$SPIKE}],\"source\":\"synthetic-spike\",\"actor\":\"live_run\"}" \
  | python3 -c "
import sys,json; r=json.load(sys.stdin); a=r['alerts']
assert a, 'expected an alert'; a=a[0]
print(f\"    alert #{a['alert_id']} {a['severity']}  z={a['z_score']:.2f}  baseline n_obs={a['n_obs']}\")
assert a['n_obs'] >= $HISTORY, 'baseline was cold after restart!'
print('    OK: baseline state survived the restart (warm, n_obs >= history)')"
SPIKE_ID=$(psqlx -Atc "SELECT max(alert_id) FROM alerts")

say "Out-of-order round is rejected (EWMA is order-dependent)"
api -s -o /dev/null -w '    HTTP %{http_code}\n' -X POST "$WW_API_URL/v1/rounds" -d "{\"analyte\":\"SARS-CoV-2\",\"observed_on\":\"2020-01-06\",
  \"observations\":[{\"site_id\":\"$SITE\",\"log10_conc\":3.0}]}"

say "Analyst review workflow"
OPEN=$(api "$WW_API_URL/v1/alerts?status=OPEN&severity=CRITICAL&limit=3")
IDS=$(python3 -c "import json,sys; print(' '.join(str(a['alert_id']) for a in json.loads(sys.argv[1])))" "$OPEN")
echo "    open CRITICAL alerts (first 3): $IDS"
set -- $IDS
api -X POST "$WW_API_URL/v1/alerts/$SPIKE_ID/review" -d '{"analyst":"Dr. Martinez","outcome":"confirm","notes":"Consistent with community wave"}' \
  | python3 -c "import sys,json; a=json.load(sys.stdin); print(f\"    #{a['alert_id']} -> {a['status']} by {a['reviewed_by']}\")"
[ -n "${1:-}" ] && api -X POST "$WW_API_URL/v1/alerts/$1/review" -d '{"analyst":"Dr. Okafor","outcome":"escalate","to_team":"Regional Epi","notes":"Needs sequencing"}' \
  | python3 -c "import sys,json; a=json.load(sys.stdin); print(f\"    #{a['alert_id']} -> {a['status']}  notes={a['analyst_notes']!r}\")"
echo "    re-reviewing a closed alert:"
api -s -o /dev/null -w '    HTTP %{http_code} (expected 409)\n' -X POST "$WW_API_URL/v1/alerts/$SPIKE_ID/review" -d '{"analyst":"x","outcome":"dismiss","notes":"again"}'

say "Evidence chain for alert #$SPIKE_ID"
api "$WW_API_URL/v1/alerts/$SPIKE_ID/evidence" | python3 -c "
import sys,json; e=json.load(sys.stdin)
print('    site      :', e['site']['site_id'], '| region:', e['site']['region'], '| catchments:', e['site']['catchments'])
print('    round     :', e['round']['observed_on'], f\"spectral={e['round']['spectral_score']:.3f} thr={e['round']['spread_threshold']:.3f}\")
print('    audit     :', [(t['actor'], t['action']) for t in e['audit_trail']])"

say "Audit log is append-only at the database level"
psqlx -c "UPDATE audit_log SET actor='mallory' WHERE audit_id=1" 2>&1 | sed 's/^/    /' | head -2 || true
psqlx -c "DELETE FROM audit_log" 2>&1 | sed 's/^/    /' | head -2 || true
psqlx -c "TRUNCATE audit_log" 2>&1 | sed 's/^/    /' | head -2 || true
api "$WW_API_URL/v1/audit/export" > "$OUT/audit.ndjson"
echo "    exported $(wc -l < "$OUT/audit.ndjson") audit entries -> $OUT/audit.ndjson"

say "Final PostgreSQL state"
psqlx -c "SELECT (SELECT count(*) FROM monitoring_sites) sites, (SELECT count(*) FROM catchments) catchments,
  (SELECT count(*) FROM rounds) rounds, (SELECT count(*) FROM observations) observations,
  (SELECT count(*) FROM alerts) alerts, (SELECT count(*) FROM audit_log) audit_entries"
psqlx -c "SELECT severity, status, count(*) FROM alerts GROUP BY 1,2 ORDER BY 1,2"
echo; echo "Done. Schema '$WW_PG_SCHEMA' left in place for inspection; outputs in $OUT/"
