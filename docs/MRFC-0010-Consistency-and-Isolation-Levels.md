# MRFC-0010: Consistency & Isolation Levels

- **Status:** Draft v1.0 (implemented behavior — codifies Phase 1 Inc-1/2)
- **Project:** aikoql
- **Category:** Foundation / Concurrency
- **Depends on:** MRFC-0001 (KOM), MRFC-0008 (Commit Pipeline & Journal)
- **Supersedes:** None

> This RFC is **normative**. Keywords **MUST**, **SHALL**, **SHOULD**, **MAY**, and **MUST NOT** are interpreted as defined by RFC 2119.

---

# 1. Abstract

This RFC defines the consistency and isolation semantics exposed by the Aikoql Knowledge Kernel: a single-writer commit pipeline, snapshot isolation for readers, optimistic concurrency control for writers, and idempotency for retry-safe clients. These semantics are implemented directly by the storage pipeline and observable through the syscall ABI (MRFC-0011).

---

# 2. Goals

- Define the read-isolation model clients can rely on.
- Prevent lost updates without pessimistic locking.
- Make retries and replays safe via deterministic idempotency.
- Preserve a causal total order of all writes via the HLC.
- Specify the trade-off between strong consistency and index freshness.

## Non-goals

- Distributed linearizability or Raft/Paxos protocols.
- SQL transaction levels (SERIALIZABLE, REPEATABLE READ, etc.).
- Multi-statement interactive transactions.
- Cross-kernel federation (see MRFC-0015).

---

# 3. Terminology

| Term | Meaning |
|------|---------|
| Snapshot | A point-in-time view anchored at an HLC value |
| Snapshot Isolation | A reader sees only data committed at or before its snapshot timestamp |
| OCC | Optimistic Concurrency Control via version guards |
| Idempotency Key | Client-supplied token that maps a retried call to the same result |
| Pipeline Mutex | The single kernel mutex serializing all commits |
| Index Lag | Maximum milliseconds between a committed KE and its index application |
| `index_lag_ms` | Per-result disclosure of potential secondary-index staleness |

---

# 4. Consistency Model

aikoql provides **causal+ total order** within a single kernel:

1. All successful commits form a single, gapless sequence of `seq` values.
2. Every KE carries a monotone HLC `commit_ts`.
3. If commit A finishes before commit B starts, then `A.commit_ts < B.commit_ts` and `A.seq < B.seq`.
4. Readers observe a consistent snapshot anchored at an HLC value.

---

# 5. Isolation Levels

The kernel supports exactly two isolation levels.

## 5.1 Read Committed (default)

1. A reader without an explicit `snapshot` SHALL see all state committed before the call begins.
2. The reader SHALL NOT observe uncommitted data.
3. The reader SHALL observe durable data only (commits are durable on return).

## 5.2 Snapshot Isolation

1. A reader MAY provide an explicit `snapshot` HLC value.
2. The reader SHALL see exactly the state that was committed at or before `snapshot`.
3. The reader SHALL NOT see any data committed after `snapshot`.
4. Snapshot reads SHALL NOT block concurrent writes and vice versa.
5. Snapshot reads are implemented by filtering keys and head records by `commit_ts <= snapshot`.

---

# 6. Single-Writer Pipeline

1. Exactly one commit MAY be in flight at any time, guarded by a single mutex.
2. No reader lock is required; reads operate against committed, immutable data.
3. Because writes are serialized, write skew anomalies are structurally impossible.
4. The pipeline SHALL reject concurrent writes by serialization, not by blocking forever.

---

# 7. Optimistic Concurrency Control

1. Every mutable operation on an existing KOID SHALL accept an `expected_version` guard.
2. The commit SHALL fail with `VERSION_CONFLICT` if the current head version differs from `expected_version`.
3. A create operation with `expected_version = 0` (or `koid = None`) SHALL fail with `VERSION_CONFLICT` if the KOID already exists.
4. OCC checks occur inside the single-writer mutex, so no race exists between check and write.

---

# 8. Idempotency

1. Clients MAY supply an `idempotency_key` on `remember`.
2. If `idem/<key>` exists, the kernel SHALL return the previously stored result without mutation.
3. If the key is absent, the commit proceeds and stores `idem/<key>` in the same atomic batch.
4. Idempotency keys SHOULD be unique per logical operation and scoped to the caller.
5. Idempotency records are durable and survive restart.

---

# 9. Index Consistency & Lag

1. Secondary indexes are maintained asynchronously by `IndexMaintainer`.
2. Indexes are NOT on the commit path and SHALL NOT block commits.
3. Every indexed search result SHALL expose `index_lag_ms`: the elapsed time between the latest committed KE and the maintainer's high-water mark at query time.
4. A caller MAY wait for `index_lag_ms == 0` (or below a threshold) before trusting recall-critical results.
5. `IndexMaintainer::wait_caught_up` SHALL block until all KEs at or before a given `seq` are reflected in all indexes.

---

# 10. Durability & Recovery

1. A successful commit return SHALL guarantee durability on the local storage engine.
2. After abrupt termination, the kernel SHALL recover to a state consistent with the last committed batch.
3. No partial commit SHALL be observable.
4. Snapshot isolation across restarts is preserved because HLC values are re-seeded from durable state (MRFC-0008 §6).

---

# 11. Concurrency Rules for Clients

1. Reads are concurrent and non-blocking.
2. Writes are serialized; clients SHOULD expect finite queueing latency under contention.
3. A `VERSION_CONFLICT` SHOULD be handled by re-reading and re-applying the intended change.
4. Retries SHOULD reuse the same `idempotency_key` to avoid duplicate effects.
5. Long-running snapshot reads SHOULD use an explicit `snapshot` value to avoid seeing interleaved writes.

---

# 12. Conformance Tests (implemented)

| Requirement | Test |
|---|---|
| HLC monotonicity across commits | `durability::d02` |
| Snapshot reads see old state | `durability::d02` (assert after evolve + reopen) |
| OCC version conflict | `kernel::tests::remember_version_conflict` |
| Create-once conflict | `kernel::tests::auto_koid_different_each_call` |
| Idempotency key dedup | `kernel::tests::idempotent_remember_same_key` |
| Idempotency survives reopen | `durability::d05` |
| Index lag disclosure | `indexes::i02`, `kernel::find_similar` |
| Caught-up wait drains lag | `indexes::i02` |

---

# 13. AI Implementation Checklist

The coding agent SHALL produce:

- [ ] Snapshot read support (`snapshot` parameter) for `get`/`find_similar`
- [ ] OCC version guard on `remember`
- [ ] Idempotency-key lookup and storage
- [ ] Single-writer mutex in commit pipeline
- [ ] `index_lag_ms` calculation and disclosure
- [ ] `wait_caught_up` maintainer primitive
- [ ] Tests for each isolation / consistency rule above

No behavior may be invented beyond this RFC. Ambiguities MUST be reported rather than assumed.

---

# 14. Acceptance Criteria

- Snapshot reads return the state as of the given HLC.
- Concurrent reads never block writes.
- Version conflicts are returned, not silently overwritten.
- Idempotent retries return identical results without side effects.
- Index results disclose lag; callers can wait for zero lag.
- Recovered kernels preserve the causal order and HLC monotonicity of all previous commits.

---

# 15. Future RFC Dependencies

- MRFC-0011 Knowledge Syscall ABI (exposes snapshot and idempotency to clients)
- MRFC-0015 Federated `notify` (cross-kernel causal ordering)
- MRFC-0016 Memory Evals (quantifies recall/staleness under lag semantics)
