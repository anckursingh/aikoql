# Wave 5 QA report — Phase C OLAP baseline (2026-08-30)

Collapsed from the plan's three leaves (report / release-gate / failures):
the gate readout is §1, the measured report is §2 (full table in
`results/olap-baseline.md`), the failures are §3. The full TDD log lives in
TESTING-PLAN §12.4.

## 1. Release gate

| Gate | Verdict |
| --- | --- |
| W5-G01 conventional OLAP baseline reproducible | ✅ MET — `wave5_olap.rs` + compose profile `olap`; deterministic dataset, Rust ground truth; reruns reproduce bit-for-bit |
| W5-G02 ClickHouse/StarRocks configurations documented | ✅ MET — compose services + env (`AIKOQL_TEST_CH_HTTP`, `AIKOQL_TEST_SR_ADDR`, `AIKOQL_TEST_CH_PASSWORD`) in the test header, dataset.md, schema/schema.sql |
| W5-G08 negative OLAP evidence preserved | ✅ MET — AIKOQL rows NOT_MEASURED with structural reason; losses.md carries the mandatory §19 rows |
| W5-G03/G05/G10 regression & no-claim rules | ✅ held — 4/4 new tests green, no "AIKOQL replaces ClickHouse" claim anywhere |

## 2. What was measured

Four P0 OLAP workloads against live ClickHouse 24.8 + StarRocks 3.3, all
CORRECT against Rust ground truth (per-engine ms, min of 3):

| Task | ClickHouse | StarRocks |
| --- | --- | --- |
| W5-OLAP-001 large aggregation | 390ms | 561ms |
| W5-OLAP-002 time-series | 34/24/22ms | 45/26/112ms |
| W5-OLAP-003 high-cardinality GROUP BY | 90ms | 239ms |
| W5-OLAP-004 multi-table join | 188/315ms | 144/98ms |

AIKOQL leg: NOT_MEASURED on all four (no columnar scan path; §7 says
delegate). Honesty labels: debug build, loopback, reproducibility
instrument — not a performance contest; SR p95 approximate by contract;
events/minute measured as events/day (minute not in the schema).

## 3. Failures (all RED, all fixed, none suppressed)

Engine-side:

1. StarRocks connect: the mysql crate probes `@@socket` on loopback
   (default prefer_socket) — StarRocks has no such variable →
   `prefer_socket(false)`.
2. StarRocks INSERT: every derived table needs an alias → `AS g` on the
   cross-join generator.
3. StarRocks: bare `seed` resolved against an unselected session database →
   qualified `aikoql_bench.seed`.
4. ClickHouse: HTTP/1.1 responses are chunked; the raw-socket adapter read
   chunk markers as TSV rows (+513 garbage rows on the 100k-row result) →
   HTTP/1.0 (no chunked encoding, body delimited by close).
5. ClickHouse auth: the image bakes a random default-user password into the
   data volume on first boot → pinned `CLICKHOUSE_PASSWORD` + URL-param
   auth; volume rebuild documented in the compose comment.

Test-side (the engines caught these — the harness was wrong, not them):
6. "service 0 day 0 count = 3" → truth is 4320 (86400/20).
7. Spot combos violated generator coupling (device pins service/region/err:
   e.g. device 0 forces err 1; device 123 forces service 3, not 7).
8. p95 prose "475 for every service" → service+460 (lat ≡ service mod 20).
9. ClickHouse measured NOT_MEASURED (unreachable) while opted in: the probe
   swallowed `http://localhost:8123` (URL scheme) with `.ok()?` → silent
   skip beside a live engine. Fix: strict opt-in — probe accepts the
   `http://` prefix, and env-set-but-unreachable now FAILS the test; skips
   remain only when the env var is genuinely unset (honest row).

## 4. Honest rows (kept)

- AIKOQL NOT_MEASURED on all four OLAP tasks — structural, recorded in
  losses.md, never dressed as a win.
- §13/§14 federation, §9 materialized knowledge, §15 cross-system
  provenance remain NOT_IMPLEMENTED — the harness was the gate for the
  OLAP rows, not for those.
