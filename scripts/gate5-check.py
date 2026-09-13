#!/usr/bin/env python3
"""P5-M0 (gd001) — gate-5 slow-down ratio check (v2 vs committed v1 baseline).

Ratio basis (SE2-M28 / gate 5): the shipped 1M runs. The harness writes the
fresh v2 rows to `result-1m-aikoql-v2.json` when AIKOQL_REPORT_WRITE=1; the
v1 baseline is the committed `result-1m-aikoql.json`. Single-backend runs
leave the harness's own gate verdict null BY DESIGN (it only judges the full
4-backend matrix) — this script computes the ratio the harness cannot.

Shared by the baseline-guard CI job and the manual 1M procedure:

    V2ADOPT_NIGHTLY=1m V2ADOPT_BACKEND=aikoql-v2 AIKOQL_REPORT_WRITE=1 \
        cargo test -p aikoql-storage-v2 --release --test kse_m7_v2_workloads
    python scripts/gate5-check.py
"""
import argparse
import json
import sys

BOUND = 8.0  # GATE5_SLOWDOWN_BOUND — design gate, SE2-M22 (user decision)
REDLINE_FRAC = 0.99  # W1 shipped at 7.96x = 99.5% of the bound — no headroom


def p50(path, label):
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    for backend in data.get("backends", []):
        for row in backend.get("rows", []):
            if row.get("label") == label:
                return float(row["p50_us"])
    raise SystemExit(f"{label!r} not found in {path}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--fresh", default="artifacts/storage-engine-v2/result-1m-aikoql-v2.json")
    ap.add_argument("--baseline", default="artifacts/storage-engine-v2/result-1m-aikoql.json")
    ap.add_argument("--bound", type=float, default=BOUND)
    args = ap.parse_args()

    rows = []
    worst = 0.0
    for label in ("KO get (W1)", "head get (W2)"):
        f, b = p50(args.fresh, label), p50(args.baseline, label)
        ratio = f / b
        rows.append((label, f, b, ratio))
        worst = max(worst, ratio)

    for label, f, b, r in rows:
        flag = "REDLINE" if r >= args.bound * REDLINE_FRAC else ("FAIL" if r > args.bound else "pass")
        print(f"{label:14} v2 {f:>9.1f} us / v1 {b:>8.1f} us = {r:5.2f}x  {flag}")
    print(f"gate 5 bound: <= {args.bound}x (design gate, SE2-M22)")

    if worst > args.bound:
        print(f"GATE 5 FAIL: worst ratio {worst:.2f}x exceeds bound {args.bound}x")
        sys.exit(1)
    if worst >= args.bound * REDLINE_FRAC:
        print(f"REDLINE: worst ratio {worst:.2f}x is within 1% of the bound - no headroom left")
    print("GATE 5 PASS")


if __name__ == "__main__":
    main()
