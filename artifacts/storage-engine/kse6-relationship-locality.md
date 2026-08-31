# KSE-050..052 — Relationship Locality (MRFC-KSE-001 §12)

Measured 2026-08-31 on X (debug build — indicative, not release numbers).
Dataset per backend: 5 hubs with 1/10/100/1,000/10,000 outbound neighbors (interleaved "links"/"cites"), 11,111 leaf KOs, one database per backend, 10 timed reps per lookup.
Allocations: NOT_MEASURED (no counting-allocator instrumentation wired).

| fan-out | op | redb P50/P95/P99 (µs) | redb engine reqs | RocksDB P50/P95/P99 (µs) | Aikoql P50/P95/P99 (µs) | Aikoql engine reqs |
|---|---|---|---|---|---|---|
| 1 | all | 52 / 133 / 133 | 0 gets + 1 scans (1 pairs, 44 B returned) | 17 / 127 / 127 | 4 / 38 / 38 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | links | 52 / 60 / 60 | 0 gets + 1 scans (1 pairs, 44 B returned) | 17 / 23 / 23 | 4 / 5 / 5 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | cites | 42 / 43 / 43 | 0 gets + 1 scans (0 pairs, 0 B returned) | 13 / 14 / 14 | 3 / 3 / 3 | 0 gets + 1 scans (0 pairs, 0 B returned) |
| 10 | all | 94 / 99 / 99 | 0 gets + 1 scans (10 pairs, 440 B returned) | 34 / 36 / 36 | 10 / 16 / 16 | 0 gets + 1 scans (10 pairs, 440 B returned) |
| 10 | links | 69 / 70 / 70 | 0 gets + 1 scans (5 pairs, 220 B returned) | 25 / 25 / 25 | 5 / 6 / 6 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 10 | cites | 69 / 75 / 75 | 0 gets + 1 scans (5 pairs, 220 B returned) | 25 / 29 / 29 | 7 / 7 / 7 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 100 | all | 558 / 592 / 592 | 0 gets + 1 scans (100 pairs, 4400 B returned) | 192 / 201 / 201 | 65 / 84 / 84 | 0 gets + 1 scans (100 pairs, 4400 B returned) |
| 100 | links | 300 / 344 / 344 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 103 / 112 / 112 | 33 / 41 / 41 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 100 | cites | 301 / 357 / 357 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 103 / 110 / 110 | 33 / 42 / 42 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 1000 | all | 5209 / 6828 / 6828 | 0 gets + 1 scans (1000 pairs, 44000 B returned) | 1838 / 2721 / 2721 | 586 / 764 / 764 | 0 gets + 1 scans (1000 pairs, 44000 B returned) |
| 1000 | links | 3406 / 3589 / 3589 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 903 / 1144 / 1144 | 296 / 366 / 366 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 1000 | cites | 3338 / 3394 / 3394 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 867 / 925 / 925 | 293 / 309 / 309 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 10000 | all | 53045 / 67926 / 67926 | 0 gets + 1 scans (10000 pairs, 440000 B returned) | 18762 / 20232 / 20232 | 9019 / 9362 / 9362 | 0 gets + 1 scans (10000 pairs, 440000 B returned) |
| 10000 | links | 27811 / 31478 / 31478 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9209 / 11222 / 11222 | 3674 / 4524 / 4524 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |
| 10000 | cites | 27465 / 50537 / 50537 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9409 / 12574 / 12574 | 3361 / 3729 / 3729 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |

## Consistency (KSE-052)

All 11,111 edges verified bidirectionally on every measured backend: for each outbound (hub -type-> leaf), inbound_edges(leaf, type) contains the hub. Zero divergences — the relo/reli index pair is symmetric over AikoqlStorageEngine exactly as over redb/RocksDB.

## Adjacency-structure verdict

The existing layout IS the knowledge-aware adjacency: every KO's neighbors live in one contiguous key range (relo/<hub>/…), so a lookup is one seek + one linear-in-range scan — see the engine-reqs column (single scan, pairs == fan-out). A custom packed adjacency would save at most the per-row key overhead, and only by moving the write path off the kernel's own index rows. No prototype built; revisit only if a release-build profile shows the per-row key copy dominating.
