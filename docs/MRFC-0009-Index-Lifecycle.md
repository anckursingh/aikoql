# MRFC-0009: Secondary Index Lifecycle

- **Status:** Draft v1.0 (implemented behavior — codifies Phase 1 Inc-3)
- **Project:** Mnemosyne
- **Category:** Foundation / Storage
- **Depends on:** MRFC-0001 (KOM), MRFC-0008 (Commit Pipeline & Journal), MRFC-0011 (KS-ABI)
- **Supersedes:** None

> This RFC is **normative**. Keywords **MUST**, **SHALL**, **SHOULD**, **MAY**, and **MUST NOT** are interpreted as defined by RFC 2119.

---

# 1. Abstract

Secondary indexes (vector, text, graph-adjacency, future projections) accelerate recall but own no truth. This RFC defines their lifecycle: how indexes are built, maintained, consulted, lagged, rebuilt, and retired — uniformly from the Knowledge Event stream. It makes three hard problems (crash-consistency, freshness semantics, algorithm evolution) into one well-understood one.

---

# 2. Normative Requirements

1. Indexes are **secondary structures**. The commit pipeline (MRFC-0008) is the single source of truth; indexes MUST NOT be written on the commit path.
2. Index maintenance SHALL be **driven exclusively by the Knowledge Event stream**: catch-up replay from seq 0 on startup, then live application in commit order.
3. Every index (or index set under one maintainer) SHALL track a **high-water mark**: the greatest journal seq fully applied.
4. Queries consulting an index SHALL disclose `index_lag = journal_head_seq − high_water` to the caller.
5. Index scoring semantics for a given fusion mode MUST be identical to the exact (index-free) path; parity is conformance-tested. ANN algorithms MAY approximate ranking but MUST document recall bounds.
6. Index algorithms SHALL be pluggable behind stable traits (`VectorIndex`, `TextIndex`). A new algorithm MUST pass the same conformance suite against the exact-path oracle.
7. Rebuilding an index SHALL be equivalent to replaying the journal from seq 0 — always possible, never requiring bespoke repair.
8. `Forgotten` events and `Deleted`-state objects SHALL remove the document from every index.
9. Vector entries SHALL be namespaced by `embedding_model`; a model migration = build-new-index in background → dual-read with fusion → cutover → drop old (procedure in §6).
10. Index state is **derived and disposable**: it MUST be safe to delete any index at any time and rebuild from the journal.

---

# 3. Architecture (as implemented in Phase 1 Inc-3)

```
commit pipeline ──KE──> IndexMaintainer
                          ├─ catch-up replay (journal seq 0..head)
                          ├─ live apply (notify stream, commit order)
                          ├─ high-water mark (last applied seq)
                          ├─ VectorIndex (trait) ─ BruteForce (oracle) → HNSW (Inc-4)
                          └─ TextIndex (trait) ─ TokenInverted (oracle) → BM25/tantivy (Inc-4)

find_similar ──routes──> attached indexes (lag disclosed per result)
           └─fallback──> exact inline scan (no maintainer attached)
```

- Catch-up replay happens synchronously at maintainer start; live events apply via the `notify` CDC stream on a dedicated thread.
- `find_similar` routes through attached indexes with parity scoring; without a maintainer it computes exact inline scores over committed state.

---

# 4. Freshness Semantics

- **Default:** read-your-writes is NOT guaranteed through indexes; freshness is disclosed via `index_lag`.
- Callers requiring strict freshness SHOULD compare `index_lag` against a tolerance and MAY return `INDEX_LAG_EXCEEDED` policy-side (adapter concern).
- The exact path (no index) is always fully fresh; adapters MAY force it per query (future `fusion=exact` hint).

---

# 5. Crash-Consistency

- Indexes are rebuilt from the journal after any crash; a maintainer MUST NOT persist derived state it cannot reconstruct (in-memory indexes satisfy this trivially; disk-backed indexes MUST checkpoint `(index_files, high_water)` atomically).

---

# 6. Embedding-Model Migration Procedure

1. Register vectors under the new `embedding_model` (namespace separation).
2. Build the new-model index in the background from the journal.
3. Dual-read: query both indexes, fuse rankings (RRF).
4. Cut over when the new index's high-water equals the journal head.
5. Drop the old index; its namespace MAY be garbage-collected lazily.

---

# 7. Conformance Tests (implemented)

| Requirement | Test |
|---|---|
| Catch-up replay builds indexes from journal | `indexes.rs i01` |
| Live commits applied; lag returns to 0 | `indexes.rs i02` |
| Parity: indexed recall == exact path (order + scores) | `indexes.rs i03` |
| `forget` removes from indexed recall | `indexes.rs i04` |
| Rebuild from journal after restart | `indexes.rs i05` |
| Trait-level order/remove semantics | unit tests in `index.rs` |

Future ANN/BM25 implementations MUST pass i03's parity gate within documented recall bounds.

---

# 8. Future Work

- HNSW `VectorIndex` (usearch/hnsw_rs) with recall-at-k bounds in docs.
- BM25 `TextIndex` (tantivy) with disk-backed checkpoints.
- Graph-adjacency index for traversal pushdown.
- `fusion=exact` query hint; per-tenant index partitioning.