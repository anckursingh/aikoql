# Phase 1 — Milestone 1: Deterministic Kernel Core (COMPLETE)

**Status:** ✅ SHIPPED — all gates green  
**Date:** 2026-08-02  
**Current-state addendum:** 2026-08-04 — directory layout refactored to HLD submodules, `Kernel` slimmed to orchestrator, SHA-256/HMAC-SHA256 signatures landed, crash-fuzz gate added, root `.gitignore` consolidated. See §6 addendum for current counts and architecture.

---

# 1. What was built

The deterministic heart of the Knowledge Kernel — the component every later
phase depends on, and the piece the "non-absorbable moat" (provenance in the
write path) is made of.

| Module | File | Contents |
|---|---|---|
| `knowledge::kom` | `crates/kernel/src/knowledge/kom.rs` | MRFC-0001 canonical types: `KOID` (48-bit ms \| 32-bit counter \| 48-bit salt, time-ordered), `KnowledgeObject` (all 9 canonical blocks), `KnowledgeEvent`, lifecycle state machine, ACL types, MRFC-0001+0011 error model, FNV-1a-64, SHA-256 helpers |
| `knowledge::codec` | `crates/kernel/src/knowledge/codec.rs` | Deterministic canonical binary codec: strict round-trip, canonical encoding (BTreeMap ordering), truncation/trailing rejection, extension preservation (MRFC-0001 req 9) |
| `storage::store` | `crates/kernel/src/storage/store.rs` | `StorageEngine` trait (get / ordered prefix scan / **atomic** write_batch) + `MemoryEngine` reference backend |
| `transaction::kernel` | `crates/kernel/src/transaction/kernel.rs` | Single-writer commit pipeline, MVCC, OCC, HLC, and all 9 Class A syscalls |
| conformance | `crates/kernel/tests/conformance.rs` | Acceptance suite (MRFC-0011 §11) |

**Syscalls shipped (MRFC-0011 §6):** `remember`, `forget` (Tombstone + GDPR-class Erase), `evolve`, `verify`, `find_similar` (exact hybrid v0: cosine + token-Jaccard + property filters + RRF/weighted fusion), `trace`, `explain`, `prove` (hash-chained audit), `notify` (in-process CDC).

**Key architecture decisions (ADRs in brief):**
1. **std-only kernel.** Zero supply-chain in the commit domain; full determinism (injectable `Clock`, seedable `IdGen`) enabling byte-identical replay conformance. Wire format (prost/rkyv) arrives with MRFC-0005 without touching KS-ABI semantics.
2. **Single-writer pipeline + OCC.** One mutex serializes validate → OCC → HLC → atomic batch → ack. Correctness first; group-commit batching is a later optimization that must not change semantics.
3. **KEs are committed in the SAME atomic batch as the KO version** (plus journal head) — the commit pipeline is the single source of truth; there is no WAL/journal dual-write (review R3).
4. **MVCC by (koid, commit_ts) keys**; readers pin an HLC snapshot; HLC state is persisted in the journal head and re-seeded on open (monotone across restarts).
5. **ACL enforced inside the kernel**, never in adapters; `find_similar` ACL-filters silently (no existence leak).
6. **Erase keeps proof possible:** legal erasure removes payloads but retains the journal + a hash-only tombstone, so `prove` still verifies the chain (per-version hashes of erased payloads are unverifiable BY DESIGN; chain links still protect the journal).

---

# 2. Test results (implement → test loop)

```
cargo test --workspace
  unit tests (lib):        17 passed, 0 failed
  conformance suite:       26 passed, 0 failed
  doc tests:                0
  warnings:                 0 (clean build, stable toolchain)
```

Conformance coverage map (MRFC-0011 §11 → tests):

| Requirement | Tests |
|---|---|
| Deterministic replay (byte-identical journal) | t24 |
| Idempotent exactly-once commit | t06 |
| Lifecycle matrix — all 25 state pairs | t07, t08 |
| Provenance: trace / explain / prove | t14, t15, t16 |
| Tamper evidence (event + payload, incl. decode-breaking flips) | t17, t18 |
| OCC version conflicts (deterministic) | t02–t05 |
| ACL default-deny / allow / deny-precedence / roles / admin | t11, t12, t22 |
| MVCC snapshot stability under commits | t13 |
| Hybrid recall: vector order / text / type filter / RRF fusion / ACL silence | t19–t22 |
| CDC notify: ordering + filters | t23 |
| Tombstone + legal erasure with proof continuity | t09, t10 |
| Concurrency: unique KOIDs + gapless journal under 4×25 writers | t25 |
| Restart recovery: journal head + HLC continuity | t26 |
| Extension round-trip (MRFC-0001 req 9) | t01 |
| Codec strictness, canonicality, known vectors | 17 unit tests |

## Failures found & fixed during the loop (this is the process working)

1. **HLC restart collision (durability bug, found by t26):** a reopened kernel re-issued the same `commit_ts` and overwrote a prior version's payload key. Fix: journal head now persists `(seq, audit_hash, last_commit_ts)`; HLC re-seeds via `Hlc::starting_at` on open. *Lesson codified: timestamp state is part of the durable log.*
2. **Create-vs-update keyed off `koid.is_none()` (t04):** replaced with OCC-driven semantics — `RememberRequest::create()` is insert-guarded (`expected_version = Some(0)`); explicit-target update without a guard on a missing object is `NOT_FOUND`; `Some(0)` on an existing object is `VERSION_CONFLICT`.
3. **`prove` returned `Codec` error on decode-breaking tamper (t18):** now treats undecodable payloads as tamper evidence (`chain_valid = false`).
4. **RRF test data was symmetric (t21):** legitimate tie; data corrected so the intended ranking invariant is what is asserted.

---

# 3. Known limitations (honest capability ladder — Guardrail §3)

| Limitation | Why | Lands in |
|---|---|---|
| `MemoryEngine` is not durable | Inc-1 targets the deterministic core + conformance harness; `StorageEngine` trait is the seam | Inc-2 (`redb`/RocksDB backend, suite runs unchanged) |
| `find_similar` is exact O(N) scan | no ANN/BM25 indexes yet; scores are exact so results are *correct*, just not fast | Inc-2 (`usearch`/`hnsw_rs` + `tantivy` behind `VectorIndex` trait, async KE-maintained per MRFC-0009, real `index_lag`) |
| FNV-1a audit hash is non-cryptographic | std-only constraint; demonstrates chain semantics | Phase 4 (SHA-256 via `ring` + signed checkpoints) — **resolved in later refresh: SHA-256 audit hashing + HMAC-SHA256 version signatures now implemented** |
| `notify` is in-process, fan-out only | no durable CDC / resume tokens | Phase 2 (journal-shipped CDC) |
| No graph traversal operators | relationships stored + queryable; `Traverse` IR op pending | Phase 1 Inc-2 / Phase 2 — **resolved in later refresh: `GraphEngine` landed in `crates/engines/graph` with `relate` and `traverse`** |
| Sync API only | async facade (tokio) wraps without changing semantics | Inc-2 |

---

# 4. Gate review vs VISION-AND-STRATEGY Phase 1 exit criteria

| Criterion | Status |
|---|---|
| Conformance suite green | ✅ 26/26 (+ 17 unit) |
| Crash-recovery fuzz clean | ⏳ Inc-2 (needs durable backend to crash against; fault-injection harness scoped) — **resolved: crash fuzz gate `d04b` now green on redb** |
| P99 point read <10 ms on defined bench | ⏳ Inc-2 (benchmark corpus MRFC + criterion harness) — **resolved: P99 = 83.1 µs on redb** |
| Flagship agent demo with memory replay | ⏳ after Python SDK (Phase 1 Inc-3) — **resolved in Milestone 3 via MCP `m02`** |

**Verdict:** the kernel core gate passes; Phase 1 continues with Increment 2 rather than pausing — the remaining gates require the durable backend.

---

# 5. Next increments (queued, in order)

1. **Inc-2 — Durability & real recall:** `redb` backend behind `StorageEngine` (pure-Rust ACID, Windows-friendly); `VectorIndex` trait + `usearch`; `tantivy` text index; KE-driven async index maintenance with high-water marks (MRFC-0009 draft); crash-fault injection (`kill -9` loops); benchmark corpus MRFC + `criterion` P99 gate; tokio async facade. — **completed in Milestone 2/3**
2. **Inc-3 — Python SDK (PyO3) + MCP server** exposing the 9 Class A syscalls → flagship agent demo ("why did the agent know this?" via `explain`). — **completed in Milestone 3**
3. **MRFC drafting to ratify alongside Inc-2:** MRFC-0008 (Commit Pipeline & Journal), MRFC-0009 (Index Lifecycle), MRFC-0010 (Consistency & Isolation). — **drafts landed in Milestone 3**

---

# 6. Current-state addendum (2026-08-04)

The Milestone 1 modules have been restructured to match the HLD workspace layout
and the "knowledge microkernel" direction from `docs/knowledge-kernel-review.md`.
`Kernel` is now an orchestrator; the following services/managers live alongside it:

- **`KnowledgeRepository`** — `crates/kernel/src/storage/repository.rs`. Hides all
  key prefixes, journal encoding, and `WriteBatch` layout from the orchestrator.
- **`AuthManager`** — `crates/kernel/src/security/auth.rs`. In-memory role
  inheritance + per-type ACL policies loaded from persisted `mnemosyne:role` /
  `mnemosyne:policy` objects.
- **`SchemaRegistry`** — `crates/kernel/src/lifecycle/schema.rs`. In-memory type
  schemas; validation is called by the commit pipeline.
- **`IndexCoordinator`** — `crates/kernel/src/index/coordinator.rs`. Owns hybrid
  recall scoring and `find_similar` delegation; `IndexMaintainer` remains the
  KE-driven async index service.
- **`KnowledgeCache`** — `crates/kernel/src/storage/cache.rs`. Optional in-memory
  LRU for heads and object versions, disabled by default, invalidated on every
  repository write path.

- **`EventManager`** — `crates/kernel/src/event.rs`. Durable CDC subscriptions,
  live broadcast, replay/ack; extracted from the transaction orchestrator.
- **`KnowledgeContext`** — `transaction::kernel.rs`. Groups `subject`, `tenant`,
  `agent`, `reasoning_mode`, and `snapshot`; carried by `RememberRequest`,
  `TransactionOp`, `SimilarityQuery`, eval queries, and read syscalls instead of
  a long parameter list.

Additional later enhancements now reflected in the codebase:

- **SHA-256 audit hashing** for every `KnowledgeEvent`.
- **Optional HMAC-SHA256 at-rest version signatures** via `Kernel::with_signing_key`.
- **Crash-fuzz durability gate** `d04b`.
- **Root `.gitignore` consolidation** (stray `mnemosyne/.gitignore` merged up).

As of this refresh:

```
cargo test --workspace
  kernel unit tests:       41 passed
  conformance suite:       39 passed
  durability suite:         7 passed + 1 ignored P99 bench
  index acceptance:         8 passed
  eval acceptance:          3 passed
  fuzz codec suite:         5 passed
  proptest KOM suite:       6 passed
  mcp stdio suite:          7 passed
  graph engine tests:       7 passed
  ───────────────────────────────
  total active:           123 passed, 0 failed, 0 warnings
```

---

# 7. How to reproduce

```bash
cargo test --workspace          # current: 115 active Rust tests, all green
cargo build --workspace         # zero warnings
```

Repository: https://github.com/anckursingh/mnemosyne
