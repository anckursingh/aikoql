# KSE-142 — Recovery Scaling (certification §6)

Date: 2026-09-01 · seed 0x14200000 · engine: AikoqlStorageEngine · build profile: debug (CPU inflated; RSS comparable) · sizes run: 1 MB · test: kse142_recovery_scaling.rs

| WAL (exact) | records | unique keys | live keys | overwrites | recreates | deletes | avg value B | open ms | replay (open−read) ms | first query ms | peak RSS | final RSS |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1.00 MB | 449 | 2694 | 2385 | 22.4% | 2.6% | 11.1% | 256 | 77.4 | 76.6 | 0.012 | 8622080 B | 8622080 B |


## Correctness — 100% semantic recovery (asserted, all sizes)

| check | pin |
|---|---|
| logical key count | scan-all count == model live keys |
| reference key/value data | full equality of every live key against the re-derived model |
| prefix scans | 4 key families, byte-exact against the model slice |
| deletes | last deleted key serves None |
| overwrites | pinned multi-put key serves its FINAL value |
| no corruption | open succeeds only after every envelope checksum verifies (any damage fails closed — KSE-082B) |
| no OOM | the loader child completed within the measured peak RSS |

## Proposed recovery SLO

- open(100 MB WAL) <= 11595 ms
- open(1 GB WAL) <= 115950 ms
computed as linear slope (77.3 ms/MB from the 1.00 MB row) x 1.5 headroom — replay is linear by construction (KSE-15).

## Honest limits

- peak RSS is polled at 100 ms on the loader child — a LOWER BOUND (spikes between samples are missed); a fast smoke child can outrun the sampler's startup, shown as NOT_SAMPLED
- final RSS is a phase-anchored self-report taken after open(), before the validation scans — validation transient memory is not in it
- open_ms is the cold open (read + replay + handles); replay is approximated as open minus a WARM-cache streamed read — an upper bound on replay CPU, because the read inside open ran cold
- the workload keeps a fixed 10K-key keyspace with growing version history — live-key scaling is KSE-19's surface, WAL scaling is this one's
- RSS is Windows-only (PowerShell WorkingSet64); non-Windows rows carry timings with NOT_SAMPLED RSS
- child runs race sibling tests for CPU (kse19 convention); wall times are evidence, not gates
