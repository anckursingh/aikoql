# KSE-050..052 — Relationship Locality (MRFC-KSE-001 §12)

Measured 2026-08-31 on X (debug build — indicative, not release numbers).
Dataset per backend: 5 hubs with 1/10/100/1,000/10,000 outbound neighbors (interleaved "links"/"cites"), 11,111 leaf KOs, one database per backend, 10 timed reps per lookup.
Allocations: NOT_MEASURED (no counting-allocator instrumentation wired).

| fan-out | op | redb P50/P95/P99 (µs) | redb engine reqs | RocksDB P50/P95/P99 (µs) | Aikoql P50/P95/P99 (µs) | Aikoql engine reqs |
|---|---|---|---|---|---|---|
| 1 | all | 64 / 184 / 184 | 0 gets + 1 scans (1 pairs, 44 B returned) | 18 / 116 / 116 | 4 / 41 / 41 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | links | 64 / 92 / 92 | 0 gets + 1 scans (1 pairs, 44 B returned) | 17 / 18 / 18 | 4 / 5 / 5 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | cites | 90 / 108 / 108 | 0 gets + 1 scans (0 pairs, 0 B returned) | 13 / 14 / 14 | 3 / 3 / 3 | 0 gets + 1 scans (0 pairs, 0 B returned) |
| 10 | all | 166 / 199 / 199 | 0 gets + 1 scans (10 pairs, 440 B returned) | 37 / 39 / 39 | 11 / 17 / 17 | 0 gets + 1 scans (10 pairs, 440 B returned) |
| 10 | links | 145 / 160 / 160 | 0 gets + 1 scans (5 pairs, 220 B returned) | 26 / 27 / 27 | 6 / 7 / 7 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 10 | cites | 114 / 117 / 117 | 0 gets + 1 scans (5 pairs, 220 B returned) | 26 / 34 / 34 | 7 / 7 / 7 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 100 | all | 758 / 951 / 951 | 0 gets + 1 scans (100 pairs, 4400 B returned) | 210 / 251 / 251 | 65 / 93 / 93 | 0 gets + 1 scans (100 pairs, 4400 B returned) |
| 100 | links | 561 / 818 / 818 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 113 / 149 / 149 | 34 / 61 / 61 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 100 | cites | 385 / 579 / 579 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 114 / 134 / 134 | 34 / 51 / 51 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 1000 | all | 8043 / 9680 / 9680 | 0 gets + 1 scans (1000 pairs, 44000 B returned) | 1957 / 2105 / 2105 | 604 / 869 / 869 | 0 gets + 1 scans (1000 pairs, 44000 B returned) |
| 1000 | links | 3679 / 4586 / 4586 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 864 / 909 / 909 | 300 / 330 / 330 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 1000 | cites | 3667 / 4238 / 4238 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 867 / 908 / 908 | 305 / 361 / 361 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 10000 | all | 99813 / 193973 / 193973 | 0 gets + 1 scans (10000 pairs, 440000 B returned) | 19251 / 21753 / 21753 | 9843 / 12043 / 12043 | 0 gets + 1 scans (10000 pairs, 440000 B returned) |
| 10000 | links | 40595 / 52318 / 52318 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9370 / 13875 / 13875 | 5111 / 6087 / 6087 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |
| 10000 | cites | 39377 / 45234 / 45234 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9252 / 13782 / 13782 | 3995 / 5951 / 5951 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |

## Consistency (KSE-052)

All 11,111 edges verified bidirectionally on every measured backend: for each outbound (hub -type-> leaf), inbound_edges(leaf, type) contains the hub. Zero divergences — the relo/reli index pair is symmetric over AikoqlStorageEngine exactly as over redb/RocksDB.

## Adjacency-structure verdict

The existing layout IS the knowledge-aware adjacency: every KO's neighbors live in one contiguous key range (relo/<hub>/…), so a lookup is one seek + one linear-in-range scan — see the engine-reqs column (single scan, pairs == fan-out). A custom packed adjacency would save at most the per-row key overhead, and only by moving the write path off the kernel's own index rows. No prototype built; revisit only if a release-build profile shows the per-row key copy dominating.
