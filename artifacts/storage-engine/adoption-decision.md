# Storage Engine Adoption Decision (MRFC-KSE-001 §29-31)

Scale: 100000 KOs / 10000 deep × 10 versions (M7_NIGHTLY — strict opt-in).

## Gate evidence

| gate (§29) | result | evidence |
|---|---|---|
| correctness: P0 100%, P1 ≥98% | PASS | P0 33/33, P1 13/13 — artifacts/mvp-test-report.md (committed); six KSE-1 asserts 6/6 on all four backends (KSE-20) |
| reliability: 0 unrecoverable crash cases | PASS | KSE-9 WAL fault injection green; KSE-15 real-kill recovered seqs exactly 1..=n; KSE-12/13 stress green |
| maintainability: no unjustified operational burden | PASS | single-file enveloped WAL format (KSE-3), zero new external deps, all backend access behind &dyn StorageEngine (§32 — KSE-20), per-backend capability divergences documented in conformance.md |
| performance: ≥2× on ≥1 important workload, no core workload >2× slower | PASS | vs redb P50: best 2.90× (ingestion (W6)), worst 0.91× (history (W3)) |
| resource: no unacceptable RAM/CPU/disk regression | PASS | disk 0.42× redb, CPU 0.34× redb, RSS 1.19× redb (bounds encoded: disk ≤2×, CPU ≤2×, RAM ≤3× — RSS Windows-only) |

## Verdict

ADOPT AIKOQL STORAGE ENGINE
