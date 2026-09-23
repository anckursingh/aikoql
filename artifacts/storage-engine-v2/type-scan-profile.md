# Type Scan Profile (W5) — SE2-M26

Generated only when `SE2M26_NIGHTLY=1` (strict opt-in). Perf numbers are report cells, never asserts.

- Test: `v2_m26_scan_profile`
- Build mode: release
- Machine: windows/x86_64; 8 logical cores; AMD64 Family 23 Model 24 Stepping 1, AuthenticAMD
- Date: 2026-09-23
- Dataset: one v2 database, 100000 KOs / 10000 deep × 10 versions (SEED 0x270000); one W5 op = `k.scan_by_type` = 1 engine prefix scan over the type index (empty values) + 1 head_object per candidate (2 engine point gets — head + ~1.4 KiB version row — + wire decode + type/Deleted checks + authz read-lock)
- Index shape (capture-pinned): m7_0 → 100000 rows → 100000 returned (harness phase-2 `rmv(.., "m7_0")` restated every KO to m7_0); m7_1..99 → 1000 rows → 0 returned (stale phase-1 entries, rejected by the payload re-check after full decode — stale entries kept by design, kernel.rs:1282); mean candidates per matrix op = 1990
- Matrix reference (09-05 workloads.md, warm): W5 v2 27451 µs vs v1 5534 µs — the cell mixes both shapes via TYPE_ROUND: 10 rounds × 100 types = 1% m7_0 ops + 99% stale-type ops
- Decision-tree thresholds (fixed before the run): scan share < 15% → no index (W5 is get-bound); 15–40% → block-summary investigation opens; > 40% → scan-shape work (posting lists); kernel residual > 30% → kernel-side profiling follow-up

| leg | p50 | p95 | p99 | max | throughput |
|---|---|---|---|---|---|
| W5 kernel op — scan_by_type (rotating) | 25704 µs | 30444 µs | 38138 µs | 1286887 µs | 26 ops/s (mean 38689 µs) |
| engine scan — type/m7_t/ (rotating) | 472 µs | 845 µs | 27496 µs | 43562 µs | 874 ops/s (mean 1144 µs) |
| kernel gets — k.get over scan candidates | 25821 µs | 38106 µs | 49369 µs | 974105 µs | 28 ops/s (mean 36292 µs) |
| hot-type ceiling — m7_0 × 10 | 1274644 µs | 1314893 µs | 1314893 µs | 1314893 µs | 1 ops/s (mean 1255383 µs) |

| leg | lookups/op | cache hits/op | cache misses/op | blocks read/op | bytes read/op | entries decoded/op | get_wall/op |
|---|---|---|---|---|---|---|---|
| W5 kernel op — scan_by_type (rotating) | 3980 | 2512.2 | 1372.4 | 1372.4 | 22246554 | 32799 | 28737 µs |
| engine scan — type/m7_t/ (rotating) | 0 | 1.0 | 4.3 | 4.3 | 70120 | 2564 | 0 µs |
| kernel gets — k.get over scan candidates | 3980 | 2512.2 | 1367.2 | 1367.2 | 22160225 | 30234 | 29502 µs |
| hot-type ceiling — m7_0 × 10 | 200000 | 186266.0 | 8930.0 | 8930.0 | 143307533 | 1675231 | 673116 µs |

## Decomposition (sums over the legs)

- engine prefix scan: 1144 µs of the 38689 µs mean W5 op (3.0%) — leg 2 runs the same rotation on the same prefix
- engine point gets: 28737 µs/op (74.3%) — get_wall accumulated by the gets inside the W5 op (mean 1990 candidates × 2 gets; the mean op includes the 1% m7_0 giant)
- kernel residual: 8808 µs/op (22.8%) = W5 wall − scan − engine gets (decode + type/Deleted checks + authz + assembly)
- per-candidate kernel check: 4.4 µs per candidate in the W5 op vs 3.4 µs per plain k.get in leg 3 (+29.7% per candidate beyond a plain get)
- hot-type ceiling: 1274644 µs p50 when the polluted m7_0 (100_000 KOs) is re-scanned (leg 4, cache-served) vs 25704 µs rotating
- bimodality: p50–p99 are ALL stale-type ops (1000 candidates → 0 returned); the 1% m7_0 op (100_000 candidates → 100_000 returned) is the max column — invisible to p99 but 35% of the mean wall


## Verdict

- scan share 3.0%: no type index / no posting lists / no block summaries — W5 is candidate-bound, not scan-bound (the index already resolves candidates; the cost is the per-candidate head_object); its warm gate-5 cell (27451/5534 = 4.96× v1, 09-05) sits inside the amended ≤8× bound.
- kernel residual 22.8%: no kernel instrumentation — the per-candidate work matches a plain get.
- stale-index note: 99% of matrix ops decode 1000 stale candidates and return 0 — wasted work by design (kernel keeps stale entries); m7_0's 100_000-row scan carries the tail. The harness shape is unchanged (matrix cells are the certification reference).
