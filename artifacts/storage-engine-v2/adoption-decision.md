# V2 Adoption Decision (design §26 + MRFC-KSE-001 V2-Adopt)

Scale: smoke (2K KOs / 2K deep × 10 versions / 2K ops) — `V2ADOPT_NIGHTLY` unset. All correctness asserts real; perf verdicts only at the adoption-scale run.

## Gate evidence

| gate (§26) | result | evidence |
|---|---|---|
| conformance: six KSE-1 asserts × 4 backends | PASS | `kse20_backend_conformance_v2` — memory/redb/aikoql/aikoql-v2 all 6/6, reopen probes served identically on the three durable backends (artifacts/storage-engine-v2/conformance.md); granular suite tests/engine.rs green |
| 1. recovery bounded by the active WAL | PASS | SE2-M3 — artifacts/storage-engine-v2/recovery-independence.md: replay only the active WAL, orphan/missing-segment policies; real-kill recovery suites M3/M4/M6 green |
| 2. dataset larger than RAM remains queryable | PASS | `v2_gate2_3_dataset_larger_than_ram` — ~820 KB dataset under a 64 KiB memtable + zero cache: served from ≥2 on-disk segments, full scan byte-exact, spot gets byte-exact, identical after reopen |
| 3. memory limits configurable | PASS | the same probe pins both knobs: `memtable_bytes=64 KiB` forced the flushes; `cache_bytes=0` detaches the cache (silent stats), a 4 KiB cap is consulted (misses) yet holds nothing (oversize block never retained) |
| 4. group commit improves concurrent throughput without weakening Sync | NOT_EVIDENCED | weakening Sync: PASS by the SE2-M6 suite (Sync baseline reproduced byte-exactly, apply-before-ack, every acked batch present under 8-writer load — asserted). throughput gain: NOT evidenced — the shipped `SE2M6_NIGHTLY=1` matrix (release, artifacts/storage-engine-v2/group-commit.md) shows GC 1-writer ≈ Sync (233 vs 229 ms) and 200/200 fsyncs on the 8-writer row: at 25 batches/writer with 5 ms windows the groups never coalesce, so the one-fsync-per-group win has nothing to show. Honest conclusion: the matrix at its current size cannot evidence the perf half of this gate. |
| 5. KO lookup competitive with the MVP baseline (v1) | NOT_EVIDENCED (smoke-favorable) | smoke: W1 p50 42 µs vs v1 37 µs (1.15×), W2 46 vs 31 µs (1.49×) — inside the ≤2× bound everywhere; redb is 3-5× slower on the same rows. Adoption-scale verdict needs `V2ADOPT_NIGHTLY=1`. |

## Smoke summary (debug, per-backend, one seeded dataset each)

- KO lookups: v2 within 1.5× of v1 on every W1..W8 row; every row beats redb by ≥2.5× except F=1000 traversal (3.7×).
- Ingestion: v2 548 ops/s vs v1 600 vs redb 85; disk 40.44 MiB vs v1 39.83 vs redb 48.70; CPU (seed wall) 36.5 s vs 33.3 vs 234.
- Cache (release, SE2-M7 matrix): warm second pass 125 ms vs cold 558 ms — 4.5×, 3993 hits / 7 misses.

## What the adoption-scale run must show

`V2ADOPT_NIGHTLY=1 cargo test --release -p aikoql-storage-v2 --test kse_m7_v2_workloads` regenerates artifacts/storage-engine-v2/workloads.md at 100K KOs / 10K deep × 10 / 20K ops: gate 5 flips to PASS/FAIL against the ≤2×-of-v1 bound on W1/W2. Gate 4's throughput claim needs either a matrix where concurrent submitters actually share windows (a follow-up to the M6 harness, not a re-run) or the gate is recorded as mechanism-only.

## Verdict

VERDICT: v2 stays OPT-IN (default remains aikoql v1) — ADOPT PENDING V2ADOPT_NIGHTLY=1: conformance + gates 1-3 PASS with committed evidence, gate 5 within bound at smoke, gate 4's throughput half not evidenced at the shipped matrix size
