# KSE-050..052 — Relationship Locality (MRFC-KSE-001 §12)

Measured 2026-08-31 on X (debug build — indicative, not release numbers).
Dataset per backend: 5 hubs with 1/10/100/1,000/10,000 outbound neighbors (interleaved "links"/"cites"), 11,111 leaf KOs, one database per backend, 10 timed reps per lookup.
Allocations: NOT_MEASURED (no counting-allocator instrumentation wired).

| fan-out | op | redb P50/P95/P99 (µs) | redb engine reqs | RocksDB P50/P95/P99 (µs) | Aikoql P50/P95/P99 (µs) | Aikoql engine reqs |
|---|---|---|---|---|---|---|
| 1 | all | 53 / 139 / 139 | 0 gets + 1 scans (1 pairs, 44 B returned) | 16 / 100 / 100 | 4 / 39 / 39 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | links | 53 / 61 / 61 | 0 gets + 1 scans (1 pairs, 44 B returned) | 15 / 16 / 16 | 4 / 5 / 5 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | cites | 43 / 49 / 49 | 0 gets + 1 scans (0 pairs, 0 B returned) | 12 / 28 / 28 | 3 / 3 / 3 | 0 gets + 1 scans (0 pairs, 0 B returned) |
| 10 | all | 97 / 102 / 102 | 0 gets + 1 scans (10 pairs, 440 B returned) | 33 / 35 / 35 | 10 / 17 / 17 | 0 gets + 1 scans (10 pairs, 440 B returned) |
| 10 | links | 70 / 71 / 71 | 0 gets + 1 scans (5 pairs, 220 B returned) | 24 / 24 / 24 | 5 / 6 / 6 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 10 | cites | 70 / 77 / 77 | 0 gets + 1 scans (5 pairs, 220 B returned) | 24 / 24 / 24 | 7 / 7 / 7 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 100 | all | 589 / 751 / 751 | 0 gets + 1 scans (100 pairs, 4400 B returned) | 186 / 223 / 223 | 65 / 166 / 166 | 0 gets + 1 scans (100 pairs, 4400 B returned) |
| 100 | links | 305 / 335 / 335 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 102 / 108 / 108 | 34 / 34 / 34 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 100 | cites | 378 / 415 / 415 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 101 / 102 / 102 | 33 / 33 / 33 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 1000 | all | 5285 / 5488 / 5488 | 0 gets + 1 scans (1000 pairs, 44000 B returned) | 1709 / 1770 / 1770 | 589 / 835 / 835 | 0 gets + 1 scans (1000 pairs, 44000 B returned) |
| 1000 | links | 2634 / 2748 / 2748 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 875 / 989 / 989 | 458 / 513 / 513 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 1000 | cites | 2655 / 4009 / 4009 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 922 / 1211 / 1211 | 308 / 433 / 433 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 10000 | all | 56046 / 60458 / 60458 | 0 gets + 1 scans (10000 pairs, 440000 B returned) | 20437 / 21851 / 21851 | 8022 / 10297 / 10297 | 0 gets + 1 scans (10000 pairs, 440000 B returned) |
| 10000 | links | 28639 / 35450 / 35450 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9492 / 48151 / 48151 | 3867 / 5473 / 5473 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |
| 10000 | cites | 26769 / 29942 / 29942 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9861 / 14782 / 14782 | 3327 / 3642 / 3642 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |

## Consistency (KSE-052)

All 11,111 edges verified bidirectionally on every measured backend: for each outbound (hub -type-> leaf), inbound_edges(leaf, type) contains the hub. Zero divergences — the relo/reli index pair is symmetric over AikoqlStorageEngine exactly as over redb/RocksDB.

## Adjacency-structure verdict

The existing layout IS the knowledge-aware adjacency: every KO's neighbors live in one contiguous key range (relo/<hub>/…), so a lookup is one seek + one linear-in-range scan — see the engine-reqs column (single scan, pairs == fan-out). A custom packed adjacency would save at most the per-row key overhead, and only by moving the write path off the kernel's own index rows. No prototype built; revisit only if a release-build profile shows the per-row key copy dominating.
