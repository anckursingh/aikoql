# MRFC-0008: Commit Pipeline & Journal

- **Status:** Draft v1.0 (implemented behavior — codifies Phase 1 Inc-1/2)
- **Project:** Mnemosyne
- **Category:** Foundation / Storage
- **Depends on:** MRFC-0001 (KOM)
- **Supersedes:** None

> This RFC is **normative**. Keywords **MUST**, **SHALL**, **SHOULD**, **MAY**, and **MUST NOT** are interpreted as defined by RFC 2119.

---

# 1. Abstract

The commit pipeline is the single source of truth in Mnemosyne. This RFC defines the storage-engine abstraction, the atomic commit batch, the append-only journal, the hybrid logical clock (HLC), and the crash-recovery contract that makes every other subsystem deterministic and replayable.

---

# 2. Goals

- Provide a minimal, engine-agnostic storage contract (`StorageEngine`).
- Guarantee atomic, durable commits of a Knowledge Object version, its Knowledge Event, and the journal head.
- Define an append-only, hash-chained journal that survives abrupt termination.
- Make time monotone and recoverable across process restarts via the HLC.
- Enable deterministic replay of the entire commit history.

## Non-goals

- Distributed consensus or multi-node replication.
- Query-language syntax.
- Index construction algorithms (see MRFC-0009).
- Adapter protocols (see MRFC-0011).

---

# 3. Terminology

| Term | Meaning |
|------|---------|
| `StorageEngine` | The trait abstracting all durable state access |
| `WriteBatch` | An atomic all-or-nothing set of puts and deletes |
| Journal | The append-only sequence of `KnowledgeEvent`s |
| HLC | Hybrid Logical Clock: packed `(millis << 16) \| counter` |
| Pipeline | The single-writer commit machinery (seq, audit hash) |
| `K_JOURNAL` | The well-known key storing `(seq, audit_hash, last_ts)` |

---

# 4. Storage Engine Contract

```rust
pub trait StorageEngine: Send + Sync {
    fn get(&self, key: &[u8]) -> KResult<Option<Vec<u8>>>;
    fn scan(&self, prefix: &[u8]) -> KResult<Vec<(Vec<u8>, Vec<u8>)>>;
    fn write_batch(&self, batch: &WriteBatch) -> KResult<()>;
}
```

## 4.1 Requirements

1. `get` SHALL return the latest committed value for `key`, or `None` if absent.
2. `scan(prefix)` SHALL return all entries whose keys start with `prefix`, sorted ascending by key.
3. `write_batch` SHALL apply all puts and deletes atomically: either the entire batch is visible after success, or none of it is.
4. `write_batch` commits SHALL be durable: a successful return SHALL mean the batch survives abrupt process termination of the calling process.
5. Implementations SHALL be `Send + Sync` and safe for concurrent readers.
6. Implementations SHALL NOT expose partial writes, torn pages, or reordering across batches.

## 4.2 Reference Backends

- `MemoryEngine`: deterministic, in-memory reference implementation for conformance testing. NOT durable.
- `RedbEngine`: durable, pure-Rust ACID backend over `redb`. Maps `write_batch` to one redb write transaction.

---

# 5. Key Layout

| Prefix | Contents |
|--------|----------|
| `ko/` | Versioned Knowledge Object payloads, keyed by `koid \|\| commit_ts` |
| `head/` | Current head record per KOID: `(version, commit_ts, lifecycle_state)` |
| `ke/` | Knowledge Events, keyed by `seq` |
| `tomb/` | Erasure tombstone stubs per KOID |
| `idem/` | Idempotency-key to `(koid, version, commit_ts)` mappings |
| `meta/journal` | Journal head: `(seq, audit_hash, last_ts)` |

Keys SHALL be lexicographically ordered so that prefix scans yield semantically useful sequences (e.g., all versions of a KOID in commit-ts order).

---

# 6. Hybrid Logical Clock (HLC)

The HLC produces commit timestamps that are:
- monotone non-decreasing within a process,
- monotone across process restarts when re-seeded from the persisted `last_ts`,
- dense enough to support high commit rates via a 16-bit counter.

```
timestamp = (wall_millis << 16) | counter
```

1. On each commit, the kernel SHALL choose `max(now, last_millis) << 16` and increment the counter if `now == last_millis`.
2. On `Kernel::open`, the HLC SHALL be re-seeded from `last_ts` stored in `meta/journal`.
3. `snapshot()` SHALL return the current HLC value and MAY be used by callers as a snapshot-isolation anchor.

---

# 7. Commit Pipeline

The pipeline is single-writer: one mutex serializes validation → OCC check → HLC assignment → atomic batch write.

## 7.1 Single-Writer Invariants

1. Exactly one commit MAY be in flight at any time.
2. The pipeline state consists of the journal `seq` and the latest `audit_hash`.
3. Every commit SHALL produce exactly one `KnowledgeEvent` with `seq = previous_seq + 1`.
4. Every commit SHALL write the updated `meta/journal` key in the same `write_batch` as the KO version and KE.

## 7.2 Atomic Commit Batch

A successful commit SHALL write the following in one `write_batch`:
- the KO payload at `ko/<koid>/<commit_ts>`,
- the head record at `head/<koid>`,
- the KE at `ke/<seq>`,
- the updated journal head at `meta/journal`,
- optionally, idempotency and tombstone records.

No commit SHALL be considered successful until `write_batch` returns `Ok(())`.

## 7.3 Audit Hash Chain

Each KE carries:
- `prev_audit_hash`: the audit hash of the previous KE (0 for seq 1),
- `audit_hash`: `fnv1a64(prev_audit_hash, seq, koid, version, kind, commit_ts, payload_hash, actor, note)`.

The journal head stores the audit hash of the latest KE. This chain SHALL be verifiable by the `prove` syscall (MRFC-0011 §6.7).

---

# 8. Crash Recovery

1. On `Kernel::open`, the kernel SHALL read `meta/journal` to recover `(seq, audit, last_ts)`.
2. If `meta/journal` is absent, the kernel SHALL initialize `(0, 0, 0)`.
3. The HLC SHALL be re-seeded from `last_ts`, preserving monotonicity across restarts.
4. Because every commit is one atomic batch, the store is always in one of two states after a crash:
   - the batch was committed: journal head, KE, and KO are all present;
   - the batch was not committed: none of them are present.
5. There SHALL be no state requiring bespoke repair beyond replaying the journal from seq 1.

---

# 9. Optimistic Concurrency Control

1. `remember` for an existing KOID SHALL accept an `expected_version` guard.
2. If `expected_version` does not match the current head version, the commit SHALL fail with `VERSION_CONFLICT`.
3. A create operation SHALL specify `expected_version = 0` (or `None` with `koid = None`) and SHALL conflict if the KOID already exists.
4. OCC is enforced inside the single-writer mutex, so races are impossible.

---

# 10. Idempotency

1. `remember` MAY carry an `idempotency_key`.
2. Before committing, the kernel SHALL check `idem/<key>`.
3. If a previous result is found, the kernel SHALL return it verbatim without mutating state.
4. On successful commit, the kernel SHALL write `idem/<key>` mapping to the resulting `(koid, version, commit_ts)` in the same batch.

---

# 11. Conformance Tests (implemented)

| Requirement | Test |
|---|---|
| Atomic batch application | `store::tests::batch_is_atomic_and_ordered` |
| Sorted prefix scan | `store::tests::scan_is_prefix_limited_and_sorted` |
| Commits survive restart | `durability::d01` |
| Journal seq and HLC continue after reopen | `durability::d02` |
| Atomic batches on disk (redb) | `durability::d03` |
| Abrupt termination preserves commits | `durability::d04` |
| Syscall surface identical on durable engine | `durability::d05` |
| redb persistence across reopen | `store_redb::tests::persists_across_reopen` |
| redb sorted prefix scan | `store_redb::tests::scan_is_sorted_and_prefix_limited` |

---

# 12. AI Implementation Checklist

The coding agent SHALL produce:

- [ ] `StorageEngine` trait and `WriteBatch`
- [ ] `MemoryEngine` reference backend
- [ ] At least one durable backend (`RedbEngine`)
- [ ] HLC implementation with restart re-seeding
- [ ] Single-writer commit pipeline
- [ ] Atomic commit batch encoding
- [ ] Audit hash chain
- [ ] Idempotency-key handling
- [ ] Durability / crash-recovery tests

No behavior may be invented beyond this RFC. Ambiguities MUST be reported rather than assumed.

---

# 13. Acceptance Criteria

- All storage-engine contract tests pass against every backend.
- Committed writes survive abrupt termination without repair.
- Journal sequence and audit chain are gapless across restarts.
- HLC timestamps are strictly monotone across restarts.
- `write_batch` is atomic and observable ordering matches commit order.

---

# 14. Future RFC Dependencies

- MRFC-0009 Secondary Index Lifecycle (replays the journal defined here)
- MRFC-0010 Consistency & Isolation Levels (defines read semantics over this pipeline)
- MRFC-0011 Knowledge Syscall ABI (calls this pipeline)
- MRFC-0015 Federated `notify` (cross-kernel journal exchange)
