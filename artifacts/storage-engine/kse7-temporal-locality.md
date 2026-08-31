# KSE-060..063 — Temporal Locality (MRFC-KSE-001 §13)

Measured 2026-08-31 on X (debug build — indicative, not release numbers).
Dataset per backend: 50 KOs × (50 updates + create), manual clock +10,000 ms per commit — every version a distinct commit ts; 20 timed reps × 50 KOs per op.

| op | redb P50/P95/P99 (µs) | redb engine reqs | RocksDB P50/P95/P99 (µs) | Aikoql P50/P95/P99 (µs) | Aikoql engine reqs |
|---|---|---|---|---|---|
| current | 70 / 89 / 115 | 2 gets + 0 scans (0 pairs, 1113 B returned) | 31 / 35 / 44 | 22 / 25 / 30 | 2 gets + 0 scans (0 pairs, 1113 B returned) |
| historical | 365 / 549 / 659 | 0 gets + 1 scans (51 pairs, 35598 B returned) | 125 / 191 / 238 | 46 / 70 / 92 | 0 gets + 1 scans (51 pairs, 35598 B returned) |
| history | 1062 / 2318 / 5849 | 0 gets + 1 scans (51 pairs, 35598 B returned) | 747 / 1025 / 1245 | 660 / 862 / 1139 | 0 gets + 1 scans (51 pairs, 35598 B returned) |
| range | 987 / 1406 / 1635 | 0 gets + 1 scans (51 pairs, 35598 B returned) | 744 / 1219 / 1536 | 662 / 780 / 995 | 0 gets + 1 scans (51 pairs, 35598 B returned) |

## Pins (KSE-060..063)

- KSE-060: current-version read issues 0 scans on every backend at every version depth — head-pointer get, no history walk.
- KSE-061: get_as_of at a mid-history wall instant returns exactly the newest-committed version (commit ts == snap match) — pinned on all 50 KOs per backend. But the request shape is a full version-prefix scan (51 pairs, 35.6 KB — identical to history), not a seek: object_at walks the ko/ prefix from the start. The lever is a seek-to-snap (engine lands at the newest ts <= snap, kernel reads one row) — same class as the range pushdown below.
- KSE-062: history returns all 51 versions (create + 50 updates), strictly ascending commit ts.
- KSE-063: [t1, t2) with t1 = version 10, t2 = version 30 returns exactly versions 10..29 — the kernel has no range API, so the filter runs above the engine over the full history scan (see the range row: it costs a history read + client-side filter). A range pushdown into the ko/ prefix scan is the only lever if range queries ever need to beat full-history cost.
- Backend parity: identical version-ts sequences over every backend (same seed, same clock) — the MVCC shape is engine-neutral.
