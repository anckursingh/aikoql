# Training Data Architecture — discovered interfaces (T-01 recon)

The design's Phase 0 acceptance: no guessed API contracts. Every interface
below was verified against the tree at the T-01 pre-fix head (662f974);
file:line cites are current as of that commit. The one place the design
was wrong is called out (§3).

## 1. Python SDK surface

`crates/sdk/python/python/aikoql/__init__.py` exports: `Agent`,
`McpClient`, `McpError`, `Transaction`, `Pool`, `PooledConnection`,
`PreparedStatement`, `BoundStatement`, adapters (crewai, langgraph), and
the optional PyO3 native module (`aikoql`, `__version__` — absent in
pure-MCP deployments).

- `Agent.connect(target)` (`agent.py:38`) — one entry point, two modes:
  a path (embedded, in-process PyO3) or `"host:port"` (MCP over TCP).
  Unified method surface: remember/get/find_similar/aikoql/create_index/
  relate/traverse/forget/health/metrics/batch/decide/agent_memory/
  discover_schema/session_init.
- `McpClient` (`mcp_client.py:129`) — the raw MCP client: connect/
  initialize/session_init/call_tool/begin, then one method per tool
  (remember, get, forget, find_similar, aikoql, prepare, relate,
  traverse, batch, health, discover_schema, decide, agent_memory,
  metrics, trace, explain, aikoql_stream). Every method is a thin
  `call_tool(name, args)` wrapper returning the tool's JSON envelope.
- Transactions: `begin(txn_id) -> Transaction` with execute/commit/
  rollback (`mcp_client.py:348`); `txn_begin/commit/rollback` tools
  (`tools/txn.rs`).
- `PreparedStatement` (`prepared.py`) — client-side `:name` placeholder
  validation; compiles on every execute.

## 2. MCP tool registry

`crates/services/api/mcp/src/tool_registry.rs` dispatches:
`remember, relate, forget, get, find_similar, trace, explain, traverse,
aikoql, metrics, discover_schema, health, agent_memory, session_init,
decide` plus the txn tools. This is the complete public tool surface the
Training Data Engine may call — nothing else is reachable over MCP.

## 3. Query path — TEXT aikoql (design-doc correction)

`McpClient.aikoql(query: str, subject=None)` sends `{"query": <text>}` to
`tool_aikoql` (`tools/query.rs:12`), which parses with
`aikoql_compiler::parser::parse`, executes CREATE/UPDATE/DELETE directly
via `kernel.remember`, and compiles everything else through the IR plan
pipeline. There is **no JSON query payload** on the public surface — the
design's `"language": "aikoql-json"` is wrong. The canonical schema
therefore pins `query_target = {"language": "aikoql", "query": "<text>"}`,
and T-05's query builder targets the compiler's text grammar.

## 4. Snapshot anchors

`tool_health` (`tools/admin.rs:190`) returns: `status, ready, journal_seq,
journal_lag_ms, object_count, connection_pool, audit_hash` (32-byte hex),
`uptime_seconds, semantic.state/detail`. `tool_metrics` (`admin.rs:78`)
returns journal sequence and object counts. T-02's knowledge revision is
`journal_seq + audit_hash` from this tool.

Two T-02 recon findings shape the implemented snapshot record
(`training/src/aikoql_training/snapshot.py`):

1. **Embedded health is a stub** — `Agent.health()` returns
   `{"ready": True, "status": "healthy"}` in embedded mode
   (`agent.py:187-190`); journal_seq/audit_hash exist only behind the
   MCP health tool. Snapshot capture therefore always goes over the
   server surface (`capture_from_agent` -> `Agent.health()`).
2. **No database identity is exposed** — the MCP initialize handshake
   carries only `serverInfo {name, version}` (`dispatcher.rs:100`);
   nothing names the database instance. `snapshot_id` binds
   `database_id + knowledge_revision`, with `database_id` an explicit
   operator parameter (the operator names the KB, e.g. "acmepay").

The invariant follows the design's split (§10): `snapshot_id` binds the
database state; `configuration_hash`, `source_manifest_hash`,
`generator_version` and `seed` are recorded fields — same snapshot +
same generator + same configuration + same seed = same example IDs.
`created_at` is metadata, excluded from identity.

## 5. Authorization

- Server: TCP token carries roles (`--tcp-token TOKEN::ROLES` convention,
  the P3-M9 trap); MCP sessions are tenant-scoped (R9 — session-injected
  caller identity with roles + tenant, `query.rs:21`).
- Client: `McpClient(host, port, token)`; `session_init(agent_id, run_id,
  tenant, roles)` (`mcp_client.py:315`); per-call `subject` argument,
  defaulting to `"query-user"` (`query.rs:17-20`).
- T-10's authorization scenarios therefore run as: same query, different
  subject/roles through the SAME public path — never by re-implementing
  ACL checks in Python.

## 6. KO serialization

`get(koid, subject)` returns the tool JSON envelope for one knowledge
object; KOIDs are 32-char hex. The exact envelope fields (origin, status,
confidence, valid_from/valid_to, version) are pinned by T-02's tests
against a live server, not restated here from memory.

## 7. Retrieval and context — compile_context (T-06 correction)

The T-01 recon recorded "no context-compiler tool" — that finding was
**wrong**. `compile_context` has been on the MCP surface since
MRFC-0070-A6 (`tool_registry.rs` → `tools/agent_knowledge.rs`), and
T-06's adapter (`training/src/aikoql_training/context/adapter.py`)
calls it directly through the public client — no Python retrieval
logic, no server change.

- **Contract**: `{koid, task, token_budget (default 2000), subject?}` →
  `{context_markdown, package, koid, task, token_budget, semantic,
  experiences}`. The koid names a KO carrying `ir_json` (direct
  knowledge) or an ingested document's sha256; anything else errors.
- **Package**: ranked `entities` / `facts` / `relations` with scores
  and justifications; `estimated_tokens`, `trimmed`, `status`
  ("healthy" / "semantic_fallback"). The adapter maps only the rows
  plus the facts' evidence (deduped, package order) into the schema's
  context shape.
- **ACL is server-side**: `get_ir_for_koid` reads the KO as the calling
  subject; unauthorized → ACCESS_DENIED, never rows (CTX-001). Over
  stdio the subject is per-call; over TCP every authenticated
  connection gets agent_id `"tcp-agent"` (`transport.rs`) — the subject
  name is connection-invariant, so the TCP denial boundary is the
  TENANT (`tcp_tenant_isolation_across_tokens`). T-06's unauthorized
  cell spawns two tokens in different tenants.
- **Staleness is the IR-version boundary**: the compiler compiles the
  live KO's `ir_json`, and its 5-min cache is fingerprint-keyed
  (task + budget + IR fingerprint + semantic fingerprint), so an
  updated document never serves its superseded facts (CTX-003). The
  kernel's validity bridge (`compile_context_with_validity`) is not
  wired into the tool; superseded-KO filtering is T-08's temporal work.
- **Client surface**: the Python SDK has no compile_context wrapper —
  `McpClient.call_tool` is the generic path; the adapter does the mode
  dispatch and rejects clients without a tool surface (embedded mode
  cannot compile).

T-03 adds the traverse-shape finding: `Agent.traverse(koid, rel,
depth)` returns `{"hits": [...]}` over MCP but a flat list in embedded
mode; hits carry `koid, depth, rel_type, direction`, with direction
`"outbound"`/`"inbound"` (`knowledge.rs:88-105`). `client.scan_edges`
normalizes both surfaces and inverts inbound hits into (from, rel, to)
edges.

## 8. Fixtures and test patterns

The SDK's tests are the house pattern to follow: `test_agent_embedded.py`
runs against an embedded in-process server (`Agent.connect(tmp_path)` —
no socket, no spawn); `scripted.py` drives scripted server scenarios;
conformance rides a real binary. Training integration tests use embedded
first, one MCP full-path cell for the wire surface (T-06+).

T-03's live cell proves the pattern end to end: seed KOs → relate →
`scan_edges` → generate → assert every answer equals the live value,
all over the spawned MCP server. T-04 extends it to a 3-KO chain
(settlement → checkout → gateway): the multi-hop scenario's
`expected_path` re-verifies edge by edge against the traversed graph.

## 9. Crate anchors the design §3 assumed

Verified present at 662f974: `crates/ingestion/src/{resolution,
embedding, secret_filter, context, pipeline, merge, ingest_dir,
ingest_incremental}.rs`; `crates/compiler/src/{parser, planner,
semantic}`; `crates/runtime/src/backend.rs` routes production opening
through Storage V2. The design's reuse claims hold; training code must
not import any of these directly — the SDK/MCP surface is the boundary.

## 10. TEXT grammar pins (T-05 recon)

`build_queries` renders against the real lexer/parser/lowering, so the
following contract is load-bearing for every T-05+ generator:

1. **String literals are double-quoted ONLY and have NO escape
   mechanism** (`parser/lexer.rs` `read_string` stops at the next `"`).
   A value containing `"` is unrepresentable; the single-quoted form
   does not lex at all.
2. **MATCH predicates address properties, never the KOID.** Text-level
   KOID addressing exists only in `UPDATE`/`DELETE` (as a
   double-quoted string).
3. **TRAVERSE is outbound-only, one rel_type per clause**, optional
   `DEPTH n` (default 1, 0 rejected at compile). This differs from the
   MCP `traverse` tool (inbound+outbound, CI-07) — same word, different
   semantics on the two surfaces.
4. **A traverse query must project a field.** `RETURN *` after
   TRAVERSE yields `RowSet::Traversal`, which `tool_aikoql` renders as
   `{"results": []}`; the Project op is what loads the KOs back.
5. **Literal typing is fail-closed** (`mod.rs` kq010): integral
   literals lower to `Value::Int` and cross-type property comparison
   fails closed, so ints render as integers and floats as decimals;
   negative numbers (`-` does not lex) and scientific notation (`e`
   lexes as an ident) are unrepresentable.
6. **Ident names** (type/property/relation) must match
   `[A-Za-z_][A-Za-z0-9_:]*` and must not be keywords (they lex as
   tokens).

`build_queries` therefore skips unrepresentable scenarios instead of
emitting a bad query; `verify_scenario` (the oracle) proves each
emitted query compiles, plans, executes and recovers its scenario's
hop targets over the public `tool_aikoql` surface.

## 11. Grounding (T-07)

`build_answer(scenario, context)` (`generators/answer.py`) certifies the
scenario's expected answer against the compiled package: the claim must
appear in a fact statement and every supporting fact's evidence must be
present in `context.evidence` — otherwise the example is REFUSED (None),
never emitted ungrounded. `validate_grounding(example)`
(`validation/grounding.py`) enforces the same trace fail-closed on the
schema shape: a grounded example that does not trace, an ungrounded one
that does, or `expected.evidence_ids` that do not trace to the
supporting evidence are all violations (§26 grounding gate: 100% of
accepted examples grounded).

- **Trace rule**: substring match of the answer inside the fact
  statement — the deterministic ceiling; semantic-equivalence grounding
  is the fine-tuned model's job at T-15, not a Python
  re-implementation.
- **Evidence identity**: the canonical sort-keyed JSON of the evidence
  dict — content-derived, stable across compiler runs (same IR ⇒ same
  package rows), shared between generator and validator.
- **Refusal semantics**: `build_answer` returns None, matching
  `build_queries`' skip pattern — a refused example is dropped, never a
  false positive.
