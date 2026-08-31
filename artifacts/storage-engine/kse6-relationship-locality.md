# KSE-050..052 — Relationship Locality (MRFC-KSE-001 §12)

Measured 2026-08-31 on X (debug build — indicative, not release numbers).
Dataset per backend: 5 hubs with 1/10/100/1,000/10,000 outbound neighbors (interleaved "links"/"cites"), 11,111 leaf KOs, one database per backend, 10 timed reps per lookup.
Allocations: NOT_MEASURED (no counting-allocator instrumentation wired).

| fan-out | op | redb P50/P95/P99 (µs) | redb engine reqs | RocksDB P50/P95/P99 (µs) | Aikoql P50/P95/P99 (µs) | Aikoql engine reqs |
|---|---|---|---|---|---|---|
| 1 | all | 54 / 136 / 136 | 0 gets + 1 scans (1 pairs, 44 B returned) | 16 / 105 / 105 | 4 / 43 / 43 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | links | 54 / 63 / 63 | 0 gets + 1 scans (1 pairs, 44 B returned) | 15 / 15 / 15 | 4 / 5 / 5 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | cites | 44 / 45 / 45 | 0 gets + 1 scans (0 pairs, 0 B returned) | 12 / 27 / 27 | 3 / 3 / 3 | 0 gets + 1 scans (0 pairs, 0 B returned) |
| 10 | all | 100 / 107 / 107 | 0 gets + 1 scans (10 pairs, 440 B returned) | 33 / 35 / 35 | 11 / 16 / 16 | 0 gets + 1 scans (10 pairs, 440 B returned) |
| 10 | links | 72 / 84 / 84 | 0 gets + 1 scans (5 pairs, 220 B returned) | 23 / 23 / 23 | 6 / 7 / 7 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 10 | cites | 71 / 78 / 78 | 0 gets + 1 scans (5 pairs, 220 B returned) | 23 / 31 / 31 | 7 / 7 / 7 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 100 | all | 580 / 620 / 620 | 0 gets + 1 scans (100 pairs, 4400 B returned) | 189 / 194 / 194 | 67 / 88 / 88 | 0 gets + 1 scans (100 pairs, 4400 B returned) |
| 100 | links | 312 / 881 / 881 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 103 / 114 / 114 | 34 / 35 / 35 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 100 | cites | 308 / 317 / 317 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 103 / 153 / 153 | 34 / 34 / 34 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 1000 | all | 5450 / 6805 / 6805 | 0 gets + 1 scans (1000 pairs, 44000 B returned) | 1907 / 2079 / 2079 | 603 / 1198 / 1198 | 0 gets + 1 scans (1000 pairs, 44000 B returned) |
| 1000 | links | 2791 / 3062 / 3062 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 897 / 1093 / 1093 | 300 / 304 / 304 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 1000 | cites | 2746 / 4312 / 4312 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 917 / 963 / 963 | 297 / 312 / 312 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 10000 | all | 55579 / 65215 / 65215 | 0 gets + 1 scans (10000 pairs, 440000 B returned) | 18840 / 21175 / 21175 | 8048 / 10538 / 10538 | 0 gets + 1 scans (10000 pairs, 440000 B returned) |
| 10000 | links | 30022 / 33572 / 33572 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9273 / 9889 / 9889 | 3848 / 10193 / 10193 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |
| 10000 | cites | 26986 / 29253 / 29253 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9227 / 11448 / 11448 | 3911 / 4579 / 4579 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |

## Consistency (KSE-052)

All 11,111 edges verified bidirectionally on every measured backend: for each outbound (hub -type-> leaf), inbound_edges(leaf, type) contains the hub. Zero divergences — the relo/reli index pair is symmetric over AikoqlStorageEngine exactly as over redb/RocksDB.

## Adjacency-structure verdict

The existing layout IS the knowledge-aware adjacency: every KO's neighbors live in one contiguous key range (relo/<hub>/…), so a lookup is one seek + one linear-in-range scan — see the engine-reqs column (single scan, pairs == fan-out). A custom packed adjacency would save at most the per-row key overhead, and only by moving the write path off the kernel's own index rows. No prototype built; revisit only if a release-build profile shows the per-row key copy dominating.
