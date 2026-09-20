#!/usr/bin/env python3
"""PR6-F6 (P1-6) — per-commit perf smoke budget check.

Compares the fresh smoke cells — W1/W2 point reads (the 2K matrix,
result-smoke.json) and the hot-head P50 (hot-head.md) — against the
committed baseline with a generous 3x budget. The 3x is deliberate: it
catches O(n^2)-class regressions, not machine noise, and the budget stays
3x until enough CI runs pin the variance. The recall cell (ann004) is
self-asserting (recall@10 >= 9 vs the brute-force oracle), so it needs no
budget row.

Baseline shape (artifacts/storage-engine-v2/perf-smoke-baseline.json):
{
  "generated_at": "...", "machine": "...",
  "cells": {"w1_ko_get_p50_ns": ..., "w2_head_get_p50_ns": ...,
            "hot_head_p50_ns": ...}
}
"""
import json
import re
import sys

BOUND = 3.0
BASE = "artifacts/storage-engine-v2/perf-smoke-baseline.json"
SMOKE = "artifacts/storage-engine-v2/result-smoke.json"
HOTHEAD = "artifacts/storage-engine-v2/hot-head.md"
BACKEND = "aikoql-v2"


def die(msg):
    print(f"PERF SMOKE FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def p50_rows(path):
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    out = {}
    for backend in data.get("backends", []):
        for row in backend.get("rows", []):
            out[(backend.get("name"), row.get("label"))] = float(row["p50_us"])
    return out


def main():
    try:
        with open(BASE, encoding="utf-8") as f:
            base = json.load(f)
    except OSError as e:
        die(f"baseline unreadable: {e}")
    cells = base.get("cells")
    if not cells:
        die("baseline has no cells block")
    try:
        fresh = p50_rows(SMOKE)
    except OSError as e:
        die(f"fresh smoke artifact unreadable: {e} (did the run set V2ADOPT_PERF_SMOKE=1?)")

    for key, label in (
        ("w1_ko_get_p50_ns", "KO get (W1)"),
        ("w2_head_get_p50_ns", "head get (W2)"),
    ):
        got = fresh.get((BACKEND, label))
        if got is None:
            die(f"{label!r} missing from fresh smoke ({BACKEND})")
        want = float(cells[key])
        ratio = got / want
        print(f"{key}: {got:.0f} ns vs baseline {want:.0f} ns = {ratio:.2f}x (bound {BOUND}x)")
        if ratio > BOUND:
            die(f"{label} {ratio:.2f}x vs baseline — over the {BOUND}x budget")

    try:
        with open(HOTHEAD, encoding="utf-8") as f:
            md = f.read()
    except OSError as e:
        die(f"hot-head report unreadable: {e}")
    m = re.search(r"- P50: (\d+) ns", md)
    if not m:
        die("hot-head P50 line missing from hot-head.md")
    got_ns = int(m.group(1))
    want_ns = float(cells["hot_head_p50_ns"])
    ratio = got_ns / want_ns
    print(f"hot_head_p50_ns: {got_ns} ns vs baseline {want_ns:.0f} ns = {ratio:.2f}x (bound {BOUND}x)")
    if ratio > BOUND:
        die(f"hot-head {ratio:.2f}x vs baseline — over the {BOUND}x budget")
    print("PERF SMOKE OK")


if __name__ == "__main__":
    main()
