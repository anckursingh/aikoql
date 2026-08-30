# Parity (measured)

Workloads where the substrate matched the baseline — neither side wins.

| Row | Note |
| --- | --- |
| W5-KA-002 temporal knowledge | Closed in Wave 3.1 (W31-TEMP-001) — pointer row, not re-run (plan §3) |
| W5-KA-005 unknown handling | Closed in Wave 3.1 (W31-UNK-001) — pointer row, not re-run (plan §3) |
| W5-KA-007 change impact | Closed in Wave 3.1 (W31-IMPACT-001) — pointer row, not re-run (plan §3) |
| W5-KA-006 aggregation correctness | The COUNT itself is app-side (13 LOC) — by design, the plan's build-vs-buy rule forbids an OLAP COUNT in the substrate until measured need; the app loop reads knowledge state that OLAP SQL cannot see (evidence independence, supersession) |
