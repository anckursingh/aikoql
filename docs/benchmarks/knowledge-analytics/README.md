# Knowledge Analytics Benchmark — Wave 5

Boundary-discovery benchmark (plan `../AIKOQL_Wave5_Knowledge_Analytics_vs_OLAP_TDD_Test_Plan.md`).
Status: **knowledge side GREEN, OLAP baseline GREEN (Phase C measured), federation NOT_IMPLEMENTED (honest).**

## What ran (2026-08-30)

Knowledge side (Phase A, `crates/ingestion/tests/wave5_ka.rs`) — five P0
tests, deterministic and LLM-free:

| Test | Question | Result |
| --- | --- | --- |
| W5-KA-001 | Which customers are affected by the LedgerService outage, and why? | 6/6 hops, 0 false, 0 missed, ~391µs, 12 app LOC; mechanical RAG pack: 2/5 chain chunks + 1 false chunk |
| W5-KA-003 | Why is Customer X high risk? | Derivation record complete (how/by-whom/when/from-what/why); premise superseded → classification swept; lineage intact, ~375µs |
| W5-KA-004 | CRM ACTIVE vs Fraud BLOCKED vs Policy | Effective state BLOCKED, conflict disclosed, authority selection correct, loser traceable |
| W5-KA-006 | How many high-risk customers have ≥2 independent sources? | 5 customers → answer 1 (source-independence + temporal validity enforced), ~296µs, 13 app LOC |
| W5-KA-008 | What did we know about Customer X on date T? | t=500 ACTIVE(crm), t=1500 SUSPENDED(billing), t=2500 ACTIVE(audit); in-place versions separated by get_as_of, ~330µs |

Three further Phase A rows (KA-002 temporal, KA-005 unknown, KA-007 change
impact) are pointer rows to Wave 3.1 closures — plan §3 forbids duplication.

OLAP baseline (Phase C, `crates/ingestion/tests/wave5_olap.rs`) — four P0
tasks against live ClickHouse 24.8 and StarRocks 3.3 via
`docker compose --profile olap up -d clickhouse starrocks` (dev profile,
plan §4), same dataset/tasks/ground truth per plan §23:

| Test | ClickHouse | StarRocks | Result |
| --- | --- | --- | --- |
| W5-OLAP-001 large aggregation (10M rows → 100k groups) | 390ms | 561ms | grand total 4,995,000,000 CORRECT |
| W5-OLAP-002 time-series (1M events) | 34/24/22ms | 45/26/112ms | buckets / errs / p95 CORRECT |
| W5-OLAP-003 high-cardinality GROUP BY | 90ms | 239ms | 12,000 combos CORRECT |
| W5-OLAP-004 multi-table join | 188/315ms | 144/98ms | 3 tiers + 1M device matches CORRECT |

The AIKOQL leg is NOT_MEASURED on all four by design: the substrate has no
columnar scan path, and §7 says delegate to OLAP rather than build one in.
Full numbers + honesty labels: `results/olap-baseline.md`. Env opt-in:
`AIKOQL_TEST_CH_HTTP`, `AIKOQL_TEST_SR_ADDR` (skips honestly when an engine
is unreachable — NOT_MEASURED, never invented).

## What still did NOT run (honest rows)

- §13/§14 federation/pushdown, §9 materialized knowledge, §15 cross-system
  provenance: out of substrate, ordered after this benchmark (plan §23).
- §16/§20 token-cost and crossover rows: the OLAP side is now measured;
  the knowledge side has no data point on OLAP workloads by design, so the
  crossover curve is degenerate at the boundary (losses.md).

## Artifact tree (plan §22) — collapsed where the code IS the spec

Present: `README.md`, `dataset.md`, `schema/schema.sql`,
`results/olap-baseline.md`, `wins.md`, `parity.md`, `losses.md`,
`unknown.md`, `qa-wave5-results.json`, `qa-wave5-report.md` (report +
release-gate + failures in one file).

Not created as prose: `tasks/` and `ground-truth/` live in
`crates/ingestion/tests/wave5_olap.rs` (executable — not duplicated);
per-engine dirs `clickhouse/ starrocks/ aikoql/ federation/` are collapsed
into `results/`: the aikoql/federation legs have no measured rows to hold,
and empty directories would fake evidence.

The gate matrix lives in TESTING-PLAN §12.2–§12.3 (W5-G01..G12 readout).
