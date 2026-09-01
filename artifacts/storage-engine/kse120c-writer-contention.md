# KSE-120C — Writer Contention Scaling (certification §5)

Date: 2026-09-01 · seed 0x120c0000 · engine: AikoqlStorageEngine · build profile: debug (CPU inflated; RSS comparable) · workload: 800 puts per scenario (unique keys, 256 B values) · test: kse120c_writer_contention.rs

| writers | readers | writes | writes/sec | write P50/P95/P99 ms | reads | reads/sec | read P50/P95/P99 ms | wall s | recovered == acked |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | 0 | 800 | 998 | 0.91 / 1.51 / 1.67 | 0 | — | — | 0.8 | ✓ (asserted, byte-exact) |
| 1 | 32 | 800 | 864 | 1.05 / 1.65 / 1.87 | 16000 | 17271 | 0.00 / 0.00 / 0.01 | 0.9 | ✓ (asserted, byte-exact) |
| 2 | 32 | 800 | 1000 | 1.86 / 2.83 / 3.25 | 16000 | 20007 | 0.00 / 0.00 / 0.01 | 0.8 | ✓ (asserted, byte-exact) |
| 4 | 32 | 800 | 980 | 3.78 / 5.04 / 7.00 | 16000 | 19604 | 0.00 / 0.00 / 0.01 | 0.8 | ✓ (asserted, byte-exact) |
| 8 | 32 | 800 | 979 | 7.81 / 9.55 / 14.72 | 16000 | 19580 | 0.00 / 0.00 / 0.00 | 0.8 | ✓ (asserted, byte-exact) |
| 16 | 32 | 800 | 984 | 15.90 / 17.68 / 31.28 | 16000 | 19676 | 0.00 / 0.00 / 0.01 | 0.8 | ✓ (asserted, byte-exact) |
| 32 | 32 | 800 | 989 | 31.58 / 36.12 / 37.39 | 16000 | 19785 | 0.00 / 0.00 / 0.01 | 0.8 | ✓ (asserted, byte-exact) |


## Proposed SLOs (reported, not asserted)

- 100% acknowledged-write recovery at every writer count — the only asserted gate (all scenarios, above)
- write P50 at 1 writer <= 1.4 ms (measured 0.91 ms; 1.5x headroom)
- throughput must not collapse: 32-writer rate >= 25% of the 1-writer rate (measured 989/sec vs 998/sec = 99%) — serialization is intentional (log Mutex across append+fsync+apply, KSE-13 120a), so plateau is expected; a collapse would signal lock or scheduling pathology


## NOT_MEASURED (metrics that cannot be measured here)

- lock/queue wait: the serialized section is engine-internal — write P50 vs the 1-writer baseline IS the contention proxy; a separate number would need production instrumentation
- WAL append time / fsync time: one serialized section, engine-internal — not separable without production instrumentation; the behavioral pin is KSE-13 KSE-120a (log order == commit order)
- CPU: single-machine wall time is the scenario column; per-thread CPU attribution is not separable
- RSS: steady-state memory is KSE-19/143's surface; the contention matrix adds no durable state

## Honest limits

- contention surface is within-process threads — the engine does not support multi-process sharing (documented), and the kernel's own pipeline is single-writer; the 32-writer row is deliberately beyond any real AIKOQL workload
- readers hammer random keys, hit rate grows during the run; read latency includes None gets
- write latency includes fsync (the serialized section) — it is durability cost, not lock cost
- debug builds inflate CPU but not the serialization shape; nightly rows should be produced in release
- wall times race sibling tests (kse19 convention); evidence, not gates
