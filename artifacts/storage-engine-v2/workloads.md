# W1..W8 Workloads — v2 vs redb vs v1 (MRFC-KSE-001 §27-28 + design §26)

Date: 2026-09-01 · profile: release · seed 0x270000 · scale: 100000 KOs / 10000 deep × 10 versions / 20000 ops (V2ADOPT_NIGHTLY — strict opt-in)

The same workload shapes v1's M7 adoption ran, on the same seed. All workloads through the Kernel on `&dyn StorageEngine` (§32). One seeded dataset per backend.

## §28 matrix — throughput + latency

| workload | memory | redb | aikoql | aikoql-v2 |
|---|---|---|---|---|
| KO get (W1) | 140704 ops/s · p50 6 µs · p95 12 · p99 17| 43556 ops/s · p50 19 µs · p95 45 · p99 121| 169368 ops/s · p50 6 µs · p95 7 · p99 18| 1951 ops/s · p50 399 µs · p95 938 · p99 1190 |
| head get (W2) | 132578 ops/s · p50 7 µs · p95 11 · p99 21| 125762 ops/s · p50 8 µs · p95 10 · p99 14| 171821 ops/s · p50 6 µs · p95 7 · p99 11| 1996 ops/s · p50 391 µs · p95 917 · p99 1170 |
| version lookup (W3) | 66662 ops/s · p50 12 µs · p95 22 · p99 55| 79375 ops/s · p50 12 µs · p95 14 · p99 25| 96710 ops/s · p50 9 µs · p95 16 · p99 22| 2075 ops/s · p50 466 µs · p95 758 · p99 1028 |
| history (W3) | 21505 ops/s · p50 41 µs · p95 74 · p99 129| 23730 ops/s · p50 37 µs · p95 65 · p99 103| 26201 ops/s · p50 34 µs · p95 57 · p99 76| 1833 ops/s · p50 504 µs · p95 889 · p99 1422 |
| relationship lookup F=10 (W4) | 8008 ops/s · p50 121 µs · p95 153 · p99 189| 6364 ops/s · p50 155 µs · p95 164 · p99 238| 8322 ops/s · p50 117 µs · p95 132 · p99 185| 246 ops/s · p50 3725 µs · p95 5396 · p99 6859 |
| relationship lookup F=100 (W4) | 1847 ops/s · p50 516 µs · p95 1005 · p99 1005| 1447 ops/s · p50 676 µs · p95 835 · p99 835| 2331 ops/s · p50 416 µs · p95 488 · p99 488| 47 ops/s · p50 21268 µs · p95 22698 · p99 22698 |
| relationship lookup F=1000 (W4) | 191 ops/s · p50 4717 µs · p95 6497 · p99 6497| 148 ops/s · p50 6678 µs · p95 7104 · p99 7104| 241 ops/s · p50 4160 µs · p95 4165 · p99 4165| 4 ops/s · p50 215791 µs · p95 241297 · p99 241297 |
| type scan (W5) | 70 ops/s · p50 6531 µs · p95 10125 · p99 45202| 59 ops/s · p50 8194 µs · p95 10576 · p99 16901| 90 ops/s · p50 5503 µs · p95 6817 · p99 18657| 2 ops/s · p50 328465 µs · p95 481220 · p99 968221 |
| context compilation (W7) | 13225 ops/s · p50 68 µs · p95 123 · p99 199| 9393 ops/s · p50 101 µs · p95 143 · p99 168| 17479 ops/s · p50 53 µs · p95 86 · p99 103| 178 ops/s · p50 5421 µs · p95 7403 · p99 9283 |
| mixed 70/20/10 (W8) | 82930 ops/s · p50 7 µs · p95 43 · p99 74| 759 ops/s · p50 15 µs · p95 3833 · p99 33657| 1377 ops/s · p50 9 µs · p95 829 · p99 31469| 910 ops/s · p50 838 µs · p95 2469 · p99 3535 |
| ingestion (W6) | 36077 ops/s · p50 28 µs · p95 28 · p99 28| 334 ops/s · p50 2995 µs · p95 2995 · p99 2995| 1328 ops/s · p50 753 µs · p95 753 · p99 753| 950 ops/s · p50 1053 µs · p95 1053 · p99 1053 |

## §28 matrix — logical bytes read / written per workload

| workload | memory | redb | aikoql | aikoql-v2 |
|---|---|---|---|---|
| KO get (W1) | 13951795 / 0| 13951795 / 0| 13951795 / 0| 13951795 / 0 |
| head get (W2) | 13951795 / 0| 13951795 / 0| 13951795 / 0| 13951795 / 0 |
| version lookup (W3) | 147466142 / 0| 147466142 / 0| 147466142 / 0| 147466142 / 0 |
| history (W3) | 147566712 / 0| 147566712 / 0| 147566712 / 0| 147566712 / 0 |
| relationship lookup F=10 (W4) | 4123400 / 0| 4123400 / 0| 4123400 / 0| 4123400 / 0 |
| relationship lookup F=100 (W4) | 1177170 / 0| 1177170 / 0| 1177170 / 0| 1177170 / 0 |
| relationship lookup F=1000 (W4) | 4400000 / 0| 4400000 / 0| 4400000 / 0| 4400000 / 0 |
| type scan (W5) | 1441114680 / 0| 1441114680 / 0| 1441114680 / 0| 1441114680 / 0 |
| context compilation (W7) | 48818925 / 0| 48818925 / 0| 48818925 / 0| 48818925 / 0 |
| mixed 70/20/10 (W8) | 14354533 / 3853729| 14354533 / 3853729| 14354533 / 3853729| 14354533 / 3853729 |
| ingestion (W6) | 194861672 / 413844730| 194861672 / 413844730| 194861672 / 413844730| 194861672 / 413844730 |

## Per-backend resources

| backend | CPU (seed wall) | RSS (peak, loader child) | disk |
|---|---|---|---|
| memory | 7761 ms | NOT_SAMPLED | 0 B |
| redb | 838661 ms | 513.94 MiB | 1.00 GiB |
| aikoql | 210822 ms | 613.56 MiB | 435.44 MiB |
| aikoql-v2 | 294863 ms | 496.93 MiB | 354.36 MiB |

## §26 adoption gates

| gate (§26) | result | evidence |
|---|---|---|
| 1. recovery bounded by the active WAL | PASS | SE2-M3 suite — artifacts/storage-engine-v2/recovery-independence.md: replay only the active WAL, orphan/missing-segment policies; real-kill recovery suites in M3/M4/M6 |
| 2. dataset larger than RAM remains queryable | PASS | `v2_gate2_3_dataset_larger_than_ram` (this suite): ~820 KB dataset under a 64 KiB memtable + zero cache → served from on-disk segments, full scan byte-exact, survives reopen |
| 3. memory limits configurable | PASS | the same probe pins both knobs: `memtable_bytes=64 KiB` forced flushes (≥2 SEGMENT files); `cache_bytes=0` detaches the cache (silent stats), a 4 KiB cap is consulted (misses) yet holds nothing (oversize block never retained) |
| 4. group commit improves concurrent throughput without weakening Sync | — | SE2-M6 suite green (Sync baseline reproduced exactly); throughput evidence = the `SE2M6_NIGHTLY=1` matrix → artifacts/storage-engine-v2/group-commit.md |
| 5. KO lookup competitive with the MVP baseline (v1) | FAIL | W1 72.53× v1, W2 69.79× v1 (P50; bound ≤ 2× — perf verdict only at V2ADOPT_NIGHTLY=1, this run is V2ADOPT_NIGHTLY) |

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
