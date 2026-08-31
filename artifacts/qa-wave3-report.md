# AikoQL QA Wave 3 Report

> generated from TESTING-PLAN.md §11.1 by scripts/certify.js

Registry: `docs/TESTING-PLAN.md` §11.1 (Wave 3 market-reality plan). Every number is measured by a committed, deterministic, LLM-free test; negative evidence lives in `docs/benchmarks/{wins,parity,losses,unknown}.md` (plan §29). Statuses are never invented.

## Summary

- W3-MKT-001 Market corpus: PASS
- W3-WIN-001 Workload classification: PASS
- W3-TEMP-001 Temporal reality: PASS
- W3-CONF-001 Contradiction value: PASS
- W3-UNK-001 Unknown handling: PASS
- W3-LONG-001 Longitudinal: PASS
- W3-DEBUG-001 Debuggability: PASS
- W3-DEV-001 Build-vs-buy: PASS
- W3-DEV-002 Source expansion: PASS
- **Final decision: GO** (0 blocking items)

## Wave 3 gate readout (W3-G01..G07)

| Gate | Requirement | Verdict |
| --- | --- | --- |
| W3-G01 | Wave-2 release gate remains GO (no regression) | PASS |
| W3-G02 | 100% corpus integrity (MKT/TEMP/CONF/UNK/LONG/DEBUG rows pass) | PASS |
| W3-G03 | Comparative results reproducible (documented §28 recipe) | PASS |
| W3-G04 | ≥1 Strong-Fit class with a measured margin | PASS |
| W3-G05 | No unsupported universal claims in evidence docs | PASS |
| W3-G06 | Build-vs-buy quantifies application complexity | PASS |
| W3-G07 | Negative evidence preserved (wins/parity/losses/unknown) | PASS |

## Per-ID results

| ID | Pri | Gate | Status | Coverage / TDD item |
| --- | --- | --- | --- | --- |
| W3-MKT-001 Market corpus | P0 | W3-G02 | PASS | `w3_mkt_001_market_corpus_integrity` (ingestion) — 19 docs / 34 chunks / 13 questions, W1-W12 labeled, every answer unit verbatim-backed by chunk text (rig check) |
| W3-WIN-001 Workload classification | P0 | W3-G04 | PASS | `w3_win_001_workload_classification` (ingestion) — W4 multi-hop 7/8 vs RAG 3/8 (Strong Fit), W7 provenance 2/2 vs 1/2 (Strong Fit), W11 Good Fit, W1/W3/W5/W6/W9 parity, W2 unknown |
| W3-TEMP-001 Temporal reality | P0 | W3-G02 | PASS | `w3_temp_001_temporal_market_reality` (ingestion) — 2026 timeline (assert → supersede → supersede): 2/2 superseded claims+entities suppressed vs RAG 2/2 confusion; valid_to stamps + lineage preserved |
| W3-CONF-001 Contradiction value | P0 | W3-G02 | PASS | `w3_conf_001_contradiction_value` (ingestion) — policy-only pack (1/3) vs RAG 3/3 incl. the unsafe note; superseded claims stay readable |
| W3-UNK-001 Unknown handling | P0 | W3-G02 | PASS | `w3_unk_001_unknown_handling_classification` (ingestion) — 4 classes (known/unknown/conflicting/historical-only) + honest false-confidence probe (5/5 facts on an absent answer, losses.md) |
| W3-LONG-001 Longitudinal | P1 | W3-G02 | PASS | `w3_long_001_longitudinal_value` (ingestion) — 90-day capacity evolution: 4/4 checkpoints at flat tokens [27,27,27,27] vs RAG 1/4 and history 1/4 with growing tokens |
| W3-DEBUG-001 Debuggability | P1 | W3-G02 | PASS | `w3_debug_001_observability_root_cause` (ingestion) — 5 injected failures surfaced by deterministic kernel reads (5/5 diagnosed); evidence-free assertion fails closed (P0-1 held) |
| W3-DEV-001 Build-vs-buy | P0 | W3-G06 | PASS | `docs/WAVE3-MARKET-EVIDENCE.md` §4 — retrieval-only baseline 1,042 LOC vs 9,410 LOC engine surface (kernel 6,590 + graph 551 + compiler/runtime 2,269); moat table |
| W3-DEV-002 Source expansion | P1 | W3-G06 | PASS | `docs/WAVE3-MARKET-EVIDENCE.md` §4 — one `compile_file` dispatch: md/pdf/rust/python/ts/java/images + 8 storage adapters behind one kernel API |

## Evidence docs (plan §28–§30)

- `docs/WAVE3-MARKET-EVIDENCE.md` — coverage matrix, reproduction recipe, evidence matrix, build-vs-buy
- `docs/benchmarks/{wins,parity,losses,unknown}.md` — the negative-evidence set (W3-G07)
