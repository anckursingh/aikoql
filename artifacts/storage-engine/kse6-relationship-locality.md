# KSE-050..052 — Relationship Locality (MRFC-KSE-001 §12)

Measured 2026-08-31 on X (debug build — indicative, not release numbers).
Dataset per backend: 5 hubs with 1/10/100/1,000/10,000 outbound neighbors (interleaved "links"/"cites"), 11,111 leaf KOs, one database per backend, 10 timed reps per lookup.
Allocations: NOT_MEASURED (no counting-allocator instrumentation wired).

| fan-out | op | redb P50/P95/P99 (µs) | redb engine reqs | RocksDB P50/P95/P99 (µs) | Aikoql P50/P95/P99 (µs) | Aikoql engine reqs |
|---|---|---|---|---|---|---|
| 1 | all | 54 / 139 / 139 | 0 gets + 1 scans (1 pairs, 44 B returned) | 34 / 126 / 126 | 7 / 56 / 56 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | links | 54 / 69 / 69 | 0 gets + 1 scans (1 pairs, 44 B returned) | 34 / 35 / 35 | 7 / 145 / 145 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | cites | 44 / 44 / 44 | 0 gets + 1 scans (0 pairs, 0 B returned) | 27 / 45 / 45 | 7 / 9 / 9 | 0 gets + 1 scans (0 pairs, 0 B returned) |
| 10 | all | 99 / 111 / 111 | 0 gets + 1 scans (10 pairs, 440 B returned) | 68 / 69 / 69 | 22 / 43 / 43 | 0 gets + 1 scans (10 pairs, 440 B returned) |
| 10 | links | 72 / 72 / 72 | 0 gets + 1 scans (5 pairs, 220 B returned) | 49 / 53 / 53 | 12 / 23 / 23 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 10 | cites | 72 / 73 / 73 | 0 gets + 1 scans (5 pairs, 220 B returned) | 50 / 69 / 69 | 12 / 20 / 20 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 100 | all | 585 / 628 / 628 | 0 gets + 1 scans (100 pairs, 4400 B returned) | 369 / 487 / 487 | 149 / 268 / 268 | 0 gets + 1 scans (100 pairs, 4400 B returned) |
| 100 | links | 373 / 519 / 519 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 197 / 212 / 212 | 95 / 231 / 231 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 100 | cites | 452 / 508 / 508 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 184 / 471 / 471 | 119 / 406 / 406 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 1000 | all | 6687 / 7172 / 7172 | 0 gets + 1 scans (1000 pairs, 44000 B returned) | 3220 / 3373 / 3373 | 1223 / 2074 / 2074 | 0 gets + 1 scans (1000 pairs, 44000 B returned) |
| 1000 | links | 3387 / 3739 / 3739 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 1660 / 1763 / 1763 | 776 / 2786 / 2786 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 1000 | cites | 2738 / 2993 / 2993 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 1262 / 1660 / 1660 | 667 / 1311 / 1311 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 10000 | all | 55077 / 61367 / 61367 | 0 gets + 1 scans (10000 pairs, 440000 B returned) | 31701 / 33145 / 33145 | 14360 / 16456 / 16456 | 0 gets + 1 scans (10000 pairs, 440000 B returned) |
| 10000 | links | 27418 / 35362 / 35362 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 14374 / 18606 / 18606 | 6432 / 7167 / 7167 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |
| 10000 | cites | 28130 / 30017 / 30017 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 15463 / 19306 / 19306 | 6985 / 13621 / 13621 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |

## Consistency (KSE-052)

All 11,111 edges verified bidirectionally on every measured backend: for each outbound (hub -type-> leaf), inbound_edges(leaf, type) contains the hub. Zero divergences — the relo/reli index pair is symmetric over AikoqlStorageEngine exactly as over redb/RocksDB.

## Adjacency-structure verdict

The existing layout IS the knowledge-aware adjacency: every KO's neighbors live in one contiguous key range (relo/<hub>/…), so a lookup is one seek + one linear-in-range scan — see the engine-reqs column (single scan, pairs == fan-out). A custom packed adjacency would save at most the per-row key overhead, and only by moving the write path off the kernel's own index rows. No prototype built; revisit only if a release-build profile shows the per-row key copy dominating.
