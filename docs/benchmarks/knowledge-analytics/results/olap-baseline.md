# OLAP baseline results — Phase C (2026-08-30)

Harness: `crates/ingestion/tests/wave5_olap.rs` — live engines, NOT stubs.
Environment: Docker Desktop on the dev box (loopback transport),
ClickHouse 24.8 alpine, StarRocks 3.3 allin1, debug build, min-of-3
warm-cache queries.

These numbers certify **reproducibility and correctness** of the baseline
(plan gates W5-G01/G02). They are NOT a product-vs-product performance
contest: one machine, a debug harness, different engine architectures.
Reading cross-engine ms comparisons beyond order-of-magnitude would be a
misreading.

Load (deterministic, 12.1M rows total): ClickHouse 576ms, StarRocks 14705ms
(allin1's load includes its cross-join generator and first-touch costs).
Re-measured 2026-08-30 (strict-opt-in run, both engines in one process).

| Task | ClickHouse | StarRocks | Check |
| --- | --- | --- | --- |
| W5-OLAP-001 large aggregation (10M rows → 100k groups) | 311ms | 562ms | grand total 4,995,000,000 CORRECT |
| W5-OLAP-002a events/day per service | 14ms | 48ms | 240 buckets, 1M total CORRECT |
| W5-OLAP-002b error rate per service | 12ms | 62ms | 10,000 errs, all service 0 CORRECT |
| W5-OLAP-002c p95 latency per service | 22ms | 135ms | exact p95 = service+460 CORRECT (SR via percentile_approx, ±10 contract) |
| W5-OLAP-003 high-cardinality GROUP BY (1M rows → 12k combos) | 80ms | 232ms | 12,000 combos, 1M total CORRECT |
| W5-OLAP-004a tier join (10M⋈100k) | 196ms | 136ms | 3 tiers CORRECT |
| W5-OLAP-004b device join (1M⋈1M) | 453ms / 10ms | 60ms / 18ms | 1M matched, spots CORRECT |

**AIKOQL leg: NOT_MEASURED on all four tasks.** The substrate has no
columnar scan path (redb, row-at-a-time); the plan's §7 build-vs-buy rule
says delegate to OLAP, not re-implement it. Recorded as a loss
(losses.md), never as a win — this is the boundary the benchmark exists to
discover (§28).

Cross-check dividend: both engines agree with the Rust ground truth on
every figure, and the RED cycle caught three test-side ground-truth bugs
first (chunked-HTTP parsing, generator-coupled spot tuples, the day-0
count). The engines were right; the harness was wrong — exactly what a
ground-truth-first benchmark is for.
