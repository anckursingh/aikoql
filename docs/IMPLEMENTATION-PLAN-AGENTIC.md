# AikoQL Agentic Plan — from knowledge database to the agent's database

Requested 2026-10-30, after the launch branch (`feature/aikoql-db-launch`,
PR #10) closed the September review's existential gap. Sibling plan:
`docs/TESTING-PLAN-AGENTIC.md`. Milestone ids: **AG-xx**.

This is a senior-architect plan, not a commitment: every milestone carries
the evidence that motivates it (a file:line, a measurement, or a live
observation), a RED that fails today, and an acceptance gate. Repo
conventions apply unchanged — RED first, one milestone = one commit set
(test RED → feat → docs), the re-stamp ritual, no performance claim without
a structural metric or a reproducible benchmark.

## 1. Where aikoql stands (2026-10-30, verified)

The September review (`docs/ARCHITECT-REVIEW-2026-09.md`, now HISTORICAL)
named HA as the existential gap and the epistemic engine as the moat. Since
then the gap has closed and the surface has grown:

| axis | September | now |
|---|---|---|
| replication / HA | none | log shipping, semi-sync, lease HA (file/S3/k8s), read replicas, standby reads, archive + PITR (MRFC-0080 R0–R6) |
| client surface | MCP stdio/TCP + Python | MCP stdio/TCP/**Streamable HTTP + OAuth 2.1 + CIMD** (verified against claude.ai), native framed protocol (D-15), TLS everywhere, SDKs in Rust/Python/Go/TypeScript/Java |
| epistemic engine | the moat | unchanged — still the moat: authority-graded assertions, KOID lineage, Conflict KOs, hash-chained audit, `AS_OF`/`HISTORICAL`/`EPISTEMIC` |

What an agent actually meets today — measured on the tip (fc1c9f7):

| observation | evidence |
|---|---|
| **103 MCP tools, 45 455 bytes of tool definitions** (~11k tokens) in every agent's context before its first call | `tools/list` on a fresh `serve`; registry: `crates/services/api/mcp/src/tool_registry.rs` |
| no MCP **resources** and no **prompts** (both answer method-not-found) | `resources/list`, `prompts/list` on the same server |
| **two memory systems**: the `memory_*` tools read/write a Markdown directory (`AIKOQL_MEMORY_DIR`), not the knowledge base — no provenance, lineage, conflicts or vectors for them | `tools/memory.rs:102` (`resolve_memory_dir`) |
| conversation → knowledge is **deterministic keyword extraction** | `summarize_conversation` ("never invents facts"); the cue classifier is a debt-ledger shortcut (`kernel/ops.rs:2181`, "naive keyword classification … upgrade: a real intent classifier") |
| **filtered scans and vector recall are brute force at scale**: 1M objects → structured_filter 22.9 s, vector_recall 17.0 s warm p50 (point read 0.023 ms, graph 0.069 ms stay flat) | `docs/certification/competitors/REPORT.md` §Scale ("Filter and vector are still brute-force O(N) plus an N log N sort") |
| one embedding model (all-MiniLM-L6-v2, 384-d, English-centric), no reranker | `engines/semantic/src/provider.rs:647`; no rerank stage anywhere in `crates/` |
| **placeholders under the "verifiable knowledge" claim**: audit "Ed25519" is an HMAC-SHA512 placeholder; key rotation only logs; tenant accounting is hard-coded `"default"` | `kernel/security/signing.rs:67-117`, `scheduler/key_rotation.rs:49-60`, `kernel/security/tenant.rs:75` (all in the shortcut ledger) |
| no memory lifecycle: no salience, decay, consolidation or TTL | no such code in `crates/kernel` or the MCP server |

**The thesis.** The engine is ahead of the field on *trust* (provenance,
contradictions, audit, time) and behind it on *agent ergonomics* and
*retrieval at scale*. The agent-memory products (mem0, Zep/Graphiti, Letta,
LangGraph's store) win by being easy to call well — one or two tools, LLM
extraction, scoped memories, good recall — and lose on trust: provenance is
app code, contradictions are last-write-wins. The plan below keeps the moat
and closes the ergonomics and scale gaps, in that order: an agent that
cannot find the right tool or the right memory never reaches the moat.

**Non-goals.** No race with Qdrant/Turbopuffer on raw ANN QPS or with
Postgres on OLTP beyond the roadmap's ≤2× gate; no hosted offering in this
plan (it is a business decision — the platform work in phase P makes it
possible); no LLM inside the server process (phase M uses the *client's*
model through MCP sampling).

## 2. Phases and milestones

Execution order: **A (agent surface) → M (memory) → R (retrieval at
scale) → T (trust debt) → X (multi-agent) → P (platform) → E
(ecosystem)**. A and T are independent and can interleave; R-01 gates the
scale claims in E-02.

Priority: **P0** = blocks "best agentic database" outright; **P1** = needed
to win head-to-head evaluations; **P2** = differentiation / polish.

### Phase A — the agent surface (what an agent sees first)

**AG-01 (P0) Tool profiles — a small default surface.** — **DONE 2026-10-30** (`--tool-profile core`: 13 tools / 6 169 bytes vs 103 / 43 284; ten groups via `tools_more`; config `[server] tool_profile`, `AIKOQL_TOOL_PROFILE`). Deviation: the default stays `full` — flipping it changes every existing agent's surface, so it is a separate decision.
- *Problem:* 103 tools / 45 KB of schema in context. Tool-selection accuracy
  degrades with tool count and every token of schema is paid on every turn.
- *Design:* `--tool-profile core|full|<custom>` (config
  `[mcp].tool_profile`, env `AIKOQL_TOOL_PROFILE`), default **core** for new
  installs, **full** kept for existing configs (no silent break):
  `core` ≈ 12 tools — `session_init`, `recall` (AG-02), `remember`, `get`,
  `relate`, `forget`, `trace`, `resolve_conflict`, `find_conflicts`,
  `aikoql` (query), `health`, `tools_more`. `tools_more {group}` returns a
  group's schemas and enables them for the session (`notifications/tools/
  list_changed`). Groups: knowledge, epistemic, ingestion, admin, programs,
  eval, txn, compliance.
- *Gate:* core `tools/list` ≤ 8 KB; every `full` tool reachable through a
  group; existing clients unchanged under `full`.

**AG-02 (P0) One `recall` tool — retrieval as a single good call.** — **DONE 2026-10-30** (semantic-led by default: 6 no-shared-word questions, vector 5/6 top-1 vs rrf 3/6 and text 2/6). Deviations: graph expansion lists each item's `related` edges rather than re-ranking with them; the token budget is a ~4 bytes/token estimate, not a tokenizer count; the recall@10 gate waits for the agent eval set (AG-61).
- *Problem:* an agent must choose among `find_similar`, `memory_search`,
  `traverse`, `aikoql`, `compile_context`, `agent_memory` — the choice is the
  failure mode.
- *Design:* `recall {query, scope?, as_of?, max_tokens?, k?}` = lexical +
  vector (RRF, existing) + bounded graph expansion (1 hop over
  DERIVED_FROM/MENTIONS, existing `traverse`) + epistemic filter (default:
  exclude superseded/retracted, flag contested) → a **token-budgeted**
  answer: items with `koid`, snippet, status, authority, `why`
  (provenance one-liner) and the open conflicts touching them.
- *Gate:* recall@10 on the agent eval set (TESTING-PLAN-AGENTIC §2) ≥ the best of the current tools
  used by an oracle; output never exceeds `max_tokens` (tokenizer-counted).

**AG-03 (P1) MCP resources, subscriptions and prompts.**
- *Design:* resources `aikoql://ko/{koid}`, `aikoql://conflicts/open`,
  `aikoql://session/{id}/context`; `resources/subscribe` rides the existing
  event stream (`GET /mcp`, subscription events); prompts
  `aikoql-context` (compile context for a task), `aikoql-review-conflicts`.
- *Gate:* the official SDK lists/reads/subscribes (extends
  `scripts/e2e-mcp-http.py`).

**AG-04 (P1) Structured tool output.**
- *Design:* `outputSchema` + `structuredContent` (MCP 2025-06-18) for the
  core tools; text content stays for older clients.
- *Gate:* SDK validates structured results against the schema.

**AG-05 (P2) Error ergonomics for agents.** Every tool error carries
`code`, `hint`, and a `next` suggestion (an existing pattern in
`error_codes.rs`, extended to all tools). *Gate:* no tool returns a bare
string error (architecture hygiene leg).

### Phase M — memory that manages itself

**AG-10 (P0) One memory store.** — **DONE 2026-10-30** (Memory KOs as the source of truth, the Markdown files written as a view; `memory_search` served from the knowledge base; an idempotent import at start on writing nodes). Deviations: the files stay authoritative-looking rather than becoming an export-only format, for compatibility; memories are found by name with a scan of the Memory KOs (shortcut-marked).
- *Problem:* `memory_*` tools write Markdown files outside the knowledge
  base — none of the moat applies to them.
- *Design:* `memory_*` become views over KOs of type `Memory`
  (scope-tagged, embedded, versioned, conflict-checked). The Markdown
  directory becomes an import/export format (`aikoql memory import|export`),
  and existing directories are imported once on first start (idempotent,
  `idempotency_key` = file path + hash).
- *Gate:* a memory written via `memory_store` is found by `recall`, has a
  `trace`, and an edit produces a new version with lineage.

**AG-11 (P0) Memory scopes as a first-class concept.** — **DONE 2026-10-30** (`private` / `shared` / `session`, enforced by the kernel's owner + ACL with a new tenant-confined `*` principal). Deviation: three scopes instead of the org → user → agent → session chain — aikoql has one identity per caller (an OAuth login is `oauth:<user>`; a token or stdio subject is the agent), so user and agent are the same principal; a separate user/agent split needs an identity model first.
- *Design:* scope = `org / user / agent / session` (tenant stays the
  isolation boundary above it). Writes default to the session's scope;
  reads see the scope chain (session → agent → user → org) with
  precedence on conflict; promotion (`promote {koid, to: user}`) is an
  explicit, audited operation.
- *Gate:* isolation matrix (TESTING-PLAN §M): no read crosses users; the
  chain order is deterministic.

**AG-12 (P0) LLM extraction through MCP sampling.** — split 2026-10-30. **AG-12a DONE**: `remember_facts` — the calling agent extracts (it is the model, with the conversation in context) and the server judges each fact (created / duplicate / conflict → Conflict KO / superseded) with provenance. Why the split: Claude Code and Claude Desktop reportedly do not support MCP sampling, and sampling needs reply routing in all three transports. **AG-12b (open)**: sampling as an optional path for clients that support it, on the same judging code.
- *Problem:* keyword extraction (`ops.rs:2181`) is where mem0/Zep win.
- *Design:* when the client advertises `sampling`, `summarize_conversation`
  / `remember_conversation` asks the **client's** model (no API key in the
  server) for candidate facts against a fixed JSON schema; the server then
  does what an LLM-only memory cannot: dedupe against existing KOs
  (vector + exact), contradiction check → Conflict KO, authority from the
  speaker, provenance = message range. No sampling → today's deterministic
  path (unchanged, the fallback and the oracle).
- *Gate:* on the agent eval set's conversations, extraction F1 ≥ the deterministic
  path; zero facts without a message-range provenance; every contradiction
  with an existing fact becomes a Conflict KO (never an overwrite).

**AG-13 (P1) Consolidation, salience and decay.**
- *Design:* per-KO `salience` (recency × access × authority × links),
  updated on access; a scheduler job consolidates near-duplicates (existing
  `merge`/`supersede`, so lineage is kept) and demotes low-salience
  session memories after a TTL (configurable per scope; never deletes —
  `forget` stays the only deletion, and stays GDPR-audited).
- *Gate:* consolidation is lineage-preserving (every merged KO traces to
  its sources); recall@10 does not drop after consolidation on the agent eval set.

**AG-14 (P2) Procedural memory.** `record_experience`/`find_experiences`
exist; add outcome-weighted retrieval (successful procedures rank first)
and a `lesson` KO type distilled by AG-12 sampling.

### Phase R — retrieval at scale

**AG-20 (P0) ANN on the measured path.**
- *Status (2026-10-31): AG-20a done.* Cause: the coordinator asked the
  index for `usize::MAX` candidates, so HNSW walked every node and the
  leg read every scoring record. Now a bounded pool (max(512, 16k),
  widened 4x while the guards leave < k): 1M p50 11.9 ms, recall@10
  0.992 (`benches/ann_recall`, 64-d random — the hard case). Then every
  recall path: VectorOnly with text (recall) takes the bounded leg; RRF
  ranks in N log N (was N^2); hybrid fusions score a bounded vector ∪
  text union (RRF 100k: 232 → 7.1 ms); the unindexed tail is scored
  exactly; no zero-score pads. Open: 20b filtered-ANN buckets, 20c
  quantization, 20d persisted segments.
- *Problem:* 1M vector_recall 17.0 s, brute force (REPORT §Scale). An HNSW
  exists (`engines/vector`, attached to `find_similar` since M18) — the
  first step is to establish why the certified cell does not use it
  (filtered path? fusion path? scope filter?), then make every recall path
  ANN-first.
- *Design:* filtered ANN (filter-aware graph search — e.g. ACORN-style
  predicate traversal — with an exact fallback below a selectivity
  threshold), int8 scalar quantization with exact re-score of the top-k,
  persisted index segments (no rebuild on restart).
- *Gate:* 1M vector_recall p50 ≤ 50 ms warm with recall@10 ≥ 0.95 vs the
  exact oracle; RSS at 1M ≤ the current 1 005 MB; structural metric:
  distance computations per query (must be sublinear).

**AG-21 (P0) Indexed structured filters.**
- *Correction (2026-10-31):* the 1M structured_filter cell
  (`MATCH note WHERE topic == "pet" RETURN *`) returns half the dataset —
  500 000 rows, 46 µs each through the Python SDK. No index can make it
  10 ms; it measures result transfer. Equality indexes already exist and
  the CBO uses them (`Strategy::PropertyIndex`, P5-M9). AG-21 needs a
  selective cell (0.1–1%) before its gate means anything.
- *Problem:* 1M structured_filter 22.9 s (brute force + sort).
- *Design:* property secondary indexes (`index_create` exists) used by the
  planner (the CBO in `runtime/src/cbo.rs` estimates only the first Eq —
  a shortcut-ledger row); range + composite indexes; top-k without full
  sort.
- *Gate:* 1M indexed filter p50 ≤ 10 ms; rows decoded per query ≤ 2× the
  result size (structural).

**AG-22 (P1) Reranking and diversity.** Optional cross-encoder rerank
stage (candle, local, installed like the embedding model — incl. the
AIKOQL_MODEL_AUTO_INSTALL opt-in) + MMR for diverse results. *Gate:* nDCG@10
improves on the agent eval set; latency budget ≤ 30 ms for 50 candidates.

**AG-23 (P1) Embedding model choice.** Per-collection model (multilingual
and larger models via `model install`), a re-embed migration job
(background, resumable, dual-read during migration). *Gate:* switching
models never serves mixed-dimension results; migration survives a kill.

### Phase T — trust debt (the moat's own credibility)

**AG-30 (P0) Real Ed25519 audit signatures** — **DONE 2026-10-30** (`ring` Ed25519, RFC 8032 TEST 1-3; `evidence_pack` signs the audit chain under `AIKOQL_AUDIT_SIGNING_KEY`; verified by Python `cryptography` from the public key). The placeholder turned out never to have been called, so no chain migration was needed. Not yet: signing the journal per commit, CI wiring of the e2e script. (ed25519-dalek, RFC 8032),
verifiable outside aikoql; migrate existing chains (re-sign with a marked
algorithm switch, never rewrite history). *Gate:* RFC 8032 test vectors;
an external verifier (Python `cryptography`) accepts an exported chain.

**AG-31 (P0) Real KEK rotation.** Implement the rotation loop
(`key_rotation.rs:60` — "actual rotation would call envelope.rotate_kek").
*Gate:* rotate under write load; every object readable after; old KEK
unusable.

**AG-32 (P1) Tenant accounting from KO metadata** (`tenant.rs:75`). *Gate:*
usage per tenant matches the oracle count.

**AG-33 (P1) Prompt-injection provenance.** Every KO carries a trust class
of its source (`user`, `tool_output`, `web`, `document`, `system`);
`recall` returns it, and content from untrusted classes is fenced in tool
output (delimited, marked as data). Agents can filter by trust. *Gate:*
the injection corpus (TESTING-PLAN §T) never surfaces untrusted text
outside its fence.

**AG-34 (P1) OAuth scopes → roles.** Today every OAuth grant has scope
`mcp` and the login's roles. Add `mcp:read`, `mcp:write`, `mcp:admin`
(consent page shows them), intersected with the login's roles. *Gate:* a
read-scoped token cannot write (every write tool).

### Phase X — multi-agent

**AG-40 (P1) Live queries.** `subscribe {aikoql query}` → events when the
result set changes (built on the existing commit stream + event outbox).
*Gate:* no missed and no duplicate events under concurrent writers.

**AG-41 (P2) Coordination primitives.** Leases/claims on KOs (task
claiming among agents) on top of `txn_*` CAS; handoff KO type with
provenance. *Gate:* N agents, M tasks: each task claimed exactly once,
claims expire on agent death.

### Phase P — platform

**AG-50 (P1) Observability.** OpenTelemetry traces per tool call (span =
tool, kernel ops as children), per-tool latency/error metrics, a
`recall` quality counter (empty results, conflict hits). *Gate:* a trace
for every tool call in the e2e run.

**AG-51 (P1) Kubernetes.** Helm chart (leader + standby with the k8s lease,
already apiserver-certified), the open "real k8s pod" item, PVC snapshot
docs. *Gate:* kind-cluster failover e2e in CI (nightly).

**AG-52 (P2) Login backoff survives restarts** (in-memory today) and a
per-IP layer behind proxies (`X-Forwarded-For` trusted only from a
configured proxy).

### Phase E — ecosystem and proof

**AG-60 (P1) Framework adapters.** CrewAI and LangGraph exist
(`sdk/python/.../adapters`). Add LlamaIndex, OpenAI Agents SDK, Vercel AI
SDK (TS), each as a memory/store provider over `recall`/`remember`.
*Gate:* each adapter's upstream example passes against a live server in
CI.

**AG-61 (P0) Public agent-memory benchmark — measured, not claimed.**
Run published long-term-memory benchmarks (e.g. LongMemEval, LoCoMo — the
choice recorded with versions) against aikoql and the agent-memory
products at default settings; publish with the REPORT.md candor. Adds the
cells the field does not measure: contradiction handling, provenance
completeness, time-travel correctness. *Gate:* reproducible harness in
`benchmarks/`, results committed with the toolchain and model versions.

**AG-62 (P2) Hosted trial path.** One-command public trial (the verified
Cloudflare-tunnel recipe, AGENTIC-QUICKSTART) plus a container template
for common PaaS — the prerequisite for any hosted offering.

## 3. Sequencing

| wave | milestones | why first |
|---|---|---|
| 1 | AG-01, AG-02, AG-10, AG-30 | the first contact (tools, recall, one store) and the moat's credibility; all small-to-medium, no storage changes |
| 2 | AG-11, AG-12, AG-20, AG-21, AG-61 (harness) | memory quality + scale — the head-to-head evaluation axes; the benchmark harness lands with them so every claim is measured |
| 3 | AG-03, AG-04, AG-13, AG-22, AG-31, AG-33, AG-34 | depth and safety |
| 4 | AG-23, AG-32, AG-40, AG-50, AG-51, AG-60 | platform and reach |
| 5 | AG-05, AG-14, AG-41, AG-52, AG-62 | polish |

## 4. Risks

| risk | mitigation |
|---|---|
| tool profiles break existing agent configs | default stays `full` for configs that exist; `core` for new installs; changelog + quickstart call it out |
| sampling-based extraction quality varies by client model | deterministic path stays the oracle; extraction results are proposals with provenance, conflicts never overwrite |
| filtered ANN recall collapses at low selectivity | exact fallback under a measured selectivity threshold; recall@10 gate per selectivity bucket |
| Ed25519 migration "rewrites history" | algorithm switch is a signed chain event; old entries verify under the old scheme |
| benchmark results unflattering | publish anyway, decomposed — the REPORT.md precedent is a credibility asset |
