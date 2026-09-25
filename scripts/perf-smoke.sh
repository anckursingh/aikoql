#!/usr/bin/env bash
# PR6-F6 (P1-6) → CI-03 (review W1–W5): per-commit perf smoke — five cells
# with a generous 3x budget vs the committed baseline. The cheap version
# of gate-5: the 1M ratio guard stays in benchmark.yml's guard job
# (nightly/manual class).
#
#   W1 point lookup    : the 2K smoke matrix's "KO get (W1)" row
#                        (result-smoke.json)
#   W2 write throughput: the matrix's "ingestion (W6)" row — the mean
#                        commit wall per put (result-smoke.json)
#   W3 scan            : the matrix's "type scan (W5)" row
#                        (result-smoke.json)
#   W4 hot-cache       : SE2M11_NIGHTLY=1 writes hot-head.md (100K cached
#                        head lookups, answers pinned per lookup)
#   W5 small compaction: STORAGE_PERF_SMOKE=1 compaction_smoke — one merge
#                        of two 25K-key segments under a counting
#                        allocator; wall + allocs (compact-smoke.json,
#                        stamped with the tested HEAD)
#   recall             : ann004 self-asserts recall@10 >= 9 vs the brute-
#                        force oracle at N=10001 — no budget row needed
#
# The budget comparison (scripts/perf-smoke-check.py) reads the fresh
# artifacts against artifacts/storage-engine-v2/perf-smoke-baseline.json.
set -euo pipefail
cd "$(dirname "$0")/.."

STORAGE_PERF_SMOKE=1 AIKOQL_REPORT_WRITE=1 \
    cargo test -p aikoql-storage-v2 --release --test kse_m7_v2_workloads

SE2M11_NIGHTLY=1 AIKOQL_REPORT_WRITE=1 \
    cargo test -p aikoql-storage-v2 --release --test hot_head_gate

STORAGE_PERF_SMOKE=1 \
    cargo test -p aikoql-storage-v2 --release --test compaction_smoke

AIKOQL_ANN_EVIDENCE=10001 \
    cargo test -p aikoql-kernel --release --test ann_production \
        ann004_evidence_cell_past_capacity

python scripts/perf-smoke-check.py
