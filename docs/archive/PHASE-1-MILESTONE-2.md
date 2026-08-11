# Phase 1 — Milestone 2: Durable Storage & Crash Recovery (COMPLETE)

**Status:** ✅ SHIPPED — all gates green  
**Date:** 2026-08-02  
**Current-state addendum:** 2026-08-04 — directory layout refactored to HLD submodules, SHA-256/HMAC-SHA256 at-rest signatures landed, crash-fuzz gate added, root `.gitignore` consolidated. See §5 addendum for current counts.

---

# 1. What was built

The kernel is now **durable**. Committed knowledge survives restarts and abrupt
termination (kill -9 / power-loss at the commit boundary) — the first
production-grade promise of the system.

| Artifact | Contents |
|---|---|
| `crates/kernel/src/storage/store_redb.rs` | `RedbEngine`: durable `StorageEngine` over redb (pure-Rust ACID KV, no C++ FFI, Windows-friendly). `write_batch` → one redb write transaction: atomic + fsync'd at commit. `scan` → ordered B-tree range. |
| `crates/kernel/examples/crash_writer.rs` | Crash-fault injector: commits N objects with known KOIDs, then `std::process::exit(0)` — no destructors, simulating power-loss right after the commit boundary |
| `crates/kernel/tests/durability.rs` | Durability acceptance suite (incl. P99 gate) |
| `crates/kernel/Cargo.toml` | `redb = "2"` — sits BEHIND the `StorageEngine` trait; zero leakage into KS-ABI types |

**Design decision (ADR):** redb over RocksDB for the first durable backend —
pure Rust (no bindgen/MSVC toolchain risk on Windows), ACID with atomic commit
+ fsync, single-file embedded — exactly matching the embedded-first wedge
strategy. The trait seam (Milestone 1) made this a zero-kernel-change swap;
RocksDB remains an option behind the same trait if profiling ever demands it.

---

# 2. Test results (implement → test loop)

```
cargo test --workspace
  unit tests (lib):        20 passed  (17 core + 3 redb)
  conformance suite:       26 passed  (unchanged — MemoryEngine reference)
  durability suite:         6 passed + 1 bench (ignored by default)
  warnings:                 0

cargo test -p aikoql-kernel --test durability -- --ignored d07
  BENCH point-read n=500  p50=44.4µs  p99=83.1µs  (engine=redb, dataset=500 KOs)
```

**P99 gate: 83.1µs vs 10ms threshold → 120× headroom.** ✅

Durability suite coverage:

| Gate | Test | Evidence |
|---|---|---|
| Committed mutations survive clean restart | d01 | v2 object + 2-event journal + valid proof after reopen |
| Journal seq + audit + HLC continuity across reopen | d02 | seq continues; `commit_ts` strictly monotone post-restart (HLC re-seed from persisted journal head) |
| Write batches all-or-nothing on disk | d03 | 100-key atomic batch; 50-del+1-put atomic batch |
| **Abrupt termination (no destructors) preserves commits** | d04 | subprocess `crash_writer` → `process::exit(0)`; reopen: all 7 objects present, journal head seq=7, `prove` chain valid |
| Syscall surface engine-agnostic | d05 | full remember→evolve→update→find_similar(RRF)→trace→forget→prove flow identical on redb |
| Concurrent writers, gapless journal on disk | d06 | 2×25 threads → 50 events, seq 1..50 gapless |
| P99 point read <10 ms (NFR) | d07 | **83.1µs** on 500-KO dataset |

## Failures found & fixed during the loop

1. **redb 2.6 `range` generics drift (compile error):** `Table::range` takes one generic parameter in redb 2.6, not two as in older docs — fixed call site. *Lesson: verify FFI/API assumptions against the locked crate version, not memory.*
2. **`ReadableTable` import unused in redb 2.6:** table methods are inherent in 2.6 — removed; zero-warning policy holds.

---

# 3. Gate review vs VISION-AND-STRATEGY Phase 1 exit criteria

| Criterion | Status |
|---|---|
| Conformance suite green | ✅ 26/26 + 20 unit + 6 durability |
| Crash-recovery clean | ✅ abrupt-termination subprocess test (d04); committed data + audit chain survive |
| P99 point read <10 ms | ✅ **83.1µs** (120× headroom) on redb |
| Flagship agent demo with memory replay | ⏳ needs Python SDK (Inc-3) |

**Verdict:** 3 of 4 Phase-1 gates pass. The last (demo) requires the SDK — Inc-3.

---

# 4. Known limitations (unchanged honesty policy)

| Limitation | Lands in |
|---|---|
| `find_similar` still exact O(N) scan (correct, not fast) | Inc-3: `usearch`/`tantivy` behind `VectorIndex`/`TextIndex` traits, KE-driven async maintenance (MRFC-0009), real `index_lag` reporting |
| FNV-1a audit hash non-cryptographic | Phase 4 (SHA-256 + signed checkpoints) — **resolved in Milestone 3 refresh: SHA-256 audit hashing + HMAC-SHA256 version signatures are now implemented** |
| `notify` in-process only | Phase 2 (durable CDC from the on-disk journal — now unblocked) |
| Sync API | Inc-3 (tokio facade) |
| Crash testing covers commit-boundary crash; mid-commit power-loss + corruption-injection soak | Phase 4 (deterministic simulation, `madsim`) |

---

# 5. Current-state addendum (2026-08-04)

After the Milestone 3 index/MCP work, the kernel was further refactored to match
the HLD workspace layout and the "knowledge microkernel" direction from
`docs/knowledge-kernel-review.md`:

- **Directory structure aligned with HLD:** modules now live under
  `crates/kernel/src/{knowledge,storage,transaction,security,lifecycle,index}`.
- **`KnowledgeRepository`** (`storage/repository.rs`) hides all key prefixes,
  encodings, and `WriteBatch` details from the transaction orchestrator.
- **`AuthManager`** extracted to `security/auth.rs` — owns role inheritance and
  per-type ACL policies loaded from persisted `aikoql:role` /
  `aikoql:policy` objects.
- **`SchemaRegistry`** extracted to `lifecycle/schema.rs` — owns in-memory type
  schemas and validation.
- **`IndexCoordinator`** extracted to `index/coordinator.rs` — owns hybrid recall
  scoring and delegates `find_similar`; `IndexMaintainer` remains the KE-driven
  async index service.
- **Optional `KnowledgeCache`** added in `storage/cache.rs` — in-memory LRU for
  heads and object versions, disabled by default, kept coherent by invalidation
  on every repository write path.
- **At-rest HMAC-SHA256 version signatures** and **SHA-256 audit-chain hashing**
  are now implemented (`Kernel::with_signing_key`, `prove` signature verification).
- **Crash-fuzz gate `d04b`** added to the durability suite.
- **Stray `.gitignore` files** consolidated into a single root `.gitignore`.

- **`EventManager`** extracted to `event.rs` — durable CDC subscriptions,
  live broadcast, replay/ack; removed from the transaction orchestrator.
- **`KnowledgeContext`** introduced in `transaction::kernel.rs` — groups
  `subject`, `tenant`, `agent`, `reasoning_mode`, and `snapshot`; carried by
  `RememberRequest`, `TransactionOp`, `SimilarityQuery`, eval queries, and read
  syscalls instead of many parameters.

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
  mcp stdio suite:          6 passed
  ───────────────────────────────
  total active:           115 passed, 0 failed, 0 warnings
```

---

# 6. How to reproduce

```bash
cargo test --workspace                                              # current: 115 active tests, all green
cargo test -p aikoql-kernel --test durability -- --ignored d07   # P99 bench gate
cargo build --workspace                                             # zero warnings
```

Repository: https://github.com/anckursingh/aikoql
