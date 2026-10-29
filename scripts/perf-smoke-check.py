#!/usr/bin/env python3
"""PR6-F6 (P1-6) → CI-03 (review W1–W5) — per-commit perf smoke budget check.

Compares the fresh smoke cells — W1 point lookup + W2 write throughput +
W3 scan (the 2K matrix, result-smoke.json), W4 hot-head P50 (hot-head.md),
and W5 small compaction wall + allocs (compact-smoke.json) — against the
committed baseline with a generous 3x budget. The 3x is deliberate: it
catches O(n^2)-class regressions, not machine noise, and the budget stays
3x until enough CI runs pin the variance. The recall cell (ann004) is
self-asserting (recall@10 >= 9 vs the brute-force oracle), so it needs no
budget row.

Baseline shape (artifacts/storage-engine-v2/perf-smoke-baseline.json):
{
  "generated_at": "...", "machine": "...",
  "cells": {"w1_ko_get_p50_ns": ..., "w2_head_get_p50_ns": ...,
            "write_p50_ns": ..., "scan_p50_ns": ...,
            "hot_head_p50_ns": ..., "compact_wall_ms": ...,
            "compact_allocs": ...}
}
"""
import json
import re
import sys

from artifact_schema import (
    SchemaError,
    check_fresh,
    load,
    validate_1m,
    validate_smoke_cells,
)

BOUND = 3.0
# CI-03 follow-up (CI run 36099891181): the fsync-heavy wall cells don't
# hold the laptop-measured 3x on shared Windows runners — write measured
# 4.11x (3.07 ms vs 0.75 ms laptop), and the documented runner class is
# 4-7x the dev box on fsync-heavy tests. 8x still catches the smoke's
# charter (O(n^2)-class regressions, not machine noise); the structural
# cells (p50 lookups, allocs) keep 3x.
FSYNC_BOUND = 8.0
# The W6 write cell is judged per machine sync (check_write): the machine
# cancels out (measured 1.00-1.04 syncs per commit across runs whose
# absolute wall moved 25%), so it takes a same-machine bound — gate 5's
# 1.5x — which one extra sync per commit (2.0x) cannot pass.
SYNC_BOUND = 1.5
BASE = "artifacts/storage-engine-v2/perf-smoke-baseline.json"
SMOKE = "artifacts/storage-engine-v2/result-smoke.json"
HOTHEAD = "artifacts/storage-engine-v2/hot-head.md"
COMPACT = "artifacts/storage-engine-v2/compact-smoke.json"
BACKEND = "aikoql-v2"
SYNC_REF = "machine sync (ref)"


class PerfFail(Exception):
    pass


def die(msg):
    print(f"PERF SMOKE FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def check_write(fresh, cells):
    """The ingestion (W6) cell against the machine it ran on: its mean
    commit wall per put divided by a plain sync on the same disk in the
    same run, vs the baseline's write_per_sync at SYNC_BOUND. The
    absolute cell tracked the runner's disk (identical code 4.4x-9.7x the
    laptop baseline across CI runners); the ratio does not — a slow machine
    scales both, an extra sync per commit or super-linear work moves only
    the numerator. Returns the ratio; raises PerfFail over the budget."""
    write = fresh.get((BACKEND, "ingestion (W6)"))
    if write is None:
        raise PerfFail(f"'ingestion (W6)' missing from fresh smoke ({BACKEND})")
    sync = fresh.get((BACKEND, SYNC_REF))
    if not sync:
        raise PerfFail(f"{SYNC_REF!r} missing from fresh smoke ({BACKEND})")
    per_sync = write / sync
    want = cells["write_per_sync"]
    ratio = per_sync / want
    print(
        f"write_per_sync: {write:.0f} ns / {sync:.0f} ns sync = {per_sync:.2f} "
        f"vs baseline {want:.2f} = {ratio:.2f}x (bound {SYNC_BOUND}x)"
    )
    if ratio > SYNC_BOUND:
        raise PerfFail(f"ingestion (W6) {ratio:.2f}x per machine sync vs baseline — over the {SYNC_BOUND}x budget")
    return ratio


def main():
    try:
        cells = validate_smoke_cells(BASE)
    except SchemaError as e:
        die(f"baseline: {e}")
    try:
        fresh = validate_1m(SMOKE, fresh=True)
    except SchemaError as e:
        hint = " (did the run set STORAGE_PERF_SMOKE=1?)" if e.unreadable else ""
        die(f"{e}{hint}")

    for key, label in (
        ("w1_ko_get_p50_ns", "KO get (W1)"),
        ("w2_head_get_p50_ns", "head get (W2)"),
        ("scan_p50_ns", "type scan (W5)"),  # review W3 scan
    ):
        got = fresh.get((BACKEND, label))
        if got is None:
            die(f"{label!r} missing from fresh smoke ({BACKEND})")
        want = cells[key]
        ratio = got / want
        print(f"{key}: {got:.0f} ns vs baseline {want:.0f} ns = {ratio:.2f}x (bound {BOUND}x)")
        if ratio > BOUND:
            die(f"{label} {ratio:.2f}x vs baseline — over the {BOUND}x budget")
    # review W2 write throughput — judged per machine sync (check_write);
    # the absolute cell is reported, not gated
    write = fresh.get((BACKEND, "ingestion (W6)"))
    if write is not None:
        print(f"write_p50_ns: {write:.0f} ns vs laptop {cells['write_p50_ns']:.0f} ns = "
              f"{write / cells['write_p50_ns']:.2f}x (reported; the gate is write_per_sync)")
    try:
        check_write(fresh, cells)
    except PerfFail as e:
        die(str(e))

    try:
        with open(HOTHEAD, encoding="utf-8") as f:
            md = f.read()
    except OSError as e:
        die(f"hot-head report unreadable: {e}")
    m = re.search(r"- P50: (\d+) ns", md)
    if not m:
        die("hot-head P50 line missing from hot-head.md")
    got_ns = int(m.group(1))
    want_ns = cells["hot_head_p50_ns"]
    ratio = got_ns / want_ns
    print(f"hot_head_p50_ns: {got_ns} ns vs baseline {want_ns:.0f} ns = {ratio:.2f}x (bound {BOUND}x)")
    if ratio > BOUND:
        die(f"hot-head {ratio:.2f}x vs baseline — over the {BOUND}x budget")

    try:
        compact = load(COMPACT)
        check_fresh(COMPACT, compact)
    except SchemaError as e:
        die(f"compact smoke: {e}")
    for key in ("compact_wall_ms", "compact_allocs"):
        got = compact.get("cells", {}).get(key)
        if got is None:
            die(f"{key!r} missing from fresh compact smoke")
        want = cells[key]
        bound = FSYNC_BOUND if key == "compact_wall_ms" else BOUND
        ratio = got / want
        print(f"{key}: {got:.0f} vs baseline {want:.0f} = {ratio:.2f}x (bound {bound}x)")
        if ratio > bound:
            die(f"{key} {ratio:.2f}x vs baseline — over the {bound}x budget")
    print("PERF SMOKE OK")


if __name__ == "__main__":
    main()
