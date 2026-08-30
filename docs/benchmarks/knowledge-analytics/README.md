# Knowledge Analytics Benchmark — Wave 5

Boundary-discovery benchmark (plan `../AIKOQL_Wave5_Knowledge_Analytics_vs_OLAP_TDD_Test_Plan.md`).
Status: **knowledge side GREEN, OLAP side NOT_IMPLEMENTED (honest).**

## What ran (2026-08-30)

Five P0 knowledge-analytics tests, deterministic and LLM-free, in
`crates/ingestion/tests/wave5_ka.rs`:

| Test | Question | Result |
| --- | --- | --- |
| W5-KA-001 | Which customers are affected by the LedgerService outage, and why? | 6/6 hops, 0 false, 0 missed, ~391µs, 12 app LOC; mechanical RAG pack: 2/5 chain chunks + 1 false chunk |
| W5-KA-003 | Why is Customer X high risk? | Derivation record complete (how/by-whom/when/from-what/why); premise superseded → classification swept; lineage intact, ~375µs |
| W5-KA-004 | CRM ACTIVE vs Fraud BLOCKED vs Policy | Effective state BLOCKED, conflict disclosed, authority selection correct, loser traceable |
| W5-KA-006 | How many high-risk customers have ≥2 independent sources? | 5 customers → answer 1 (source-independence + temporal validity enforced), ~296µs, 13 app LOC |
| W5-KA-008 | What did we know about Customer X on date T? | t=500 ACTIVE(crm), t=1500 SUSPENDED(billing), t=2500 ACTIVE(audit); in-place versions separated by get_as_of, ~330µs |

Three further Phase A rows (KA-002 temporal, KA-005 unknown, KA-007 change impact)
are pointer rows to Wave 3.1 closures — plan §3 forbids duplication.

## What did NOT run (honest rows)

- W5-OLAP-001..004 (large aggregation, time-series, high-cardinality GROUP BY, multi-table join):
  no ClickHouse/StarRocks adapter exists, and the plan's build-vs-buy rule (§7) forbids
  building one into AIKOQL until a measured benchmark proves knowledge-native execution
  requires it. No stub tests were written.
- §13/§14 federation/pushdown, §9 materialized knowledge, §15 cross-system provenance,
  §19/§20 loss + crossover rows: blocked on the same harness.

Next step (plan §23 Phase C): the docker-compose OLAP harness (plan §4) with adapters
external to the substrate, all three legs consuming the same dataset, tasks, ground truth.

## Planned artifact tree (plan §22) — what exists vs pending

Present: `README.md`, `wins.md`, `parity.md`, `losses.md`, `unknown.md`.
Pending (created with the Phase C harness): `dataset.md`, `schema/`, `tasks/`,
`ground-truth/`, `clickhouse/`, `starrocks/`, `aikoql/`, `federation/`, `results/`,
`qa-wave5-results.json`, `qa-wave5-report.md`, `qa-wave5-release-gate.md`,
`qa-wave5-failures.md`.

The gate matrix lives in TESTING-PLAN §12.2–§12.3 (W5-G01..G12 readout).
