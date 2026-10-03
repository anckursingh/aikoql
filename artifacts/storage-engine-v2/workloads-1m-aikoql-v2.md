# W1..W8 Workloads — aikoql-v2 at 1M (MRFC-KSE-001 §27-28 + design §26)

Date: 2026-09-27 (CI-14/L-24 re-baseline: PR #7's guard run) · profile: release · seed 2555904 (0x270000) · scale: 1000000 KOs / 100000 deep × 10 versions / 200000 ops (STORAGE_REGRESSION=1m — the S-03 self-regression arm)

Single-backend run (STORAGE_BACKEND=aikoql-v2): the matrix holds one row; gate 5 = self-regression, the fresh W1/W2 P50s vs this committed baseline (S-03, bound 1.5×).
The same workload shapes the M7 adoption ran, on the same seed (0x270000). All workloads through the Kernel on `&dyn StorageEngine` (§32). One seeded dataset.

## §28 matrix — throughput + latency

| workload | aikoql-v2 |
|---|---|
| KO get (W1) | 32200 ops/s · p50 30 µs · p95 49 · p99 67 |
| head get (W2) | 37658 ops/s · p50 24 µs · p95 41 · p99 58 |
| version lookup (W3) | 23957 ops/s · p50 39 µs · p95 60 · p99 80 |
| history (W3) | 15730 ops/s · p50 60 µs · p95 88 · p99 111 |
| relationship lookup F=10 (W4) | 5692 ops/s · p50 166 µs · p95 201 · p99 214 |
| relationship lookup F=100 (W4) | 1216 ops/s · p50 789 µs · p95 1111 · p99 1111 |
| relationship lookup F=1000 (W4) | 131 ops/s · p50 7027 µs · p95 10158 · p99 10158 |
| type scan (W5) | 3 ops/s · p50 200683 µs · p95 221131 · p99 242535 |
| context compilation (W7) | 4014 ops/s · p50 238 µs · p95 321 · p99 382 |
| mixed 70/20/10 (W8) | 944 ops/s · p50 50 µs · p95 6078 · p99 22298 |
| ingestion (W6) | 262 ops/s · p50 3818 µs · p95 3818 · p99 3818 |

## §28 matrix — logical bytes read / written per workload

| workload | aikoql-v2 |
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
| ingestion (W6) | 1943921672 / 4128053402 |

## Per-backend resources

| backend | CPU (seed wall) | RSS (peak, loader child) | disk |
|---|---|---|---|
| aikoql-v2 | 10689345.607 ms | not sampled | 2.84 GiB |

## §26 adoption gates

| gate (§26) | result | evidence |
|---|---|---|
| 1. recovery bounded by the active WAL | PASS | SE2-M3 suite — artifacts/storage-engine-v2/recovery-independence.md: replay only the active WAL, orphan/missing-segment policies; real-kill recovery suites in M3/M4/M6 |
| 2. dataset larger than RAM remains queryable | PASS | `v2_gate2_3_dataset_larger_than_ram` (this suite): ~820 KB dataset under a 64 KiB memtable + zero cache → served from on-disk segments, full scan byte-exact, survives reopen |
| 3. memory limits configurable | PASS | the same probe pins both knobs: `memtable_bytes=64 KiB` forced flushes (≥2 SEGMENT files); `cache_bytes=0` detaches the cache (silent stats), a 4 KiB cap is consulted (misses) yet holds nothing (oversize block never retained) |
| 4. group commit improves concurrent throughput without weakening Sync | — | SE2-M6 suite green (Sync baseline reproduced exactly); throughput evidence = the `SE2M6_NIGHTLY=1` matrix → artifacts/storage-engine-v2/group-commit.md |
| 5. gate 5 self-regression ≤ 1.5× (S-03) | PASS | W1 0.50× / W2 0.58× vs the committed pre-refresh baseline (the in-suite gate record); this run IS the committed baseline now (CI-14/L-24), so the next fresh run ratios against these rows |

## Reference rows (not re-measured here)

- snapshot: v2 rides the trait defaults (redb snapshot — REC-002); v1 byte-exact restore pinned (KSE-14); redb single-file opens as redb.
- recovery: v2 real-kill recovery pinned by the SE2-M3/M4/M6 suites (recovery-independence.md); v1 by KSE-15.
- concurrent mixed load: v2 pinned behaviorally by the SE2-M6 group-commit suite (KSE-13 order); v1 by KSE-13. W8 above is the single-threaded mixed row.
- 1M/10M ingestion scale: v1 1M creates = 1242 s / 645 B per KO heap (KSE-19, measured). v2 at 1M: measured by this run (workloads-1m.md, CI-14 re-baseline).

## Honest metric mapping

- throughput/latency: per-op wall on one thread; percentiles over the instrumented pass (P50/P95/P99 in µs — the artifact stores ns, p50_ns per §13; this table divides by 1000)
- bytes read: CountingEngine bytes returned over the workload (get + scan Σ k+v)
- bytes written: CountingEngine batch Σ put k+v (logical, pre-codec)
- W6 ingestion P50/P95/P99 = mean commit cost (the seed loop isn't per-op instrumented)
- CPU: seed wall, single-threaded (wall ≈ CPU); disk: file (redb/aikoql) or dir (aikoql-v2) at seed end; memory = none
- RSS: Windows-only WorkingSet64 poll on a loader child (peak is a lower bound — kse19); CI/ubuntu rows NOT_SAMPLED; this run: not sampled (the RSS arm is weekly/dispatch evidence only — CI-10)
- memory backend: RAM-only reference, not an adoption candidate
- W2 = the same storage leg as W1 (k.get is the kernel's only public head read — KSE-18 pins head+version rows); measured twice on fresh samples, not a faked second API
- v2 RSS on aikoql-v2 includes the 64 MiB memtable + 8 MiB block-cache defaults; gates 2+3 show the knobs bound them
