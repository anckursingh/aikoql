# W1..W8 Workloads — v2 vs redb vs v1 (MRFC-KSE-001 §27-28 + design §26)

Date: 2026-09-01 · profile: debug (CPU inflated; RSS comparable — kse19) · seed 0x270000 · scale: 2000 KOs / 2000 deep × 10 versions / 2000 ops (smoke — strict opt-in)

The same workload shapes v1's M7 adoption ran, on the same seed. All workloads through the Kernel on `&dyn StorageEngine` (§32). One seeded dataset per backend.

## §28 matrix — throughput + latency

| workload | memory | redb | aikoql | aikoql-v2 |
|---|---|---|---|---|
| KO get (W1) | 20442 ops/s · p50 41 µs · p95 87 · p99 151| 5344 ops/s · p50 150 µs · p95 389 · p99 718| 23313 ops/s · p50 36 µs · p95 60 · p99 159| 16277 ops/s · p50 49 µs · p95 103 · p99 247 |
| head get (W2) | 20715 ops/s · p50 41 µs · p95 81 · p99 160| 5163 ops/s · p50 128 µs · p95 372 · p99 1659| 20886 ops/s · p50 39 µs · p95 91 · p99 182| 16194 ops/s · p50 49 µs · p95 113 · p99 205 |
| version lookup (W3) | 18272 ops/s · p50 46 µs · p95 100 · p99 149| 3919 ops/s · p50 220 µs · p95 456 · p99 790| 14146 ops/s · p50 56 µs · p95 134 · p99 307| 10882 ops/s · p50 75 µs · p95 173 · p99 384 |
| history (W3) | 3547 ops/s · p50 250 µs · p95 416 · p99 714| 1511 ops/s · p50 532 µs · p95 1284 · p99 2723| 2829 ops/s · p50 287 µs · p95 665 · p99 1135| 2514 ops/s · p50 297 µs · p95 739 · p99 2329 |
| relationship lookup F=10 (W4) | 649 ops/s · p50 1341 µs · p95 2870 · p99 3998| 322 ops/s · p50 2922 µs · p95 4840 · p99 6354| 675 ops/s · p50 1407 µs · p95 2457 · p99 2797| 542 ops/s · p50 1710 µs · p95 3063 · p99 4015 |
| relationship lookup F=100 (W4) | 181 ops/s · p50 5335 µs · p95 8048 · p99 8048| 60 ops/s · p50 16388 µs · p95 19154 · p99 19154| 174 ops/s · p50 6007 µs · p95 7967 · p99 7967| 103 ops/s · p50 9794 µs · p95 12750 · p99 12750 |
| relationship lookup F=1000 (W4) | 22 ops/s · p50 45154 µs · p95 56649 · p99 56649| 6 ops/s · p50 164583 µs · p95 191161 · p99 191161| 22 ops/s · p50 44133 µs · p95 49603 · p99 49603| 17 ops/s · p50 58788 µs · p95 63785 · p99 63785 |
| type scan (W5) | 571 ops/s · p50 780 µs · p95 1611 · p99 2936| 127 ops/s · p50 3674 µs · p95 7569 · p99 15421| 528 ops/s · p50 878 µs · p95 1951 · p99 3844| 408 ops/s · p50 1161 µs · p95 2483 · p99 3315 |
| context compilation (W7) | 1227 ops/s · p50 686 µs · p95 1586 · p99 2367| 392 ops/s · p50 2359 µs · p95 4294 · p99 5870| 1285 ops/s · p50 686 µs · p95 1330 · p99 1925| 796 ops/s · p50 1128 µs · p95 2326 · p99 3577 |
| mixed 70/20/10 (W8) | 11712 ops/s · p50 39 µs · p95 381 · p99 586| 792 ops/s · p50 161 µs · p95 10611 · p99 12869| 4041 ops/s · p50 48 µs · p95 1754 · p99 2388| 3612 ops/s · p50 64 µs · p95 1851 · p99 2539 |
| ingestion (W6) | 2239 ops/s · p50 447 µs · p95 447 · p99 447| 102 ops/s · p50 9806 µs · p95 9806 · p99 9806| 565 ops/s · p50 1769 µs · p95 1769 · p99 1769| 538 ops/s · p50 1860 µs · p95 1860 · p99 1860 |

## §28 matrix — logical bytes read / written per workload

| workload | memory | redb | aikoql | aikoql-v2 |
|---|---|---|---|---|
| KO get (W1) | 1731434 / 0| 1731434 / 0| 1731434 / 0| 1731434 / 0 |
| head get (W2) | 1731434 / 0| 1731434 / 0| 1731434 / 0| 1731434 / 0 |
| version lookup (W3) | 15013077 / 0| 15013077 / 0| 15013077 / 0| 15013077 / 0 |
| history (W3) | 15256086 / 0| 15256086 / 0| 15256086 / 0| 15256086 / 0 |
| relationship lookup F=10 (W4) | 4123400 / 0| 4123400 / 0| 4123400 / 0| 4123400 / 0 |
| relationship lookup F=100 (W4) | 1177170 / 0| 1177170 / 0| 1177170 / 0| 1177170 / 0 |
| relationship lookup F=1000 (W4) | 4400000 / 0| 4400000 / 0| 4400000 / 0| 4400000 / 0 |
| type scan (W5) | 17487140 / 0| 17487140 / 0| 17487140 / 0| 17487140 / 0 |
| context compilation (W7) | 8603400 / 0| 8603400 / 0| 8603400 / 0| 8603400 / 0 |
| mixed 70/20/10 (W8) | 1785174 / 422453| 1785174 / 422453| 1785174 / 422453| 1785174 / 422453 |
| ingestion (W6) | 26217272 / 37895054| 26217272 / 37895054| 26217272 / 37895054| 26217272 / 37895054 |

## Per-backend resources

| backend | CPU (seed wall) | RSS (peak, loader child) | disk |
|---|---|---|---|
| memory | 8935 ms | NOT_SAMPLED | 0 B |
| redb | 196155 ms | NOT_SAMPLED | 48.70 MiB |
| aikoql | 35388 ms | NOT_SAMPLED | 39.83 MiB |
| aikoql-v2 | 37204 ms | NOT_SAMPLED | 40.44 MiB |

## §26 adoption gates

| gate (§26) | result | evidence |
|---|---|---|
| 1. recovery bounded by the active WAL | PASS | SE2-M3 suite — artifacts/storage-engine-v2/recovery-independence.md: replay only the active WAL, orphan/missing-segment policies; real-kill recovery suites in M3/M4/M6 |
| 2. dataset larger than RAM remains queryable | PASS | `v2_gate2_3_dataset_larger_than_ram` (this suite): ~820 KB dataset under a 64 KiB memtable + zero cache → served from on-disk segments, full scan byte-exact, survives reopen |
| 3. memory limits configurable | PASS | the same probe pins both knobs: `memtable_bytes=64 KiB` forced flushes (≥2 SEGMENT files); `cache_bytes=0` detaches the cache (silent stats), a 4 KiB cap is consulted (misses) yet holds nothing (oversize block never retained) |
| 4. group commit improves concurrent throughput without weakening Sync | — | SE2-M6 suite green (Sync baseline reproduced exactly); throughput evidence = the `SE2M6_NIGHTLY=1` matrix → artifacts/storage-engine-v2/group-commit.md |
| 5. KO lookup competitive with the MVP baseline (v1) | NOT_EVIDENCED | W1 1.36× v1, W2 1.27× v1 (P50; bound ≤ 2× — perf verdict only at V2ADOPT_NIGHTLY=1, this run is smoke) |

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
