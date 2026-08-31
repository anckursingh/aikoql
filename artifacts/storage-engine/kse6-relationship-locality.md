# KSE-050..052 — Relationship Locality (MRFC-KSE-001 §12)

Measured 2026-08-31 on X (debug build — indicative, not release numbers).
Dataset per backend: 5 hubs with 1/10/100/1,000/10,000 outbound neighbors (interleaved "links"/"cites"), 11,111 leaf KOs, one database per backend, 10 timed reps per lookup.
Allocations: NOT_MEASURED (no counting-allocator instrumentation wired).

| fan-out | op | redb P50/P95/P99 (µs) | redb engine reqs | RocksDB P50/P95/P99 (µs) | Aikoql P50/P95/P99 (µs) | Aikoql engine reqs |
|---|---|---|---|---|---|---|
| 1 | all | 52 / 143 / 143 | 0 gets + 1 scans (1 pairs, 44 B returned) | 16 / 99 / 99 | 8 / 49 / 49 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | links | 52 / 59 / 59 | 0 gets + 1 scans (1 pairs, 44 B returned) | 15 / 16 / 16 | 8 / 8 / 8 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | cites | 42 / 43 / 43 | 0 gets + 1 scans (0 pairs, 0 B returned) | 12 / 13 / 13 | 5 / 6 / 6 | 0 gets + 1 scans (0 pairs, 0 B returned) |
| 10 | all | 95 / 103 / 103 | 0 gets + 1 scans (10 pairs, 440 B returned) | 34 / 71 / 71 | 20 / 26 / 26 | 0 gets + 1 scans (10 pairs, 440 B returned) |
| 10 | links | 69 / 76 / 76 | 0 gets + 1 scans (5 pairs, 220 B returned) | 24 / 24 / 24 | 11 / 12 / 12 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 10 | cites | 70 / 76 / 76 | 0 gets + 1 scans (5 pairs, 220 B returned) | 23 / 24 / 24 | 13 / 15 / 15 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 100 | all | 994 / 1395 / 1395 | 0 gets + 1 scans (100 pairs, 4400 B returned) | 197 / 229 / 229 | 112 / 168 / 168 | 0 gets + 1 scans (100 pairs, 4400 B returned) |
| 100 | links | 302 / 388 / 388 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 153 / 235 / 235 | 54 / 57 / 57 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 100 | cites | 300 / 306 / 306 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 162 / 174 / 174 | 42 / 42 / 42 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 1000 | all | 5310 / 6719 / 6719 | 0 gets + 1 scans (1000 pairs, 44000 B returned) | 1863 / 2974 / 2974 | 597 / 948 / 948 | 0 gets + 1 scans (1000 pairs, 44000 B returned) |
| 1000 | links | 2671 / 2846 / 2846 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 875 / 985 / 985 | 307 / 378 / 378 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 1000 | cites | 2666 / 3957 / 3957 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 894 / 912 / 912 | 298 / 326 / 326 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 10000 | all | 58791 / 61606 / 61606 | 0 gets + 1 scans (10000 pairs, 440000 B returned) | 20170 / 24309 / 24309 | 7628 / 9140 / 9140 | 0 gets + 1 scans (10000 pairs, 440000 B returned) |
| 10000 | links | 29111 / 31489 / 31489 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9255 / 12262 / 12262 | 3576 / 3956 / 3956 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |
| 10000 | cites | 29454 / 34430 / 34430 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 10160 / 61595 / 61595 | 3629 / 3937 / 3937 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |

## Consistency (KSE-052)

All 11,111 edges verified bidirectionally on every measured backend: for each outbound (hub -type-> leaf), inbound_edges(leaf, type) contains the hub. Zero divergences — the relo/reli index pair is symmetric over AikoqlStorageEngine exactly as over redb/RocksDB.

## Adjacency-structure verdict

The existing layout IS the knowledge-aware adjacency: every KO's neighbors live in one contiguous key range (relo/<hub>/…), so a lookup is one seek + one linear-in-range scan — see the engine-reqs column (single scan, pairs == fan-out). A custom packed adjacency would save at most the per-row key overhead, and only by moving the write path off the kernel's own index rows. No prototype built; revisit only if a release-build profile shows the per-row key copy dominating.
