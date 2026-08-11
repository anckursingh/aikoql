# Phase 1 — Milestone 3: Indexes, Async Facade, MCP Server & Kernel Refactor (COMPLETE)

**Status:** ✅ SHIPPED — **all four Phase-1 gates now pass**.  
**Date:** 2026-08-03 (architectural refresh completed 2026-08-04)  
**Implements:** MRFC-0009 (Index Lifecycle, draft) + MRFC-0011 Class A syscalls + agent-facing MCP surface + HLD-aligned kernel refactor  
**Crates:** `aikoql-kernel` v0.1.0, `aikoql-mcp` v0.1.0  
**New deps:** `tokio 1` (async facade), `serde_json 1` (MCP protocol), `hnsw = { package = "fast-hnsw" }` (ANN vector index), `tantivy 0.22` (BM25 text index), `pyo3 0.22` with `abi3-py39` (Python SDK), `sha2 0.10` + `hmac 0.12` (at-rest signatures)

---

# 1. What was built

The product's agent-facing surface is live: any MCP-compatible agent (Claude, Cline, Cursor, LangGraph via MCP adapters) can now drive the Knowledge Kernel directly — and the flagship question *"why did the agent know this?"* is answerable through one protocol call.

In addition, the kernel was refactored to match the HLD workspace layout and the "knowledge microkernel" direction from `docs/knowledge-kernel-review.md`. `Kernel` is now a thin orchestrator; heavy responsibilities live in dedicated managers/services behind stable internal boundaries.

| Artifact | File | Contents |
|---|---|---|
| `knowledge::kom` | `crates/kernel/src/knowledge/kom.rs` | MRFC-0001 canonical types: `KOID`, `KnowledgeObject`, `KnowledgeEvent`, lifecycle state machine, ACL types, SHA-256 + HMAC-SHA256 helpers |
| `knowledge::codec` | `crates/kernel/src/knowledge/codec.rs` | Deterministic canonical binary codec; canonical encoding, truncation/trailing rejection |
| `storage` | `crates/kernel/src/storage/mod.rs` | `StorageEngine` trait + `MemoryEngine`/`RedbEngine` + `KnowledgeRepository` (hides key layout) + optional `KnowledgeCache` |
| `transaction::kernel` | `crates/kernel/src/transaction/kernel.rs` | Single-writer commit pipeline (MVCC, OCC, HLC), KS-ABI Class A syscalls, journal/audit chain, `KnowledgeContext`, orchestrator only |
| `event` | `crates/kernel/src/event.rs` | `EventManager`: durable CDC subscriptions, live broadcast, replay/ack |
| `security::auth` | `crates/kernel/src/security/auth.rs` | **`AuthManager`**: in-memory role-inheritance graph + per-type policies loaded from persisted `aikoql:role` / `aikoql:policy` KOs |
| `lifecycle::schema` | `crates/kernel/src/lifecycle/schema.rs` | **`SchemaRegistry`**: in-memory type schemas; object validation |
| `index` | `crates/kernel/src/index.rs` + `index/coordinator.rs` | `VectorIndex` / `TextIndex` traits; exact oracles; **`IndexMaintainer`** (KE-driven async maintenance, catch-up, live apply, high-water mark, checkpoint/resume); **`IndexCoordinator`** (owns hybrid recall scoring + `find_similar` delegation); `HnswVectorIndex` / `TantivyTextIndex` |
| `async_kernel` | `crates/kernel/src/async_kernel.rs` | `AsyncKernel` tokio facade — identical semantics via `spawn_blocking` |
| `mcp` | `crates/services/api/mcp/` | `aikoql-mcp` server: stdio MCP, 12+ Class A syscall + eval tools, durable CDC notifications |
| `python` | `crates/sdk/python/` | `aikoql-py` PyO3 extension + pure-Python package + LangGraph/CrewAI adapters |
| `graph` | `crates/engines/graph/` | `GraphEngine`: stateless relationship edge mutation (`relate`) and traversal (`traverse`) through the public `Kernel` API |

**Security / audit hardening:**
- **SHA-256 audit-chain hashing** for every `KnowledgeEvent` (`payload_hash` + `audit_hash`).
- **Optional at-rest HMAC-SHA256 version signatures** via `Kernel::with_signing_key`; `prove` verifies every signature when a key is configured (`t18b`, `t18c`).
- **Crash-fuzz durability gate** `d04b` exercises random commit-boundary termination.

**Design notes (ADRs):**
1. **Indexes own no truth.** They are KE-maintained secondary structures, never on the commit path (Determinism Law); rebuild = replay journal from seq 0, always. High-water mark + `index_lag` disclosure replaces silent staleness.
2. **Parity before speed.** Exact brute-force oracles behind the traits and conformance-gated indexed recall (score-identical to the exact path). HNSW/BM25 swap in behind the same traits.
3. **MCP-first, SDK-second.** The protocol server is the agent-facing wedge; the Python SDK binds the same kernel in-process.
4. **Kernel-first, services-around.** Auth, schema, index scoring, and storage layout are now outside the orchestrator. External engines (graph, reasoning, etc.) route through the kernel, never to storage directly.
5. **Repository hides keys.** `KnowledgeRepository` owns all key prefixes and encodings; `Kernel` reasons in KOM types.
6. **Cache is optional and coherent.** `KnowledgeCache` caches heads and object versions; it is invalidated on every repository write path and disabled by default.
7. **Context is a first-class syscall parameter.** `KnowledgeContext` groups `subject`, `tenant`, `agent`, `reasoning_mode`, and `snapshot`; request types (`RememberRequest`, `TransactionOp`, `SimilarityQuery`, eval queries) and read methods carry it instead of a long parameter list.

---

# 2. Test results (implement → test loop)

```
cargo test --workspace
  kernel unit tests:       41 passed  (knowledge codec/KOM + storage + transaction + cache + index + eval)
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

`cargo test -p aikoql-kernel --test durability -- --ignored d07` reports **P99 = 83.1 µs** on the 500-KO redb dataset (120× headroom vs 10 ms).

Key acceptance evidence:

| Gate | Test | Evidence |
|---|---|---|
| Catch-up replay builds indexes from journal | `indexes i01` | water=2, both indexes populated before attach |
| Live commits applied; lag → 0 | `indexes i02` | `wait_caught_up` then `index_lag_ms == 0` on results |
| **Parity: indexed == exact scoring** | `indexes i03` | identical order + \|Δscore\| < 1e-6 across RRF fusion |
| forget removes from indexed recall | `indexes i04` | tombstone → indexes empty, recall empty |
| Restart rebuild from journal | `indexes i05` | fresh maintainer rebuilds solely from KEs |
| **HNSW recall parity with exact path** | `indexes i06` | top-k overlap >= k-1, score \|Δ\| < 1e-3 vs exact RRF fusion |
| **Tantivy/BM25 recall parity with exact path** | `indexes i07` | top-k overlap >= k-1 vs exact RRF fusion |
| **Checkpoint resume skips replay and stays live** | `indexes i08` | HNSW + Tantivy checkpointed, loaded, `start_at(water)` resumes; live commit advances water |
| Python SDK: remember/get + find_similar + forget | `python test_sdk` | PyO3 `aikoql` round-trips typed properties, text recall, tombstones against durable redb store |
| LangGraph native checkpoint saver | `python test_adapters` | `AikoqlLangGraphSaver` `put`/`get`/`list` round-trip checkpoints by `thread_id`; async wrappers work |
| CrewAI memory adapter | `python test_adapters` | `AikoqlCrewAIMemory` saves role-scoped memories, searches by text, resets by tombstoning |
| Legacy checkpointer alias | `python test_adapters` | `from aikoql.checkpointer import AikoqlCheckpointer` still works |
| MCP handshake + tool surface | `mcp m01` | protocolVersion 2024-11-05; all 12 tools listed |
| **Flagship: "why did the agent know this?"** | `mcp m02` | agent commits claim w/ provenance → evolve to verified → `explain` returns source/confidence/verified/lineage; `prove` chain valid; state + proof survive MCP server restart |
| ACL enforced through protocol | `mcp m03` | non-owner `get` → `ACCESS_DENIED` via tools/call error |
| **Cross-agent ACL policy + role inheritance over MCP** | `mcp m05` | admin commits `aikoql:role` (junior→senior) + `aikoql:policy` (senior reads `shared_note`); bob reads alice's note; carol denied |
| **Durable CDC over MCP** | `mcp m04` | `notifications/subscribe` receives live `Created` events; `notifications/ack` advances watermark; server restart replays un-acked events |
| **Memory Evals over MCP** | `mcp m06` | `eval_recall` hits expected set; `eval_staleness` reports lag; `eval_contradictions` finds conflicting property on similar claims |
| Durable subscription registry | `kernel::durable_subscription_*` | persisted `sub/<id>` records survive `Kernel` reopen; replay respects `last_seq` |
| Cross-agent ACLs (unit) | `kernel::cross_agent_*` + `kernel::policy_*` | role inheritance expands ACL and policy principals; deny overrides allow |
| At-rest signatures | `t18b`, `t18c` | signed commits verify; tampered signatures detected |
| Crash fuzz | `d04b` | random commit-boundary `crash_writer` runs produce no data loss or journal gaps |

## Failures found & fixed during the loop

1. **f32→JSON precision (m02):** stored confidence `0.99f32` serializes as `0.9900000095…`; assertion switched to tolerance comparison. *Codified: wire-level float comparisons always use epsilon.*
2. **Invalid hex literal (`0x1D4X`)** in the indexes harness — caught pre-run by inspection.
3. **Workspace member ordering:** `crates/mcp` was referenced before it existed — build error taught the sequencing; manifest+server landed before compiling.
4. **HNSW crate choice (loop #1):** first candidate (`hnswlib-rs`) depends on `off64` which is Unix-only and failed on Windows. Switched to pure-Rust `fast-hnsw` (renamed dep `hnsw`).
5. **Index `search(k=usize::MAX)` overflow:** `find_similar` asks indexes for "all" results with `usize::MAX`. Both HNSW and Tantivy collectors overflowed. Fixed by capping the internal query limit to capacity or index length.
6. **Tantivy checkpoint cannot reconstruct tokens:** the original `tokens` field was indexed but not stored, so a loaded checkpoint could not rebuild the in-memory doc map. Fixed by making `tokens` `TEXT \| STORED` and keeping a `BTreeMap<KOID, BTreeSet<String>>` mirror for deterministic re-index on checkpoint.
7. **HNSW `LabeledIndex::<'_>::load` lifetime mismatch:** `fast-hnsw` 1.0.1 defines `LabeledIndex<D, L>` with no lifetime parameters; the attempted explicit lifetime caused E0107. Fixed by calling `LabeledIndex::load` without lifetime annotation.
8. **PyO3 0.22 + Python 3.14 ABI mismatch:** PyO3 0.22's build script rejected the system Python 3.14 interpreter. Fixed by enabling the `abi3-py39` feature so the extension builds against the stable Python ABI.
9. **PyO3 `allow_threads` captured non-`Send` `Bound` handles:** the closure passed to `py.allow_threads` borrowed `PyDict` and `Option<&PyDict>`. Fixed by extracting all Python data into owned Rust types before releasing the GIL.
10. **Kernel God Object refactor (2026-08-04):**
    - Embedded ACL cache, schema map, scoring helpers, and `find_similar` body removed from `Kernel`.
    - `AuthManager`, `SchemaRegistry`, `IndexCoordinator`, and `KnowledgeRepository` introduced; `Kernel` delegates to them.
    - `Subject::is_admin` made `pub(crate)` so `AuthManager` can evaluate admin status.
    - `tantivy::schema::Value` aliased as `TantivyValue` to avoid name collision with `knowledge::kom::Value`.
    - `KnowledgeCache` wired into `KnowledgeRepository` with invalidation on every write path.
    - Stray `.gitignore` files consolidated into a single root `.gitignore`.

---

# 3. Gate review vs VISION-AND-STRATEGY Phase 1 exit criteria

| Criterion | Status |
|---|---|
| Conformance suite green | ✅ 123 active Rust tests passed (39 conformance + 41 kernel unit + 7 durability + 8 index + 3 eval + 5 fuzz + 6 proptest + 7 MCP + 7 graph engine) |
| Crash-recovery clean | ✅ `d04` abrupt-termination + `d04b` random-boundary crash fuzz + `m02` MCP-restart proof continuity |
| P99 point read <10 ms | ✅ **83.1 µs** (120× headroom) |
| Flagship agent demo with memory replay | ✅ **m02: commit-with-provenance → verify → explain → prove → recall → restart-replay, all through MCP** |

**✅ PHASE 1 COMPLETE — the Trustworthy Memory Substrate is shipped:** durable, transactional, provenance-native, hybrid-recall, MCP-native, conformance-proven, and architecturally aligned with the HLD.

---

# 4. Known limitations (honest ladder)

| Limitation | Lands in |
|---|---|
| `notify` not exposed over MCP as a true streaming subscription (tools-based durable CDC exists) | Phase 2 (streaming MCP notifications) |
| MCP server is single-kernel, no authn beyond per-call subject | Phase 4 (OIDC/TLS) |
| At-rest signatures use a symmetric HMAC key; asymmetric signed checkpoints + key rotation not yet supported | Phase 4 ( signing key lifecycle, certificate-backed checkpoints) |
| Python SDK is synchronous-only in this spike (LangGraph async wrappers delegate to `asyncio.to_thread`) | Phase 2 (native async PyO3 bindings if profiling demands) |
| `KnowledgeCache` is in-memory only and bounded by simple LRU; no distributed/query-result/ACL caches | Phase 2+ (measure-driven expansion) |

---

# 5. Next (queued): Phase 2 execution

Pick the first prioritized Phase-2 deliverable from `VISION-AND-STRATEGY`:

- Streaming `notifications/subscribe` over MCP.
- Native async PyO3 bindings (if Python profiling demands).
- Cryptographic audit hashes / asymmetric signed checkpoints.
- Relationship indexes (graph traversal operators landed in `crates/engines/graph`).
- Deterministic simulation harness (`madsim`) for deeper fault injection.

---

# 6. How to reproduce

```bash
cargo test --workspace                # 123 active Rust tests green, zero warnings
cargo test -p aikoql-kernel --test durability -- --ignored d07   # P99 bench gate
cd crates/sdk/python && python -m venv .venv && .venv\Scripts\Activate.ps1
python -m pip install maturin pytest
python -m maturin develop             # build & install editable Python SDK
python -m pytest tests/test_sdk.py tests/test_adapters.py -v # Python SDK tests
.\target\debug\aikoql-mcp my.redb  # MCP server on stdio; connect any MCP client
```

Python SDK quick-start:

```python
from aikoql import aikoql, AikoqlLangGraphSaver, AikoqlCrewAIMemory

m = aikoql("memory.redb")
r = m.remember("alice", "claim", {"body": "AGI is possible"}, semantic={"confidence": 0.95})
ko = m.get("alice", r["koid"])
hits = m.find_similar("alice", text="AGI", k=5)

# LangGraph
cp = AikoqlLangGraphSaver("checkpoints.redb")
config = {"configurable": {"thread_id": "thread-1"}}
cp.put(config, {"id": "c1", "channel_values": {"x": 1}})

# CrewAI
mem = AikoqlCrewAIMemory("crew_memory.redb", role="researcher")
mem.save("Revenue grew 12% YoY")
mem.search("revenue", limit=3)
```

Example MCP exchange (the flagship):

```jsonc
// -> tools/call remember {type_name:"claim", semantic:{source:"sec-10k", confidence:0.99}}
// <- {"koid":"…","version":1,"commit_ts":…}
// -> tools/call explain {koid:"…"}
// <- {"source":"sec-10k","confidence":0.99,"verified":false,"evidence":[],…}
```

Repository: https://github.com/anckursingh/aikoql
