#!/usr/bin/env python3
"""Diff the alerts the Axum/PostgreSQL service raised against the reference
`scotland_e2e` binary. Compares the first 7 columns exactly (site, week,
log10, z, spectral, severity, n_obs), all formatted to 3 dp by both sides."""
import csv, sys

def load(path):
    with open(path, newline="") as f:
        r = csv.reader(f)
        next(r)
        return [tuple(row[:7]) for row in r]

ref, api = load(sys.argv[1]), load(sys.argv[2])
ref_set, api_set = set(ref), set(api)
print(f"reference alerts : {len(ref)}")
print(f"API/Postgres     : {len(api)}")
only_ref, only_api = sorted(ref_set - api_set), sorted(api_set - ref_set)
print(f"only in reference: {len(only_ref)}")
print(f"only in API      : {len(only_api)}")
for tag, rows in (("REF ", only_ref), ("API ", only_api)):
    for row in rows[:10]:
        print(f"  {tag}{row}")
if ref == api:
    print("RESULT: IDENTICAL (same rows, same order)")
    sys.exit(0)
print("RESULT: MISMATCH")
sys.exit(1)
