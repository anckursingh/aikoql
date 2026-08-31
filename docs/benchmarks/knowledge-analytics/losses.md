# Losses (honest)

| Row | Loss | Why it stays in the record |
| --- | --- | --- |
| W5-OLAP-001..004 (large aggregation, time-series, high-cardinality GROUP BY, multi-table join) | AIKOQL NOT_MEASURED on all four — no columnar scan path (redb, row-at-a-time) | The plan's §19 *mandatory OLAP losses*. Knowledge-native execution is not a column store and makes no claim there. The baselines now exist (ClickHouse 10–453ms, StarRocks 18–562ms, `results/olap-baseline.md`); AIKOQL has no data point because §7 says delegate, not re-implement |
| §20 knowledge crossover curve | Degenerate on OLAP workloads | AIKOQL's side of the curve has no OLAP data point by design — the curve IS the boundary: knowledge workloads win (wins.md), OLAP workloads are delegated losses (this row) |
| §13/§14 federation/pushdown, §9 materialized knowledge, §15 cross-system provenance | NOT_IMPLEMENTED | Deliberately ordered after the Phase C benchmark (plan §23) — recorded, not forgotten |

No negative results were suppressed: all ten Phase C REDs (five
engine-side — StarRocks `@@socket` probe, derived-table alias, unqualified
`seed`, ClickHouse chunked HTTP, volume-baked auth password — plus three
test-side ground-truth bugs the engines caught, the compose/env
gotchas, and the silent skip while opted in) are in
`qa-wave5-report.md` §3 and TESTING-PLAN §12.4.
