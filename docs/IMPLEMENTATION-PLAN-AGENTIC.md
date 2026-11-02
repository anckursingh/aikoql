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
- *Status (2026-11-01):* **done** (branch `feature/agentic-wave-4`).
  Both transports share `handle_message`, so the three resources, the
  two prompts and `resources/subscribe` serve over stdio and HTTP alike.
  Reads compile fresh state; a subscription emits
  `notifications/resources/updated` on kernel events the uri could
  reflect (shortcut: the kernel's EventFilter has no type dimension, so a
  `conflicts/open` subscription hints on any write under the subject).
  Tests `tests/mcp_resources_prompts.rs` (r001–r006, incl. the wire-level
  resources/updated frame); the e2e gate lists/reads/subscribes through
  the official SDK (the venv's SDK predates Client.listen, so the
  notification frame itself stays pinned at the wire level).

**AG-04 (P1) Structured tool output.**
- *Design:* `outputSchema` + `structuredContent` (MCP 2025-06-18) for the
  core tools; text content stays for older clients.
- *Gate:* SDK validates structured results against the schema.
- *Status (2026-11-01):* **done** (branch `feature/agentic-wave-4`).
  The 13 core tools' defs carry `outputSchema` — real schemas for
  remember/get/recall, the honest `{"type":"object"}` for the rest —
  and a successful `tools/call` result carries `structuredContent`
  beside the text (errors carry none; older clients ignore the extra
  field — the spec's own migration story). Tests
  `tests/mcp_structured_output.rs` (s001–s004); the e2e gate validates
  remember/get/recall structured results against their declared schemas
  with jsonschema through the official SDK.

**AG-05 (P2) Error ergonomics for agents.** Every tool error carries
`code`, `hint`, and a `next` suggestion (an existing pattern in
`error_codes.rs`, extended to all tools). *Gate:* no tool returns a bare
string error (architecture hygiene leg).
- *Status (2026-11-01): done.* `wrap_result` errors carry `hint` (what
  went wrong) + `next` (what to do) beside `code`/`message`/`retryable`/
  `suggestion` (kept for older clients). `txn_dispatch` now routes through
  the shared envelope — its hardcoded INTERNAL misclassified e.g. a fenced
  write. Gate: `ag05_no_tool_returns_a_bare_string_error` drives every
  tool in the list with empty args and asserts every isError result
  parses as the envelope with all three fields.

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

- *Status (2026-10-31):* **AG-12b done** (branch
  `feature/agentic-wave-4`). With a client that advertises `sampling`,
  `summarize_conversation` sends `sampling/createMessage` (the
  conversation plus a fixed JSON schema for the facts), judges the model's
  candidate facts with `remember_facts`'s shared `judge_extracted` — the
  same scan and dedupe/conflict/supersede logic — and reports the judged
  statuses in the response. Any failure (no capability, garbage answer,
  a 30 s timeout, no stdio sink) degrades to the deterministic summary;
  its response shape is unchanged, and s002 pins that byte-for-byte.
  Tests `tests/mcp_memory_sampling.rs` (s001 judged created+conflict with
  message-range provenance, s002 guard, s003 fallback). The stdio loop
  gained a reader thread so replies reach their waiter while a tool is
  still running; requests still dispatch one at a time. **HTTP leg
  (2026-11-01):** closed as consciously unsupported — `serve
  --metrics-addr` never starts the stdio loop (S-12), the sampling state
  is stdio-only, and HTTP initialize no longer touches it (h019 guards
  the contract: the deterministic summary answers, the stdio stream
  stays silent); server-initiated requests over Streamable HTTP would
  ride the GET stream, which nothing consumes sampling replies on
  today. Remaining open: the gate's eval measurement (needs the
  AG-61 run).

**AG-13 (P1) Consolidation, salience and decay.**
- *Design:* per-KO `salience` (recency × access × authority × links),
  updated on access; a scheduler job consolidates near-duplicates (existing
  `merge`/`supersede`, so lineage is kept) and demotes low-salience
  session memories after a TTL (configurable per scope; never deletes —
  `forget` stays the only deletion, and stays GDPR-audited).
- *Gate:* consolidation is lineage-preserving (every merged KO traces to
  its sources); recall@10 does not drop after consolidation on the agent eval set.
- *Status (2026-11-01): done* — salience (recency × accesses × authority ×
  links) stamped at `remember` (authority = kernel-managed origin rank),
  refreshed on access (recall bumps counts; refresh skips unchanged
  values), a cadence job merges near-duplicates (existing `merge`,
  `derived_from` lineage kept — AG-13a) and demotes session-scoped
  memories past the 24 h TTL below salience 0.5 (epistemic Superseded;
  nothing deleted — AG-13c). The recall@10 gate (AG-13b) stays open
  until the AG-61 eval run.

**AG-14 (P2) Procedural memory.** `record_experience`/`find_experiences`
exist; add outcome-weighted retrieval (successful procedures rank first)
and a `lesson` KO type distilled by AG-12 sampling.
- *Status (2026-11-02): done.* `record_experience` returns an
  `ExperienceRecord` and materializes a carried `lesson` as a first-class
  `aikoql:lesson` KO (statement, `experience` koid backlink, same
  provenance/ACL as its experience); `match_experiences` multiplies the
  score by an outcome weight — keyword-classified success (1.0) / failure
  (0.5) / unclassified (0.8). Gate:
  `ag14_outcome_weighting_ranks_success_first` +
  `ag14_lesson_is_materialized_as_a_ko` in `crates/kernel/tests/
  experiences.rs`. The MCP `record_experience` tool also returns
  `lesson_koid`. (The AG-12b sampling path stays open — the lesson text
  arrives on the request; the caller distills it.)

### Phase R — retrieval at scale

**AG-20 (P0) ANN on the measured path.**
- *Status (2026-10-31): AG-20a done.* Cause: the coordinator asked the
  index for `usize::MAX` candidates, so HNSW walked every node and the
  leg read every scoring record. Now a bounded pool (max(512, 16k),
  widened 4x while the guards leave < k): 1M p50 11.9 ms, recall@10
  0.992 (`benches/ann_recall`, 64-d random — the hard case). Then every
  recall path: VectorOnly with text (recall) takes the bounded leg; RRF
  ranks in N log N (was N^2); hybrid fusions score a bounded vector ∪
  text union (RRF 100k: 232 → 7.1 ms; 1M: 12.7 ms p50, p95 20.0); the unindexed tail is scored
  exactly; no zero-score pads. Open: nothing on 20 — b and c below.
  20d: the HNSW already persisted in the maintainer
  checkpoint, but only replication leaders took one on a timer — every
  server does now (ck001); the embedded SDK still checkpoints on close only.
- *Status (2026-11-01): AG-20b done.* A VectorOnly query whose filter is a
  lone type now scores the repo type index directly when the type has
  ≤ 4096 heads — candidates stay sublinear in the object count and recall
  over the filtered set is exact (a 20/5020 split: 7580 → 20 scored
  candidates; gate: `crates/kernel/tests/filtered_ann.rs`, 2 tests, incl.
  a guard that the unfiltered path keeps the bounded pool). Property-pair
  filters and bigger types keep the bounded ANN pool; ACORN-style filtered
  graph traversal is the documented deep form (shortcut comment in
  `coordinator.rs`).
- *Status (2026-11-01): AG-20c done.* The rebuild copy (`model_map`) stores
  int8 scalar quantization — codes + one uniform scale per vector — ~4x
  smaller than the f32 copy. The graph keeps exact f32 (navigation
  unchanged), rebuilds dequantize for re-insert, and cosine is
  scale-invariant so the round-trip error is bounded by scale/2 per dim
  (guard: post-rebuild scores within 2e-2 of exact; gate:
  `quantized_bytes` ≤ dim+4 per vector + the tolerance guard in
  `engines/vector`). Published hits are re-scored from the store by the
  kernel, so quantization error never reaches a published score — the
  exact top-k re-score is the coordinator's existing committed-cosine
  chain, now documented as such. Checkpoint format unchanged (vectors
  were never persisted). The RSS gate (≤ 1005 MB at 1M) is verified by
  the next 1M bench run, not by a unit test.
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
- *Status (2026-10-31): AG-21 done* (`benches/filter_select`, 100k): eq
  0.1% 0.143 ms, range 0.1% 0.151 ms (was 133.6), eq under 100 live
  writes 0.254 ms (was a full scan — one write after analyze switched
  indexes off), build 32.85 → 0.26 s. Fixed on the way: Int/Float Eq
  through an index missed rows; scan_from never seeked.
- *Status (2026-11-02): the two open items landed (AG-21d).* **Composite
  range**: a range over the FIRST property of a composite index serves
  as a byte-prefix scan — the hi bound is the exclusive successor of its
  key, so hi's own suffix rows stay included (RED `idx6_003` +
  `cbo_default_010`; `covers_range` keeps Eq on the full tuple). **CBO
  range histogram**: `analyze` builds a per-property quantile histogram
  (64 buckets over the index's ordered keys, per-bucket row counts) for
  orderable values (Num/Text/Bool) with ≥ 64 distinct; `range_fraction`
  estimates a range's match fraction, the range index decision prices
  from it instead of materializing candidates (the exact-count probe is
  the fallback — never worse), and a range Filter without an Eq conjunct
  prices its rows from it (RED `cbo_a07` +
  `cbo_range_filter_prices_from_the_histogram`). Shortcut: histograms
  rebuild at analyze (an explicit DBA op) — no incremental
  maintenance, so the estimate ages with the watermark like every
  other statistic.
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
- *Status (2026-11-01): MMR done* — `memory_search` diversifies its
  oversampled candidates (greedy MMR, relevance = score normalized to the
  top hit, novelty = 1 − max token-Jaccard to the picked, λ=0.7) so
  near-duplicate memories no longer crowd out distinct ones; the 50
  candidate pass stays well inside the 30 ms budget. Open: the optional
  cross-encoder rerank stage (candle) and the nDCG@10 gate until the
  AG-61 eval run.

**AG-23 (P1) Embedding model choice.** Per-collection model (multilingual
and larger models via `model install`), a re-embed migration job
(background, resumable, dual-read during migration). *Gate:* switching
models never serves mixed-dimension results; migration survives a kill.
- *Status (2026-11-01): AG-23 DONE — dim safety + catalog bindings + migration.*
  `ModelAwareVectorIndex` (`engines/vector`) shards one HNSW per model:
  each shard adopts its own dim (P5-M27's global drop becomes per-model),
  model-scoped queries answer from their shard, and model-less queries
  merge only shards whose dim equals the query vector's — a
  mixed-dimension ranking is impossible by construction; same-dim models
  merge (dual-read). Checkpoints: `models.json` first (fail-closed load)
  + one dir per shard; legacy single-graph checkpoints load as the ""
  shard, still label-filtered. The service and the Python SDK construct
  the wrapper. Gate: 3 unit tests + `crates/kernel/tests/model_dims.rs`
  (model-scoped and dim-scoped queries never mix; checkpoint round-trip).
  AG-23b1: `catalog_set_type_model` / `catalog_type_model` journal a
  "model" catalog row per type (persists, replaces in place; binding an
  undeclared type is inert). Gate: ct007.
  AG-23b2: enrichment consumes the binding — `AiProvider::enrich` takes
  the bound model (the binding wins over the provider's configured
  default), and the engine skips only a KO already on its bound model, so
  any binding change re-embeds the collection: live (the catalog row's own
  event triggers a per-type rescan) and resumably (the start catch-up
  re-runs the scan — idempotent skip, so a kill mid-migration costs
  nothing). Dual-read during migration is the shards' same-dim merge, and
  the old model's vectors stay answerable until old-model queries stop.
  Gate: 4 semantic tests (bound-model use, catch-up migration, live
  migration, kill-resume + idempotent re-scan).

### Phase T — trust debt (the moat's own credibility)

**AG-30 (P0) Real Ed25519 audit signatures** — **DONE 2026-10-30** (`ring` Ed25519, RFC 8032 TEST 1-3; `evidence_pack` signs the audit chain under `AIKOQL_AUDIT_SIGNING_KEY`; verified by Python `cryptography` from the public key). The placeholder turned out never to have been called, so no chain migration was needed. Status (2026-11-02): the two "not yet" items landed — every journal event committed under the signer carries an Ed25519 signature chaining to the previous one (`KnowledgeEvent.audit_signature` over prev signature ‖ audit hash, all four commit paths, lenient decode of pre-AG-30 rows); `evidence_pack` emits the `journal_signatures` stream; `scripts/e2e-audit-verify.py` verifies the whole chain with Python `cryptography` and its CI leg runs in the e2e-dogfood job (wired at 5bc002e). shortcut: the fresh-open catalog bootstrap events predate the signer and stay unsigned — tighten when a signer is plumbed into `Kernel::open`. (ed25519-dalek, RFC 8032),
verifiable outside aikoql; migrate existing chains (re-sign with a marked
algorithm switch, never rewrite history). *Gate:* RFC 8032 test vectors;
an external verifier (Python `cryptography`) accepts an exported chain.

**AG-31 (P0) Real KEK rotation.** Implement the rotation loop
(`key_rotation.rs:60` — "actual rotation would call envelope.rotate_kek").
*Gate:* rotate under write load; every object readable after; old KEK
unusable.
- *Status (2026-10-31): AG-31 done* — KeyRing (wrapped store data key,
  generation-safe rotation), `aikoql-mcp key rotate`, the job rotates;
  kr001–kr004 + the job test. Found on the way: the derived store key
  (any rotation = an unreadable store), the DEK first-use race, the job's
  hanging shutdown. Passphrase change landed 2026-11-02:
  `aikoql-mcp key change-passphrase NEW [DB]` re-wraps the master key
  file under a new passphrase — the master key itself never changes (no
  data key moves), the old passphrase stops opening the database, and
  the ring keeps its passphrase copy in sync so a later rotation still
  wraps under the new one (kr005). DEK re-encryption landed 2026-11-02:
  `aikoql-mcp key roll-deks [DB]` mints fresh tenant data keys and
  re-encrypts every encrypted field through the journal (one committed
  version per object — the journal hashes the ciphertext, so this is a
  commit, not a rewrite); both generations persist before the walk
  (crash-safe), readers fall back to the previous generation, a re-run
  resumes without swapping generations again, and a later `key rotate`
  rewraps the previous generation too so history stays readable (kr006).
  shortcut: a roll can be re-run but not repeated — a second roll is a
  no-op walk (returns 0); forcing a fresh roll needs a manual reset of
  the DEK rows, which nothing offers yet. Open: cloud KMS `install`
  (AWS/Azure/GCP return unsupported).

**AG-32 (P1) Tenant accounting from KO metadata** (`tenant.rs:75`). — **DONE 2026-11-01** (rebuild buckets each head under its own `metadata.tenant`, tenant-less → "default"; remember/transact creates and both forget modes keep the live counters in sync; gate: 5 tests — tenant unit tests + `tenant_accounting.rs`). Shortcut closed 2026-11-02: `open_with` scans the heads on every non-standby open and rebuilds the counters from each head's `metadata.tenant` (RED `ag32_reopen_rebuilds_usage_from_store`). Remaining shortcut: the rebuild decodes every head on each open — switch to a persistent per-tenant watermark KV prefix when tenants number in the hundreds (the `tenant.rs` module doc names the same upgrade point). *Gate:* usage per tenant matches the oracle count.

**AG-33 (P1) Prompt-injection provenance.** Every KO carries a trust class
of its source (`user`, `tool_output`, `web`, `document`, `system`);
`recall` returns it, and content from untrusted classes is fenced in tool
output (delimited, marked as data). Agents can filter by trust. *Gate:*
the injection corpus (TESTING-PLAN §T) never surfaces untrusted text
outside its fence.
- *Status (2026-10-31): AG-33 done* — ij001: 300-payload corpus green;
  `trust` is a KO extension (undeclared reads as untrusted), recall and
  memory_search return it, accept a `trust` filter, and fence untrusted
  text in a per-response nonce fence; memory files carry the class too.

**AG-34 (P1) OAuth scopes → roles.** Today every OAuth grant has scope
`mcp` and the login's roles. Add `mcp:read`, `mcp:write`, `mcp:admin`
(consent page shows them), intersected with the login's roles. *Gate:* a
read-scoped token cannot write (every write tool).
- *Status (2026-10-31): AG-34 done* — o011; the read-tool list is an
  allowlist (a new tool is a write until classified, authz.rs).

### Phase X — multi-agent

**AG-40 (P1) Live queries.** `subscribe {aikoql query}` → events when the
result set changes (built on the existing commit stream + event outbox).
*Gate:* no missed and no duplicate events under concurrent writers.
- *Status (2026-11-01): AG-40 done* — `koql/subscribe`/`koql/unsubscribe`
  MCP methods over the kernel's durable CDC (subscribe/replay/ack): the
  response carries the current set, each change streams as a
  `notifications/koql/updated` frame with a strictly increasing seq, and
  re-subscribing the same id resumes from the acked seq. Subscribe-before-
  eval closes the last_seq window (a commit landing mid-setup would be
  neither backlog nor live); the snapshot diff absorbs replay/live
  boundary double-evaluations. Gates: the in-process test pins no
  missed/no duplicate under 4 concurrent writers; the wire test pins the
  frames and the durable resume. shortcut: every commit re-evaluates (no
  type-narrowed EventFilter) and intermediate states coalesce into the
  final-state emission.

**AG-41 (P2) Coordination primitives.** Leases/claims on KOs (task
claiming among agents) on top of `txn_*` CAS; handoff KO type with
provenance. *Gate:* N agents, M tasks: each task claimed exactly once,
claims expire on agent death.
- *Status (2026-11-02): done.* Kernel ops `claim_task` / `release_task` /
  `handoff_task` (MCP tools of the same names): a claim is an
  `expected_version` CAS under the pipe lock with kernel-managed
  `claim_holder` / `claim_until` extensions (a generic update carries the
  claim forward; only the ops clear it), so exactly one claimant wins and
  the loser gets `ClaimConflict`. Leases are wall-clock — an agent's death
  frees its tasks; the holder re-claiming renews. Handoff requires the
  holder: the claim moves to `to` (same lease end) and an
  `aikoql:handoff` KO records task/from/to with evidence, Asserted and
  agent-derived, readable by the recipient. Gate:
  `crates/kernel/tests/coordination.rs` (5 tests: 4 agents × 6 tasks
  claimed exactly once under threads, expiry, renew/release, ACL, handoff
  provenance) + the claim tool round-trip in the MCP suite.

### Phase P — platform

**AG-50 (P1) Observability.** OpenTelemetry traces per tool call (span =
tool, kernel ops as children), per-tool latency/error metrics, a
`recall` quality counter (empty results, conflict hits). *Gate:* a trace
for every tool call in the e2e run.

Status (2026-11-01): AG-50 done. Every tools/call emits a `tool_call`
span with a `tool` attribute (kernel ops as child spans from the tool
bodies); `AIKOQL_OTEL_ENDPOINT` turns on OTLP/HTTP export through a
blocking client (no async runtime in the binary), batch exporter with
no flush-on-shutdown (shortcut: process exit, gate test polls the
collector). The /metrics scrape carries `aikoql_tool_calls_total`,
`aikoql_tool_errors_total` and `aikoql_tool_latency_seconds_total`
labeled by tool, plus `aikoql_recall_quality_total` (empty/conflict,
always rendered). The e2e gate runs the server in metrics-only mode
(stdio and metrics cannot coexist: `--metrics-addr` alone is the S-12
metrics-only serve) with MCP over the metrics surface, decodes the
exported OTLP protobuf on a fake collector, and asserts the scrape.

**AG-51 (P1) Kubernetes.** Helm chart (leader + standby with the k8s lease,
already apiserver-certified), the open "real k8s pod" item, PVC snapshot
docs. *Gate:* kind-cluster failover e2e in CI (nightly).

Status (2026-11-01): AG-51 done. `deploy/helm/aikoql`: two replicas of
one binary — the lease decides who serves writes, the other follows
over replication TLS (plaintext replication is loopback-only by
policy, so a pre-install hook Job generates a CA + one shared serving
cert whose SANs cover both pods' DNS names). Per-pod PVCs, lease-only
RBAC (get/create/update), probes on /metrics, Streamable HTTP MCP with
the auth Secret's token, OAuth armed by an optional admin-password key.
The README carries the PVC snapshot playbook (snapshot the leader's
PVC; restore = PVC from snapshot + a one-off pod). The gate
`scripts/kind-failover-e2e.sh` (nightly, benchmark.yml): kind cluster,
chart install, remember through the leader, kill the leader pod, the
standby promotes and serves the writes, and an OAuth sign-in issued
before the kill keeps authorizing the new leader — this closes the
"real k8s pod" open item (the MRFC-0080 certs covered a real
apiserver via envtest, not a pod). Laws pinned by tests/helm_chart.rs.

Status (2026-11-02): the last "not yet" landed — a PR-CI job
(`ci.yml` `helm`, ubuntu-latest, azure/setup-helm@v4) runs
`helm lint deploy/helm/aikoql` + `helm template aikoql
deploy/helm/aikoql` on every PR, so a broken template fails at push
instead of waiting for the nightly kind gate (which now only
re-renders on schedule). Pinned by RED
`ag51_helm_lint_render_is_wired_into_pr_ci`; the real lint/render
verified against alpine/helm (0 failures, 239 rendered lines).

**AG-52 (P2) Login backoff survives restarts** (in-memory today) and a
per-IP layer behind proxies (`X-Forwarded-For` trusted only from a
configured proxy).
- *Status (2026-11-01): done.* The fail state persists to
  `<db>.login-backoff.json` (wall-clock lock-until, written atomically on
  change, reloaded at serve; a torn file reads as no state — fail open).
  The per-IP layer mirrors the doubling shape (20 free failures — an
  address is shared) and sits in front of the credential check on
  `/api/login` and the OAuth sign-in, so a username spray is refused
  before argon2 runs. `X-Forwarded-For` is trusted only from
  `--trusted-proxy IP` (`server.trusted_proxy` / `AIKOQL_TRUSTED_PROXY`);
  the client address is the last XFF hop when the peer is the proxy, the
  peer otherwise. Per-IP state is in-memory (a restart resets it;
  per-login state persists). Gates: `ag52_login_backoff_survives_restart`,
  `ag52_per_ip_backoff_behind_trusted_proxy`.

### Phase E — ecosystem and proof

**AG-60 (P1) Framework adapters.** CrewAI and LangGraph exist
(`sdk/python/.../adapters`). Add LlamaIndex, OpenAI Agents SDK, Vercel AI
SDK (TS), each as a memory/store provider over `recall`/`remember`.
*Gate:* each adapter's upstream example passes against a live server in
CI.

Status (2026-11-01): AG-60 done. Three adapters, each gate-shaped as
its framework's upstream example against a live server:
`python/aikoql/adapters/llamaindex.py` (AikoqlMemory, a BaseMemory —
each ChatMessage is one KO as model_dump JSON; `get()` recalls the
most relevant prior messages by text; the gate drives
`SimpleChatEngine.from_defaults(memory=...)` with a MockLLM and
re-reads the whole history through a fresh instance), `openai_agents.py`
(AikoqlSession, the SDK's Session protocol — one KO per conversation
item, latest-N `get_items`; the gate runs `Runner.run` twice with an
offline Model stub and asserts turn 2 saw turn 1), and the TS
`src/vercel-ai.ts` (`createAikoqlTools` — remember/recall/forget as ai
v7 tools; the gate asserts the tool contract and round-trips
`execute()` against the live server). Both python adapters duck-type
the two client shapes via `adapters/_client.py` (embedded positional vs
MCP kwargs, the `{"results": ...}` unwrap, the `text_only` vs `text`
fusion spellings); their re-exports are lazy (PEP 562) so
`import aikoql` never drags the frameworks in. CI pins:
llama-index-core 0.14.25 + openai-agents 0.24.0 (python-sdk job), ai
7.0.159 (ts-sdk devDependency, peer >=7).

Status (2026-11-02): the two "not yet" items landed. Embedded-client
gates (tmp_aikoql) drive both python adapters over the positional
client, and the async session methods offload their sync client I/O via
`asyncio.to_thread` (the langgraph.py shape) — RED
`test_async_session_io_runs_off_the_event_loop` pins every
remember/enumerate_rows/forget call to a worker thread, so the Runner
never blocks the loop on local I/O.

**AG-61 (P0) Public agent-memory benchmark — measured, not claimed.**
- *Status (2026-10-31): harnesses committed; the run stays open.*
  `longmemeval.py` (LongMemEval-S, xiaowu0162/longmemeval at 2ec2a557,
  session-level recall_any/recall_all/NDCG at k=5,10, no LLM judge) and
  `memory_cells.py` (contradiction / provenance / time travel, n=100,
  every rate 1.0 — `benchmarks/results/memory-cells.json`). The harness
  is fence-aware (AG-33) and shardable (`--shards/--shard` +
  `merge_lme_shards.py`, validated on a 2-question micro-run). The full
  run is not yet done: the store path embeds per turn on scalar CPU
  (~1.3 s/turn measured), so 115k turns is ~40 h single-process; AWS was
  ruled out (the project's FREE plan caps instances at 2 vCPU). Next: run
  it sharded (8 shards ≈ 5 h on the dev Mac mini), commit the result,
  then LoCoMo and the comparison products.
- Run published long-term-memory benchmarks (e.g. LongMemEval, LoCoMo — the
choice recorded with versions) against aikoql and the agent-memory
products at default settings; publish with the REPORT.md candor. Adds the
cells the field does not measure: contradiction handling, provenance
completeness, time-travel correctness. *Gate:* reproducible harness in
`benchmarks/`, results committed with the toolchain and model versions.

**AG-62 (P2) Hosted trial path.** One-command public trial (the verified
Cloudflare-tunnel recipe, AGENTIC-QUICKSTART) plus a container template
for common PaaS — the prerequisite for any hosted offering.
- *Status (2026-11-02): done.* `scripts/public-trial.sh <db>` is the
  AGENTIC-QUICKSTART tunnel recipe in one command: cloudflared quick
  tunnel to the loopback serve surface, `AIKOQL_PUBLIC_URL` = the tunnel
  URL, a generated admin password when none is set, connector URL printed,
  both processes torn down on exit (gate: `scripts/e2e-public-trial.js`,
  shimmed cloudflared/aikoql-mcp asserting the wiring — the real-tunnel
  path was claude.ai-verified in the OAuth work). The PaaS template is
  `deploy/docker-compose.yml`: the existing image plus a cloudflared
  sidecar sharing its network namespace (`network_mode: service:` — the
  loopback-only HTTP surface stays reachable without TLS keys, and the
  tunnel front-ends HTTPS so logins never cross in plaintext); the file
  passes `docker compose config -q` (Docker Desktop).

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
