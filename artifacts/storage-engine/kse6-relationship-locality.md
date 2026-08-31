# KSE-050..052 — Relationship Locality (MRFC-KSE-001 §12)

Measured 2026-08-31 on X (debug build — indicative, not release numbers).
Dataset per backend: 5 hubs with 1/10/100/1,000/10,000 outbound neighbors (interleaved "links"/"cites"), 11,111 leaf KOs, one database per backend, 10 timed reps per lookup.
Allocations: NOT_MEASURED (no counting-allocator instrumentation wired).

| fan-out | op | redb P50/P95/P99 (µs) | redb engine reqs | RocksDB P50/P95/P99 (µs) | Aikoql P50/P95/P99 (µs) | Aikoql engine reqs |
|---|---|---|---|---|---|---|
| 1 | all | 53 / 137 / 137 | 0 gets + 1 scans (1 pairs, 44 B returned) | 18 / 106 / 106 | 6 / 50 / 50 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | links | 53 / 60 / 60 | 0 gets + 1 scans (1 pairs, 44 B returned) | 16 / 17 / 17 | 6 / 7 / 7 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | cites | 43 / 44 / 44 | 0 gets + 1 scans (0 pairs, 0 B returned) | 12 / 14 / 14 | 4 / 4 / 4 | 0 gets + 1 scans (0 pairs, 0 B returned) |
| 10 | all | 98 / 108 / 108 | 0 gets + 1 scans (10 pairs, 440 B returned) | 33 / 35 / 35 | 16 / 26 / 26 | 0 gets + 1 scans (10 pairs, 440 B returned) |
| 10 | links | 71 / 72 / 72 | 0 gets + 1 scans (5 pairs, 220 B returned) | 25 / 30 / 30 | 9 / 10 / 10 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 10 | cites | 71 / 73 / 73 | 0 gets + 1 scans (5 pairs, 220 B returned) | 24 / 25 / 25 | 10 / 11 / 11 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 100 | all | 584 / 670 / 670 | 0 gets + 1 scans (100 pairs, 4400 B returned) | 194 / 242 / 242 | 98 / 125 / 125 | 0 gets + 1 scans (100 pairs, 4400 B returned) |
| 100 | links | 307 / 343 / 343 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 102 / 108 / 108 | 52 / 97 / 97 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 100 | cites | 306 / 313 / 313 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 103 / 155 / 155 | 51 / 63 / 63 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 1000 | all | 6002 / 7887 / 7887 | 0 gets + 1 scans (1000 pairs, 44000 B returned) | 1827 / 2735 / 2735 | 616 / 1063 / 1063 | 0 gets + 1 scans (1000 pairs, 44000 B returned) |
| 1000 | links | 2665 / 3564 / 3564 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 945 / 972 / 972 | 301 / 406 / 406 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 1000 | cites | 2696 / 5374 / 5374 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 903 / 920 / 920 | 307 / 352 / 352 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 10000 | all | 56022 / 68596 / 68596 | 0 gets + 1 scans (10000 pairs, 440000 B returned) | 18665 / 22238 / 22238 | 8187 / 10072 / 10072 | 0 gets + 1 scans (10000 pairs, 440000 B returned) |
| 10000 | links | 26955 / 27440 / 27440 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9370 / 11958 / 11958 | 3853 / 6057 / 6057 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |
| 10000 | cites | 27008 / 29530 / 29530 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9988 / 12782 / 12782 | 3806 / 4907 / 4907 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |

## Consistency (KSE-052)

All 11,111 edges verified bidirectionally on every measured backend: for each outbound (hub -type-> leaf), inbound_edges(leaf, type) contains the hub. Zero divergences — the relo/reli index pair is symmetric over AikoqlStorageEngine exactly as over redb/RocksDB.

## Adjacency-structure verdict

The existing layout IS the knowledge-aware adjacency: every KO's neighbors live in one contiguous key range (relo/<hub>/…), so a lookup is one seek + one linear-in-range scan — see the engine-reqs column (single scan, pairs == fan-out). A custom packed adjacency would save at most the per-row key overhead, and only by moving the write path off the kernel's own index rows. No prototype built; revisit only if a release-build profile shows the per-row key copy dominating.
