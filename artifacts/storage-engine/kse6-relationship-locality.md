# KSE-050..052 — Relationship Locality (MRFC-KSE-001 §12)

Measured 2026-08-31 on X (debug build — indicative, not release numbers).
Dataset per backend: 5 hubs with 1/10/100/1,000/10,000 outbound neighbors (interleaved "links"/"cites"), 11,111 leaf KOs, one database per backend, 10 timed reps per lookup.
Allocations: NOT_MEASURED (no counting-allocator instrumentation wired).

| fan-out | op | redb P50/P95/P99 (µs) | redb engine reqs | RocksDB P50/P95/P99 (µs) | Aikoql P50/P95/P99 (µs) | Aikoql engine reqs |
|---|---|---|---|---|---|---|
| 1 | all | 64 / 145 / 145 | 0 gets + 1 scans (1 pairs, 44 B returned) | 16 / 100 / 100 | 4 / 47 / 47 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | links | 64 / 64 / 64 | 0 gets + 1 scans (1 pairs, 44 B returned) | 15 / 16 / 16 | 4 / 5 / 5 | 0 gets + 1 scans (1 pairs, 44 B returned) |
| 1 | cites | 52 / 59 / 59 | 0 gets + 1 scans (0 pairs, 0 B returned) | 12 / 13 / 13 | 3 / 3 / 3 | 0 gets + 1 scans (0 pairs, 0 B returned) |
| 10 | all | 120 / 132 / 132 | 0 gets + 1 scans (10 pairs, 440 B returned) | 33 / 36 / 36 | 11 / 18 / 18 | 0 gets + 1 scans (10 pairs, 440 B returned) |
| 10 | links | 86 / 113 / 113 | 0 gets + 1 scans (5 pairs, 220 B returned) | 24 / 24 / 24 | 6 / 7 / 7 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 10 | cites | 87 / 88 / 88 | 0 gets + 1 scans (5 pairs, 220 B returned) | 23 / 24 / 24 | 7 / 7 / 7 | 0 gets + 1 scans (5 pairs, 220 B returned) |
| 100 | all | 749 / 888 / 888 | 0 gets + 1 scans (100 pairs, 4400 B returned) | 187 / 222 / 222 | 65 / 82 / 82 | 0 gets + 1 scans (100 pairs, 4400 B returned) |
| 100 | links | 388 / 391 / 391 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 101 / 110 / 110 | 34 / 34 / 34 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 100 | cites | 386 / 469 / 469 | 0 gets + 1 scans (50 pairs, 2200 B returned) | 100 / 107 / 107 | 33 / 38 / 38 | 0 gets + 1 scans (50 pairs, 2200 B returned) |
| 1000 | all | 6830 / 9605 / 9605 | 0 gets + 1 scans (1000 pairs, 44000 B returned) | 1696 / 1808 / 1808 | 596 / 789 / 789 | 0 gets + 1 scans (1000 pairs, 44000 B returned) |
| 1000 | links | 2935 / 3776 / 3776 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 863 / 877 / 877 | 298 / 303 / 303 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 1000 | cites | 2727 / 2771 / 2771 | 0 gets + 1 scans (500 pairs, 22000 B returned) | 860 / 891 / 891 | 297 / 333 / 333 | 0 gets + 1 scans (500 pairs, 22000 B returned) |
| 10000 | all | 59220 / 64874 / 64874 | 0 gets + 1 scans (10000 pairs, 440000 B returned) | 20937 / 39155 / 39155 | 7993 / 8781 / 8781 | 0 gets + 1 scans (10000 pairs, 440000 B returned) |
| 10000 | links | 30010 / 67541 / 67541 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 9258 / 11795 / 11795 | 3860 / 4175 / 4175 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |
| 10000 | cites | 28331 / 34365 / 34365 | 0 gets + 1 scans (5000 pairs, 220000 B returned) | 8973 / 9274 / 9274 | 3872 / 4969 / 4969 | 0 gets + 1 scans (5000 pairs, 220000 B returned) |

## Consistency (KSE-052)

All 11,111 edges verified bidirectionally on every measured backend: for each outbound (hub -type-> leaf), inbound_edges(leaf, type) contains the hub. Zero divergences — the relo/reli index pair is symmetric over AikoqlStorageEngine exactly as over redb/RocksDB.

## Adjacency-structure verdict

The existing layout IS the knowledge-aware adjacency: every KO's neighbors live in one contiguous key range (relo/<hub>/…), so a lookup is one seek + one linear-in-range scan — see the engine-reqs column (single scan, pairs == fan-out). A custom packed adjacency would save at most the per-row key overhead, and only by moving the write path off the kernel's own index rows. No prototype built; revisit only if a release-build profile shows the per-row key copy dominating.
