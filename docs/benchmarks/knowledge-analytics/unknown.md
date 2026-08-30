# Unknown (honest)

| Row | Unknown | How it becomes known |
| --- | --- | --- |
| Where the knowledge/OLAP crossover point sits (plan §20) | Unmeasured — needs both legs on one dataset | Phase C harness (docker-compose, plan §4) |
| Cost per successful investigation (plan §16, gate W5-G07) | µs per task measured (296–391µs); token/cost columns pending the OLAP leg | Phase C + the G12 cost instrument |
| Whether knowledge-native execution ever *requires* an embedded OLAP engine | The plan's build-vs-buy question itself | The Phase A → C benchmark; until then the rule (§7) stands: do not build ClickHouse inside AIKOQL |
| Snapshot (time-travel) inbound traversal completeness | The KO-fallback traverse path resolves edges from KO versions; relate()-created edges are stored Outbound on the from-KO only, so an inbound snapshot traversal across them is untested | A Phase C or targeted W5 test with `ctx.snapshot` — noted here, not asserted |
