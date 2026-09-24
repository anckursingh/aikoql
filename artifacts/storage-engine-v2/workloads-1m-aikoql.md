# W1..W8 Workloads — v2 vs redb vs v1 (MRFC-KSE-001 §27-28 + design §26)

Date: 2026-09-10 · profile: release · seed 0x270000 · scale: 1000000 KOs / 100000 deep × 10 versions / 200000 ops (V2ADOPT_NIGHTLY=1m — strict opt-in)

Single-backend run (V2ADOPT_BACKEND=aikoql — SE2-M28 staged): the matrix holds one row; gate 5 is decided across the aikoql-v2 and aikoql runs' cells.
The same workload shapes v1's M7 adoption ran, on the same seed. All workloads through the Kernel on `&dyn StorageEngine` (§32). One seeded dataset per backend.

## §28 matrix — throughput + latency

| workload | aikoql |
|---|---|
| KO get (W1) | 128423 ops/s · p50 7 µs · p95 10 · p99 13 |
| head get (W2) | 133532 ops/s · p50 7 µs · p95 10 · p99 12 |
| version lookup (W3) | 90730 ops/s · p50 10 µs · p95 14 · p99 17 |
| history (W3) | 22200 ops/s · p50 41 µs · p95 66 · p99 105 |
| relationship lookup F=10 (W4) | 6186 ops/s · p50 162 µs · p95 240 · p99 313 |
| relationship lookup F=100 (W4) | 1821 ops/s · p50 535 µs · p95 872 · p99 872 |
| relationship lookup F=1000 (W4) | 234 ops/s · p50 4163 µs · p95 4828 · p99 4828 |
| type scan (W5) | 6 ops/s · p50 66531 µs · p95 87576 · p99 184086 |
| context compilation (W7) | 11859 ops/s · p50 74 µs · p95 143 · p99 209 |
| mixed 70/20/10 (W8) | 9377 ops/s · p50 10 µs · p95 802 · p99 1190 |
| ingestion (W6) | 1205 ops/s · p50 830 µs · p95 830 · p99 830 |

## §28 matrix — logical bytes read / written per workload

| workload | aikoql |
|---|---|
| KO get (W1) | 139462290 / 0 |
| head get (W2) | 139462290 / 0 |
| version lookup (W3) | 1473052565 / 0 |
| history (W3) | 1472323609 / 0 |
| relationship lookup F=10 (W4) | 4123400 / 0 |
| relationship lookup F=100 (W4) | 1177170 / 0 |
| relationship lookup F=1000 (W4) | 4400000 / 0 |
| type scan (W5) | 14405308680 / 0 |
| context compilation (W7) | 490440085 / 0 |
| mixed 70/20/10 (W8) | 143286530 / 38501738 |
| ingestion (W6) | 1943921672 / 4128052739 |

## Per-backend resources

| backend | CPU (seed wall) | RSS (peak, loader child) | disk |
|---|---|---|---|
| aikoql | 2323827 ms | 5.90 GiB | 4.24 GiB |

## §26 adoption gates

| gate (§26) | result | evidence |
|---|---|---|
| 1. recovery bounded by the active WAL | PASS | SE2-M3 suite — artifacts/storage-engine-v2/recovery-independence.md: replay only the active WAL, orphan/missing-segment policies; real-kill recovery suites in M3/M4/M6 |
| 2. dataset larger than RAM remains queryable | PASS | `v2_gate2_3_dataset_larger_than_ram` (this suite): ~820 KB dataset under a 64 KiB memtable + zero cache → served from on-disk segments, full scan byte-exact, survives reopen |
| 3. memory limits configurable | PASS | the same probe pins both knobs: `memtable_bytes=64 KiB` forced flushes (≥2 SEGMENT files); `cache_bytes=0` detaches the cache (silent stats), a 4 KiB cap is consulted (misses) yet holds nothing (oversize block never retained) |
| 4. group commit improves concurrent throughput without weakening Sync | — | SE2-M6 suite green (Sync baseline reproduced exactly); throughput evidence = the `SE2M6_NIGHTLY=1` matrix → artifacts/storage-engine-v2/group-commit.md |
| 5. KO lookup competitive with the MVP baseline (v1) | NOT_EVIDENCED | W1 0.00× v1, W2 0.00× v1 (P50; bound ≤ 8× — perf verdict only on a real (non-smoke) matrix run; this run is V2ADOPT_NIGHTLY=1m) |

## Reference rows (not re-measured here)

- snapshot: v2 rides the trait defaults (redb snapshot — REC-002); v1 byte-exact restore pinned (KSE-14); redb single-file opens as redb.
- recovery: v2 real-kill recovery pinned by the SE2-M3/M4/M6 suites (recovery-independence.md); v1 by KSE-15.
- concurrent mixed load: v2 pinned behaviorally by the SE2-M6 group-commit suite (KSE-13 order); v1 by KSE-13. W8 above is the single-threaded mixed row.
- 1M/10M ingestion scale: v1 1M creates = 1242 s / 645 B per KO heap (KSE-19, measured). v2 at 1M: measured by this run (workloads-1m.md, SE2-M28).

## Honest metric mapping

- throughput/latency: per-op wall on one thread; percentiles over the instrumented pass (P50/P95/P99 in µs)
- bytes read: CountingEngine bytes returned over the workload (get + scan Σ k+v)
- bytes written: CountingEngine batch Σ put k+v (logical, pre-codec)
- W6 ingestion P50/P95/P99 = mean commit cost (the seed loop isn't per-op instrumented)
- CPU: seed wall, single-threaded (wall ≈ CPU); disk: file (redb/aikoql) or dir (aikoql-v2) at seed end; memory = none
- RSS: Windows-only WorkingSet64 poll on a loader child (peak is a lower bound — kse19); CI/ubuntu rows NOT_SAMPLED
- memory backend: RAM-only reference, not an adoption candidate
- W2 = the same storage leg as W1 (k.get is the kernel's only public head read — KSE-18 pins head+version rows); measured twice on fresh samples, not a faked second API
- v2 RSS on aikoql-v2 includes the 64 MiB memtable + 8 MiB block-cache defaults; gates 2+3 show the knobs bound them
