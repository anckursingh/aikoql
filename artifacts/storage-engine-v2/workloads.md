# W1..W8 Workloads — v2 vs redb vs v1 (MRFC-KSE-001 §27-28 + design §26)

Date: 2026-09-01 · profile: release · seed 0x270000 · scale: 100000 KOs / 10000 deep × 10 versions / 20000 ops (V2ADOPT_NIGHTLY — strict opt-in)

The same workload shapes v1's M7 adoption ran, on the same seed. All workloads through the Kernel on `&dyn StorageEngine` (§32). One seeded dataset per backend.

## §28 matrix — throughput + latency

| workload | memory | redb | aikoql | aikoql-v2 |
|---|---|---|---|---|
| KO get (W1) | 163665 ops/s · p50 6 µs · p95 9 · p99 13| 43219 ops/s · p50 19 µs · p95 46 · p99 142| 160518 ops/s · p50 6 µs · p95 8 · p99 10| 24045 ops/s · p50 39 µs · p95 70 · p99 101 |
| head get (W2) | 164338 ops/s · p50 6 µs · p95 8 · p99 10| 118066 ops/s · p50 8 µs · p95 11 · p99 13| 158090 ops/s · p50 6 µs · p95 8 · p99 13| 26944 ops/s · p50 37 µs · p95 60 · p99 78 |
| version lookup (W3) | 101868 ops/s · p50 9 µs · p95 14 · p99 20| 81187 ops/s · p50 12 µs · p95 14 · p99 18| 104057 ops/s · p50 9 µs · p95 12 · p99 15| 20438 ops/s · p50 46 µs · p95 82 · p99 116 |
| history (W3) | 25914 ops/s · p50 33 µs · p95 67 · p99 82| 24227 ops/s · p50 36 µs · p95 67 · p99 112| 31060 ops/s · p50 32 µs · p95 38 · p99 51| 13694 ops/s · p50 70 µs · p95 107 · p99 139 |
| relationship lookup F=10 (W4) | 6501 ops/s · p50 152 µs · p95 160 · p99 185| 4932 ops/s · p50 157 µs · p95 298 · p99 350| 7986 ops/s · p50 123 µs · p95 135 · p99 156| 4469 ops/s · p50 231 µs · p95 242 · p99 327 |
| relationship lookup F=100 (W4) | 1991 ops/s · p50 492 µs · p95 581 · p99 581| 1269 ops/s · p50 714 µs · p95 1264 · p99 1264| 2434 ops/s · p50 403 µs · p95 480 · p99 480| 957 ops/s · p50 1010 µs · p95 1407 · p99 1407 |
| relationship lookup F=1000 (W4) | 213 ops/s · p50 4811 µs · p95 4885 · p99 4885| 135 ops/s · p50 7006 µs · p95 8414 · p99 8414| 248 ops/s · p50 4025 µs · p95 4207 · p99 4207| 95 ops/s · p50 9894 µs · p95 12878 · p99 12878 |
| type scan (W5) | 83 ops/s · p50 5430 µs · p95 7380 · p99 12436| 61 ops/s · p50 7702 µs · p95 9433 · p99 21661| 91 ops/s · p50 5485 µs · p95 6643 · p99 14543| 24 ops/s · p50 22686 µs · p95 27858 · p99 41263 |
| context compilation (W7) | 15234 ops/s · p50 57 µs · p95 104 · p99 149| 8835 ops/s · p50 110 µs · p95 151 · p99 175| 16116 ops/s · p50 56 µs · p95 91 · p99 115| 3269 ops/s · p50 274 µs · p95 495 · p99 793 |
| mixed 70/20/10 (W8) | 95988 ops/s · p50 6 µs · p95 38 · p99 57| 1436 ops/s · p50 12 µs · p95 2825 · p99 31144| 1301 ops/s · p50 8 µs · p95 774 · p99 31316| 7746 ops/s · p50 46 µs · p95 788 · p99 1073 |
| ingestion (W6) | 44121 ops/s · p50 23 µs · p95 23 · p99 23| 444 ops/s · p50 2250 µs · p95 2250 · p99 2250| 1306 ops/s · p50 766 µs · p95 766 · p99 766| 1307 ops/s · p50 765 µs · p95 765 · p99 765 |

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
| memory | 6346 ms | NOT_SAMPLED | 0 B |
| redb | 630070 ms | 510.23 MiB | 1.00 GiB |
| aikoql | 214442 ms | 613.38 MiB | 435.44 MiB |
| aikoql-v2 | 214248 ms | 1.35 GiB | 347.99 MiB |

## §26 adoption gates

| gate (§26) | result | evidence |
|---|---|---|
| 1. recovery bounded by the active WAL | PASS | SE2-M3 suite — artifacts/storage-engine-v2/recovery-independence.md: replay only the active WAL, orphan/missing-segment policies; real-kill recovery suites in M3/M4/M6 |
| 2. dataset larger than RAM remains queryable | PASS | `v2_gate2_3_dataset_larger_than_ram` (this suite): ~820 KB dataset under a 64 KiB memtable + zero cache → served from on-disk segments, full scan byte-exact, survives reopen |
| 3. memory limits configurable | PASS | the same probe pins both knobs: `memtable_bytes=64 KiB` forced flushes (≥2 SEGMENT files); `cache_bytes=0` detaches the cache (silent stats), a 4 KiB cap is consulted (misses) yet holds nothing (oversize block never retained) |
| 4. group commit improves concurrent throughput without weakening Sync | — | SE2-M6 suite green (Sync baseline reproduced exactly); throughput evidence = the `SE2M6_NIGHTLY=1` matrix → artifacts/storage-engine-v2/group-commit.md |
| 5. KO lookup competitive with the MVP baseline (v1) | FAIL | W1 6.61× v1, W2 6.13× v1 (P50; bound ≤ 2× — perf verdict only at V2ADOPT_NIGHTLY=1, this run is V2ADOPT_NIGHTLY) |

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
