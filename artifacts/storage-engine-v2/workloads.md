# W1..W8 Workloads — v2 vs redb vs v1 (MRFC-KSE-001 §27-28 + design §26)

Date: 2026-09-04 · profile: release · seed 0x270000 · scale: 100000 KOs / 10000 deep × 10 versions / 20000 ops (V2ADOPT_NIGHTLY — strict opt-in)

The same workload shapes v1's M7 adoption ran, on the same seed. All workloads through the Kernel on `&dyn StorageEngine` (§32). One seeded dataset per backend.

## §28 matrix — throughput + latency

| workload | memory | redb | aikoql | aikoql-v2 |
|---|---|---|---|---|
| KO get (W1) | 178200 ops/s · p50 6 µs · p95 7 · p99 8| 49648 ops/s · p50 16 µs · p95 40 · p99 120| 167768 ops/s · p50 6 µs · p95 8 · p99 9| 25955 ops/s · p50 37 µs · p95 66 · p99 90 |
| head get (W2) | 178026 ops/s · p50 5 µs · p95 7 · p99 9| 122639 ops/s · p50 8 µs · p95 10 · p99 14| 182634 ops/s · p50 5 µs · p95 7 · p99 8| 27404 ops/s · p50 36 µs · p95 61 · p99 81 |
| version lookup (W3) | 96451 ops/s · p50 10 µs · p95 12 · p99 18| 79457 ops/s · p50 12 µs · p95 17 · p99 23| 115444 ops/s · p50 8 µs · p95 10 · p99 12| 20986 ops/s · p50 45 µs · p95 80 · p99 110 |
| history (W3) | 29396 ops/s · p50 32 µs · p95 40 · p99 59| 19826 ops/s · p50 40 µs · p95 74 · p99 151| 31012 ops/s · p50 31 µs · p95 39 · p99 53| 14045 ops/s · p50 69 µs · p95 103 · p99 133 |
| relationship lookup F=10 (W4) | 8175 ops/s · p50 118 µs · p95 147 · p99 212| 5284 ops/s · p50 186 µs · p95 201 · p99 275| 8292 ops/s · p50 119 µs · p95 126 · p99 164| 4225 ops/s · p50 203 µs · p95 462 · p99 581 |
| relationship lookup F=100 (W4) | 2407 ops/s · p50 394 µs · p95 537 · p99 537| 1228 ops/s · p50 790 µs · p95 981 · p99 981| 2441 ops/s · p50 401 µs · p95 469 · p99 469| 838 ops/s · p50 1202 µs · p95 1678 · p99 1678 |
| relationship lookup F=1000 (W4) | 246 ops/s · p50 4018 µs · p95 4306 · p99 4306| 106 ops/s · p50 9941 µs · p95 10276 · p99 10276| 256 ops/s · p50 3905 µs · p95 4022 · p99 4022| 88 ops/s · p50 10972 µs · p95 14338 · p99 14338 |
| type scan (W5) | 83 ops/s · p50 5390 µs · p95 7202 · p99 23738| 59 ops/s · p50 7992 µs · p95 10638 · p99 17333| 94 ops/s · p50 5232 µs · p95 6353 · p99 7584| 24 ops/s · p50 22627 µs · p95 29050 · p99 50051 |
| context compilation (W7) | 14551 ops/s · p50 58 µs · p95 106 · p99 137| 8437 ops/s · p50 112 µs · p95 162 · p99 208| 16347 ops/s · p50 56 µs · p95 88 · p99 106| 3882 ops/s · p50 252 µs · p95 349 · p99 470 |
| mixed 70/20/10 (W8) | 99265 ops/s · p50 6 µs · p95 37 · p99 52| 3324 ops/s · p50 12 µs · p95 2769 · p99 3204| 1363 ops/s · p50 8 µs · p95 743 · p99 31288| 7917 ops/s · p50 43 µs · p95 798 · p99 1061 |
| ingestion (W6) | 43665 ops/s · p50 23 µs · p95 23 · p99 23| 424 ops/s · p50 2358 µs · p95 2358 · p99 2358| 1216 ops/s · p50 822 µs · p95 822 · p99 822| 1263 ops/s · p50 792 µs · p95 792 · p99 792 |

## §28 matrix — logical bytes read / written per workload

| workload | memory | redb | aikoql | aikoql-v2 |
|---|---|---|---|---|
| KO get (W1) | 13951795 / 0| 13951795 / 0| 13951795 / 0| 13913136 / 0 |
| head get (W2) | 13951795 / 0| 13951795 / 0| 13951795 / 0| 13913136 / 0 |
| version lookup (W3) | 147466142 / 0| 147466142 / 0| 147466142 / 0| 147466142 / 0 |
| history (W3) | 147566712 / 0| 147566712 / 0| 147566712 / 0| 147566712 / 0 |
| relationship lookup F=10 (W4) | 4123400 / 0| 4123400 / 0| 4123400 / 0| 4067200 / 0 |
| relationship lookup F=100 (W4) | 1177170 / 0| 1177170 / 0| 1177170 / 0| 1144870 / 0 |
| relationship lookup F=1000 (W4) | 4400000 / 0| 4400000 / 0| 4400000 / 0| 4279640 / 0 |
| type scan (W5) | 1441114680 / 0| 1441114680 / 0| 1441114680 / 0| 1437125780 / 0 |
| context compilation (W7) | 48818925 / 0| 48818925 / 0| 48818925 / 0| 48719559 / 0 |
| mixed 70/20/10 (W8) | 14354533 / 3853729| 14354533 / 3853729| 14354533 / 3853729| 14320634 / 3849734 |
| ingestion (W6) | 194861672 / 413844730| 194861672 / 413844730| 194861672 / 413844730| 194861672 / 413840735 |

## Per-backend resources

| backend | CPU (seed wall) | RSS (peak, loader child) | disk |
|---|---|---|---|
| memory | 6413 ms | NOT_SAMPLED | 0 B |
| redb | 660113 ms | 513.38 MiB | 1.00 GiB |
| aikoql | 230219 ms | 611.22 MiB | 435.44 MiB |
| aikoql-v2 | 221646 ms | 428.05 MiB | 347.99 MiB |

## §26 adoption gates

| gate (§26) | result | evidence |
|---|---|---|
| 1. recovery bounded by the active WAL | PASS | SE2-M3 suite — artifacts/storage-engine-v2/recovery-independence.md: replay only the active WAL, orphan/missing-segment policies; real-kill recovery suites in M3/M4/M6 |
| 2. dataset larger than RAM remains queryable | PASS | `v2_gate2_3_dataset_larger_than_ram` (this suite): ~820 KB dataset under a 64 KiB memtable + zero cache → served from on-disk segments, full scan byte-exact, survives reopen |
| 3. memory limits configurable | PASS | the same probe pins both knobs: `memtable_bytes=64 KiB` forced flushes (≥2 SEGMENT files); `cache_bytes=0` detaches the cache (silent stats), a 4 KiB cap is consulted (misses) yet holds nothing (oversize block never retained) |
| 4. group commit improves concurrent throughput without weakening Sync | — | SE2-M6 suite green (Sync baseline reproduced exactly); throughput evidence = the `SE2M6_NIGHTLY=1` matrix → artifacts/storage-engine-v2/group-commit.md |
| 5. KO lookup competitive with the MVP baseline (v1) | FAIL | W1 6.53× v1, W2 6.81× v1 (P50; bound ≤ 2× — perf verdict only at V2ADOPT_NIGHTLY=1, this run is V2ADOPT_NIGHTLY) |

## Reference rows (not re-measured here)

- snapshot: v2 rides the trait defaults (redb snapshot — REC-002); v1 byte-exact restore pinned (KSE-14); redb single-file opens as redb.
- recovery: v2 real-kill recovery pinned by the SE2-M3/M4/M6 suites (recovery-independence.md); v1 by KSE-15.
- concurrent mixed load: v2 pinned behaviorally by the SE2-M6 group-commit suite (KSE-13 order); v1 by KSE-13. W8 above is the single-threaded mixed row.
- 1M/10M ingestion scale: v1 1M creates = 1242 s / 645 B per KO heap (KSE-19, measured). v2 at 1M NOT_MEASURED.

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
