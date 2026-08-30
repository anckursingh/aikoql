# Unknown (honest)

| Row | Unknown | How it becomes known |
| --- | --- | --- |
| Where the knowledge/OLAP crossover sits for *mixed* workloads (plan §20) | Pure-OLAP workloads are now measured and are delegated losses (losses.md); the crossover for hybrid knowledge+aggregation tasks is unmeasured | A Phase D hybrid task set (knowledge traversal feeding an aggregation) — not scheduled |
| Cost per successful investigation (plan §16, gate W5-G07) | Knowledge tasks: µs measured (296–391µs). OLAP tasks: ms measured (`results/olap-baseline.md`). The token/cost columns remain pending | The G12 cost instrument extended with the OLAP leg |
| ~~Whether knowledge-native execution ever *requires* an embedded OLAP engine~~ | **Answered (2026-08-30): no.** The Phase C harness needed only external engines over HTTP/MySQL — the build-vs-buy rule (§7) held; no ClickHouse inside AIKOQL | — |
| Snapshot (time-travel) inbound traversal completeness | The KO-fallback traverse path resolves edges from KO versions; relate()-created edges are stored Outbound on the from-KO only, so an inbound snapshot traversal across them is untested | A targeted W5 test with `ctx.snapshot` — noted here, not asserted |
