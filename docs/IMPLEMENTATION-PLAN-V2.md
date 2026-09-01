# AIKOQL Storage Engine v2 — Implementation Plan

Source: `docs/AIKOQL_Storage_Engine_V2_Production_Design.md` (2026-09-01). Branch `feature/sorage-engine`, commit per milestone, NO push (user pushes). TDD loop per milestone: PoV → RED → root-cause GREEN → regression → gates (`cargo fmt --all` + `cargo clippy --all-targets --all-features -- -D warnings`).

## Coder point of view (before implementation)

**Verdict: CONFIRM the architecture.** Segmented WAL → memtable → immutable segments → manifest → L0/L1 compaction is the proven shape for exactly the three limits the MVP certification published (unbounded WAL, full replay at open — 3.8 ms/MB / 8.77× peak, whole dataset in RAM). The doc's restraint is right twice over: don't clone RocksDB, and don't touch the kernel's `StorageEngine` semantics. The MVP engine stays the certified production default until v2 earns its own adoption gate.

**Refinements (senior challenges):**

1. **Version history is safe by key layout, not by special-casing.** AIKOQL's version rows are distinct logical keys `(koid, ts)` (KSE-12/15 evidence: 27-byte version-row keys, heads separate). A pure LSM merge (last-sequence-wins per *user key*) therefore preserves history **by construction** — an old version is never an overwrite of the same key. M5's risk collapses to one thing: retention policy (KEEP/DROP/ARCHIVE) for genuinely-obsolete rows (superseded heads, tombstones). Compaction stays key-space-generic; policy is an input, not an engine feature.
2. **Sequence = per-batch, not per-op.** The kernel commits one atomic batch per transaction (QA2-PROP-002). The commit coordinator assigns one monotonic sequence per batch; per-op sequences inside a batch are redundant. Simplifies WAL frames, memtable entries, and internal keys.
3. **P1 refinement: KO-awareness at the trait boundary is impossible and unnecessary.** `StorageEngine` sees opaque bytes. The real win is prefix-range locality (blocks grouped by key range), which captures AIKOQL's head/relo/reli/type layout on any key space — measured on AIKOQL's, implemented generically.
4. **Multi-process lock lands in M2** (the first milestone that writes): one OS lock file, `LockFileEx`/`flock`, ~20 lines, fail-closed. Prevents the two-writer catastrophe before the engine can write anything.
5. **Legacy migration lands in M3** (after bounded recovery works, never before): §23's WAL→segment builder is the direct path; the certified REC-002 redb snapshot remains the fixed-format fallback. Never delete the source WAL before semantic verification.
6. **Platform honesty:** Windows has no portable directory-fsync — CURRENT publication is write-temp → fsync file → rename (NTFS replace is atomic) → best-effort dir fsync, documented as such.

**Crate strategy:** new crate `crates/storage/aikoql-v2` (lib `aikoql_storage_v2`), depends on `aikoql-kernel` (checksums + later the `StorageEngine` trait), zero v1 production changes. v1 remains default; v2 opts in via `AIKOQL_BACKEND=aikoql-v2` once the engine lands (M2+), becomes default only after a v2 adoption matrix (the M7 discipline).

## Milestones

### SE2-M0 — Format contracts — done (19/19 green, clippy clean)

Deliver: `CURRENT` (magic `AKCV`, format_version u16, manifest_generation u64, sha256-8 over the preceding bytes — fixed 22-byte layout), `MANIFEST-{generation:06}` (magic `AKMV`, format_version, generation, segment records, wal records, trailing sha256-8), atomic publication (temp → fsync → rename), classification of the §20 corruption classes.

Acceptance (the doc's): golden byte-format tests; unknown version fails closed; manifest corruption detected; atomic CURRENT publication tested; existing StorageEngine semantics unchanged (v2 doesn't implement the trait yet — nothing to break; the kernel and v1 suites are the regression).

TDD REDs (M0): `current_golden` (hex fixture round-trip), `current_corrupt` (checksum mismatch → fail closed; unknown version → fail closed), `manifest_golden` (hex fixture round-trip), `manifest_corrupt` (checksum mismatch; generation mismatch vs CURRENT), `publication_atomic` (torn temp write never visible; crash between write and rename leaves the old CURRENT parseable — child-kill harness, KSE-15 pattern).

### SE2-M1 — Immutable segments

Deliver: segment writer/reader (header, 64 KiB-target data blocks, index block, bloom, footer; per-block magic/version/type/entry-count/compressed-size/uncompressed-size/checksum; prefix-compressed keys, value len, value, seq, flags PUT/DELETE/VERSION/TOMBSTONE). Published segments never modified — read handle only.

REDs: round-trip golden; random lookup; prefix scan; corrupted block fails closed; immutability (writer returns no mutable handle).

### SE2-M2 — Memtable and flush

Deliver: active memtable (BTreeMap, deterministic — the doc's own order), threshold → immutable → background flush → segment; WAL v2 frames (magic, format_version, frame type, batch sequence, payload len, payload, CRC); commit pipeline (assign seq → append WAL → durability boundary → apply memtable → ack — the KSE-13 120a order, ported); durability modes Sync/GroupCommit/Async (Sync default, no silent downgrade); OS file lock.

REDs: writes visible during flush; crash before publication recovers from WAL; crash after publication does not duplicate state (manifest generation dedup); memory threshold bounds the active memtable; Sync default + mode enforcement; second process fails closed on the lock.

### SE2-M3 — Bounded recovery

Deliver: open = CURRENT → manifest → validate referenced segments → open active WAL → replay only the active WAL. Legacy WAL migration (§23 path) + the §20 corruption-class policies (torn tail, complete corruption, missing segment → fail closed; orphan segment → ignored + reported; partial compaction output → never published, invisible by construction). v1 corruption tests ported to v2 frames (the KSE-082B resync classifier verbatim).

REDs: historical WAL not replayed after flush (the doc's Recovery Independence Test: 10 GB segments + 100 MB WAL → open cost ≈ active WAL, reported not asserted); only the active WAL replays; missing segment fails closed; orphan reported; migration never deletes source before semantic verification.

### SE2-M4 — Compaction

Deliver: L0 flush segments → background merge → L1 sorted segments. Pipeline: select inputs → write tmp → validate → fsync → publish manifest → release obsolete files after readers drain (`Arc<Segment>` handles). No deep levels until measurements justify.

REDs: logical state before == after (byte-exact across random workloads); crash injection at every publication stage (the §25 crash matrix, child-kill harness); readers continue during compaction; obsolete segments survive while referenced.

### SE2-M5 — Version-aware compaction

Deliver: retention policy interface (KEEP/DROP/ARCHIVE per key class) + the semantic-equivalence harness. Per refinement 1, history preservation is by key layout — the milestone proves it rather than implementing machinery.

REDs: the §25 Compaction Semantic Equivalence gate (create → update ×100 → supersede → relate/unrelate → temporal queries → compact repeatedly → all logical answers before == after); head preserved; tombstones retained until safe; supersede lineage preserved; kernel-level invariants (the KSE-12 battery) hold across compaction.

### SE2-M6 — Group commit

Deliver: commit queue, `max_batch_ops` / `max_batch_bytes` / `max_wait_duration`, one fsync per group, apply/ack the group. Sync mode remains the correctness baseline.

REDs: no acknowledged commit lost (crash at group boundaries); commit order deterministic (log order == apply order == ack order); Sync semantics unchanged (byte-exact parity with M2 pins); throughput improves under multi-writer load (measured, the KSE-120C matrix shape); strict opt-in for the perf variant.

### SE2-M7 — Cache / bloom filters

Deliver: segment metadata cache + bounded block cache + bloom filters; metrics (cache bytes, hit/miss rate, evictions). Cache must never affect correctness.

REDs: bounded cache (bytes enforced); cache-neutrality (random workloads with/without cache byte-equal); bloom false negatives forbidden (by construction + property test); measured random-read improvement (reported, not asserted).

## Performance acceptance (§26) — gates for the v2 adoption matrix

Recovery bounded by active WAL (M3 measurement); dataset larger than RAM queryable (M4+, measured against KSE-19's ceiling); memory limits configurable (M2); group commit improves concurrent throughput without weakening Sync (M6); KO lookup competitive with the MVP baseline (M7, the M7-W1..W8 harness re-run on v2). These are architecture gates measured when the milestone lands — not claims until then.

## Regression policy

v1 suites untouched; the kernel's `StorageEngine` contract is the compatibility surface. Once v2 implements the trait, the KSE-20 conformance battery (six asserts × backend) runs on it verbatim. v1 stays the certified default until v2 passes its own adoption matrix.
