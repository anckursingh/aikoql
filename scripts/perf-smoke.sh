#!/usr/bin/env bash
# PR6-F6 (P1-6): per-commit perf smoke — three fixed cells with a generous
# 3x budget vs the committed baseline. The cheap version of gate-5: the
# 1M ratio guard stays in benchmark.yml's guard job (nightly/manual class).
#
#   W1/W2 point reads : the 2K smoke matrix writes result-smoke.json
#                       (STORAGE_PERF_SMOKE=1 — the arm that makes a smoke
#                       run machine-readable; the -smoke suffix never
#                       clobbers the canonical artifacts)
#   hot-head          : SE2M11_NIGHTLY=1 writes hot-head.md (100K cached
#                       head lookups, answers pinned per lookup)
#   recall            : ann004 self-asserts recall@10 >= 9 vs the brute-
#                       force oracle at N=10001 — no budget row needed
#
# The budget comparison (scripts/perf-smoke-check.py) reads the two fresh
# artifacts against artifacts/storage-engine-v2/perf-smoke-baseline.json.
set -euo pipefail
cd "$(dirname "$0")/.."

STORAGE_PERF_SMOKE=1 AIKOQL_REPORT_WRITE=1 \
    cargo test -p aikoql-storage-v2 --release --test kse_m7_v2_workloads

SE2M11_NIGHTLY=1 AIKOQL_REPORT_WRITE=1 \
    cargo test -p aikoql-storage-v2 --release --test hot_head_gate

AIKOQL_ANN_EVIDENCE=10001 \
    cargo test -p aikoql-kernel --release --test ann_production \
        ann004_evidence_cell_past_capacity

python scripts/perf-smoke-check.py
