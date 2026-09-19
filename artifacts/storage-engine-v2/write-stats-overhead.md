# Write-Path Instrumentation Overhead — P3-M2 (met007)

Generated only when `P3M2_ATTRIB=1` (strict opt-in). Perf numbers are report cells, never asserts.

- Test: `v2_p3m2_write_stats_overhead`
- Build mode: release
- Machine: windows/x86_64; 8 logical cores; AMD64 Family 23 Model 24 Stepping 1, AuthenticAMD
- Date: 2026-09-09
- Method: 100000 puts per leg through the instrumented Sync write path (one put per batch); the marginal cost loop runs the exact counter sequence the write path adds per op (1 × wal_bytes fetch_add + 2 × Instant::now + elapsed + 1 × latency-bucket fetch_add + 2 × backlog-gauge fetch_adds — §21 atomics, no allocs) with no engine underneath

## Cells

- 16 B put: mean 832341 ns/op, p50 792200 ns (instrumentation share 0.02%)
- 1400 B put: mean 902174 ns/op (instrumentation share 0.02%)
- marginal instrumentation cost: mean 194.6 ns/op
- counters at end: wal_bytes 152600000 · fsync_count 200000 · flush_count 2 · checkpoint_count 0 · group_commit_batches 0 · write_queue_depth 0
