# KSE-050..052 — Relationship Locality (MRFC-KSE-001 §12)

Measured 2026-08-31 on X (debug build — indicative, not release numbers).
Dataset per backend: 5 hubs with 1/10/100/1,000/10,000 outbound neighbors (interleaved "links"/"cites"), 11,111 leaf KOs, one database per backend, 10 timed reps per lookup.
Allocations: NOT_MEASURED (no counting-allocator instrumentation wired).

| fan-out | op | redb P50/P95/P99 (µs) | redb engine reqs | RocksDB P50/P95/P99 (µs) | Aikoql P50/P95/P99 (µs) | Aikoql engine reqs |
|---|---|---|---|---|---|---|
| 1 | all | 52 / 126 / 126 | 0 gets + 1 scans (1 pairs, 44 B returned) | 29 / 172 / 172 | 6 / 46 / 46 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | links | 52 / 60 / 60 | 0 gets + 1 scans (1 pairs, 44 B returned) | 16 / 33 / 33 | 6 / 24 / 24 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | cites | 42 / 43 / 43 | 0 gets + 1 scans (0 pairs, 0 B returned) | 12 / 13 / 13 | 4 / 4 / 4 | 0 gets + 1 scans (0 pairs, 0 B returned) |
| 10 | all | 97 / 105 / 105 | 0 gets + 1 scans (10 pairs, 440 B returned) | 33 / 35 / 35 | 16 / 23 / 23 | 0 gets + 1 scans (10 pairs, 440 B returned) |
| 10 | links | 70 / 77 / 77 | 0 gets + 1 scans (5 pairs, 220 B returned) | 24 / 24 / 24 | 8 / 10 / 10 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 10 | cites | 69 / 76 / 76 | 0 gets + 1 scans (5 pairs, 220 B returned) | 24 / 24 / 24 | 10 / 10 / 10 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 100 | all | 649 / 975 / 975 | 0 gets + 1 scans (100 pairs, 4400 B returned) | 190 / 202 / 202 | 97 / 123 / 123 | 0 gets + 1 scans (100 pairs, 4400 B returned) |
| 100 | links | 308 / 382 / 382 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 104 / 138 / 138 | 51 / 60 / 60 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 100 | cites | 300 / 309 / 309 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 102 / 102 / 102 | 50 / 50 / 50 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 1000 | all | 5233 / 7296 / 7296 | 0 gets + 1 scans (1000 pairs, 44000 B returned) | 1735 / 1892 / 1892 | 587 / 1050 / 1050 | 0 gets + 1 scans (1000 pairs, 44000 B returned) |
| 1000 | links | 2640 / 2811 / 2811 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 868 / 930 / 930 | 295 / 356 / 356 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 1000 | cites | 2604 / 2771 / 2771 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 871 / 877 / 877 | 293 / 313 / 313 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 10000 | all | 58957 / 62877 / 62877 | 0 gets + 1 scans (10000 pairs, 440000 B returned) | 18594 / 23077 / 23077 | 7880 / 10800 / 10800 | 0 gets + 1 scans (10000 pairs, 440000 B returned) |
| 10000 | links | 29103 / 35513 / 35513 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 10175 / 12350 / 12350 | 4494 / 4955 / 4955 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |
| 10000 | cites | 29027 / 32676 / 32676 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9283 / 11456 / 11456 | 3487 / 5073 / 5073 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |

## Consistency (KSE-052)

All 11,111 edges verified bidirectionally on every measured backend: for each outbound (hub -type-> leaf), inbound_edges(leaf, type) contains the hub. Zero divergences — the relo/reli index pair is symmetric over AikoqlStorageEngine exactly as over redb/RocksDB.

## Adjacency-structure verdict

The existing layout IS the knowledge-aware adjacency: every KO's neighbors live in one contiguous key range (relo/<hub>/…), so a lookup is one seek + one linear-in-range scan — see the engine-reqs column (single scan, pairs == fan-out). A custom packed adjacency would save at most the per-row key overhead, and only by moving the write path off the kernel's own index rows. No prototype built; revisit only if a release-build profile shows the per-row key copy dominating.
