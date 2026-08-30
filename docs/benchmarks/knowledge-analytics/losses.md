# Losses (honest)

| Row | Loss | Why it stays in the record |
| --- | --- | --- |
| W5-OLAP-001..004 (large aggregation, time-series, high-cardinality GROUP BY, multi-table join) | NOT_IMPLEMENTED — no ClickHouse/StarRocks leg | The plan's §19 names these as *mandatory OLAP losses*: knowledge-native execution is not a column store and makes no claim there. Recorded as NOT_IMPLEMENTED, never as a win (plan §23 honest-failure rule) |
| W5-G01/G02/G08/G09 (§19/§20) | Gates blocked on the Phase C harness | No OLAP baseline → no comparative numbers → the gates stay ⛔ rather than being redefined to pass |
| §13/§14 federation/pushdown, §9 materialized knowledge, §15 cross-system provenance | NOT_IMPLEMENTED | Deliberately ordered after the Phase A benchmark (plan §23) — recorded, not forgotten |

No negative *knowledge-side* results were suppressed: the two REDs from the
TDD cycle (traverse direction bug, currency predicate) are recorded in
TESTING-PLAN §12.4 with their fixes.
