# AikoQL Agentic Test Plan

Sibling of `docs/IMPLEMENTATION-PLAN-AGENTIC.md`; cell ids match its
milestones (AG-xx). Every cell names its RED (what fails on the tip today,
fc1c9f7), its GREEN, and where it runs.

## 1. Standing rules

The launch plan's rules carry over unchanged (`docs/TESTING-PLAN-LAUNCH.md`
§1): real RED first (an assert or a counter that moves), no weakened
assertions, no sleeps for synchronization (park hooks), no timing pins on
shared runners, structural metrics for every performance claim, one
milestone = one commit set, the re-stamp ritual. Three additions for this
plan:

1. **Quality claims ride the agent eval set (§2)** with a committed
   baseline: a retrieval/extraction change states its metric delta
   (recall@k, nDCG@10, extraction F1), measured, in the commit message.
2. **No network in the default suite.** Sampling (AG-12) is exercised with
   a scripted MCP client that answers `sampling/createMessage` from
   fixtures; live-model runs are env-gated (`tests/gated.toml`, with an
   `ungated_by` home in the nightly benchmark job).
3. **Security cells are adversarial corpora, not examples** (§T): every
   injection/isolation cell runs the whole corpus and asserts zero escapes.

## 2. The agent eval set (shared by A, M, R)

`benchmarks/agent-eval/` — seeded, synthetic, versioned (`EVAL_VERSION`),
regenerated deterministically from a seed, so it carries no third-party
data:

| part | contents | gold |
|---|---|---|
| conversations | 200 multi-session dialogues (user ↔ agent), 20–60 turns | per-turn facts (subject, predicate, value, valid-from), the fact a later turn **contradicts** or **updates**, the speaker's authority |
| queries | 1 000 questions over the conversations: single-hop, multi-hop (2 facts), temporal ("before the move"), contradiction-aware ("what is current?"), scope-bound ("what does *this agent* know?") | the KO ids that answer, the expected epistemic status |
| distractors | near-duplicate facts, superseded facts, other users' facts | must not be returned (or must be flagged) |
| injection corpus | 300 documents/tool outputs with embedded instructions (§T) | must stay fenced |

Metrics: recall@{1,5,10}, nDCG@10, extraction precision/recall/F1,
contradiction-detection rate, cross-scope leak count (must be 0),
tokens per `recall` answer. The baseline is committed as
`benchmarks/agent-eval/baseline.json` with the toolchain and model ids.

Published benchmarks (AG-61: e.g. LongMemEval, LoCoMo) run in the weekly
`benchmark.yml` job only — they need downloads and model calls.

## 3. Cells per milestone

### Phase A — agent surface

| cell | RED today | GREEN | runs in |
|---|---|---|---|
| AG-01a core profile size | `tools/list` = 103 tools / 45 455 bytes > 8 192 | core ≤ 8 KB, ≤ 12 tools | `tests/mcp_tools_profile.rs` (unit-fast) |
| AG-01b every full tool reachable | no `tools_more` (method error) | each of the 103 names reachable via exactly one group; `list_changed` notification sent | same |
| AG-01c no silent break | — (guard) | a config without `tool_profile` lists all 103 (existing configs) | same |
| AG-02a single-call recall | no `recall` tool | eval-set recall@10 ≥ the oracle-best of `find_similar`/`memory_search`/`traverse` | `tests/agent_eval.rs` (quick slice, 100 queries) |
| AG-02b token budget | — | answer ≤ `max_tokens` (tokenizer-counted) for every query, incl. huge KOs | same |
| AG-02c epistemic default | — | superseded facts never returned unflagged; contested facts carry their Conflict KO | same |
| AG-03 resources/prompts | `resources/list`, `prompts/list` → method not found | official Python SDK lists, reads, subscribes (event arrives on change) | `scripts/e2e-mcp-http.py` extension |
| AG-04 structured output | no `outputSchema` on any tool | SDK validates `structuredContent` for the core tools | `scripts/e2e-mcp-http.py` |
| AG-05 error shape | bare-string errors exist | hygiene leg: every tool error has `code` + `hint` | `scripts/check-architecture-hygiene.sh` |

### Phase M — memory

| cell | RED today | GREEN | runs in |
|---|---|---|---|
| AG-10a one store | a `memory_store` write is invisible to `find_similar`/`trace` (it is a Markdown file) | found by `recall`, has a `trace`, edits create versions | `tests/memory_store.rs` |
| AG-10b import is idempotent | no importer | importing the same directory twice creates no duplicates; a changed file creates a new version | same |
| AG-11 isolation matrix | no scope concept | users × agents × sessions (4×3×3) — zero cross-user reads; chain precedence deterministic; `promote` audited | `tests/memory_scopes.rs` |
| AG-12a sampling path | `summarize_conversation` never sends `sampling/createMessage` | with a sampling-capable scripted client: a request with the fixed schema; facts land with message-range provenance | `tests/memory_sampling.rs` (scripted client, fixtures) |
| AG-12b no sampling → oracle | — (guard) | without the capability the result equals today's deterministic output byte-for-byte | same |
| AG-12c contradictions never overwrite | — | every eval-set contradiction becomes a Conflict KO; zero silent overwrites | same + `agent_eval.rs` |
| AG-12d extraction quality | F1 of the deterministic path (baseline recorded) | sampling F1 ≥ baseline on fixtures; live-model F1 recorded nightly (gated) | `agent_eval.rs` / `benchmark.yml` |
| AG-13a lineage kept | no consolidation job | every merged KO traces to all its sources | `tests/memory_consolidation.rs` |
| AG-13b quality kept | — | eval-set recall@10 after consolidation ≥ before | `agent_eval.rs` |
| AG-13c nothing deleted | — | consolidation and decay never delete; only `forget` does (audit count unchanged) | `memory_consolidation.rs` |
| AG-14 outcome ranking | experiences rank by similarity only | successful procedures rank first on the procedural slice | `agent_eval.rs` |

### Phase R — retrieval at scale

| cell | RED today | GREEN | runs in |
|---|---|---|---|
| AG-20a ANN on the certified path | 1M vector_recall 17 002 ms (REPORT §Scale); distance computations per query = N | p50 ≤ 50 ms warm, recall@10 ≥ 0.95 vs exact; distance computations sublinear (counter) | `benchmark.yml` competitor-scale (1M), quick 100k cell in perf smoke |
| AG-20b filtered ANN per selectivity | — | recall@10 ≥ 0.95 in each selectivity bucket (50%, 10%, 1%, 0.1%); the exact fallback engages below the threshold (counter) | `crates/engines/vector` proptest + bench |
| AG-20c quantization | f32 only | int8 + exact re-score: recall@10 within 0.01 of f32; index bytes ≤ 30% of f32 | vector unit + bench |
| AG-20d no rebuild on restart | (measure today: rebuild time at 100k) | restart loads persisted segments; build counter = 0 | `tests/vector_persist.rs` |
| AG-21a indexed filter | 1M structured_filter 22 912 ms | ≤ 10 ms p50; rows decoded ≤ 2× result size (counter) | `benchmark.yml` + perf smoke 100k |
| AG-21b planner picks the index | CBO estimates the first Eq only (`cbo.rs:161`) | EXPLAIN shows the index for Eq/range/composite; oracle-equal results | `crates/runtime` tests |
| AG-22 rerank | no rerank stage | nDCG@10 up on the eval set; ≤ 30 ms for 50 candidates (structural: one batched forward pass) | `agent_eval.rs` + bench |
| AG-23 model switch | single model | never mixed-dimension results; migration resumes after SIGKILL (park hook at item N) | `tests/reembed.rs` |

### Phase T — trust

| cell | RED today | GREEN | runs in |
|---|---|---|---|
| AG-30a RFC 8032 | signatures are HMAC-SHA512 (`signing.rs:70`) — RFC 8032 vectors fail | all RFC 8032 §7.1 vectors pass | kernel unit |
| AG-30b external verify | — | an exported chain verifies with Python `cryptography` | `scripts/e2e-audit-verify.py` |
| AG-30c history kept | — | pre-switch entries verify under the old scheme; the switch is a signed chain event | kernel unit |
| AG-31 rotation | rotation only logs (`key_rotation.rs:60`) | rotate under a write load: every object readable; old KEK refused | `tests/kek_rotation.rs` |
| AG-32 tenant usage | always `"default"` (`tenant.rs:75`) | per-tenant counts equal the oracle | kernel unit |
| AG-33 injection fence | no trust class on KOs | the whole injection corpus: zero untrusted text outside its fence; trust class on every `recall` item | `tests/injection_corpus.rs` |
| AG-34 scopes | every grant has scope `mcp` | a `mcp:read` token gets 403 on every write tool (enumerated from the registry, not hand-listed) | `tests/mcp_oauth.rs` |

### Phase X / P / E

| cell | RED today | GREEN | runs in |
|---|---|---|---|
| AG-40 live queries | no `subscribe` on queries | N writers × M subscribers: no missed, no duplicate events (sequence-checked) | `tests/live_query.rs` |
| AG-41 claims | — | each task claimed exactly once; a killed agent's claims expire (lease) | `tests/coordination.rs` |
| AG-50 traces | no OTel | one span per tool call in the e2e run (in-memory exporter) | `tests/otel.rs` |
| AG-51 k8s failover | open item ("real k8s pod") | kind cluster: kill the leader pod, the standby serves writes, an OAuth sign-in survives | `benchmark.yml` nightly (kind) |
| AG-52 backoff persists | in memory | a restart keeps the lock; proxy-forwarded IPs counted only from the trusted proxy | `tests/auth_surface.rs` |
| AG-60 adapters | LlamaIndex/OpenAI Agents/Vercel AI absent | each adapter's upstream example passes against a live server | `ci.yml` SDK jobs |
| AG-61 public benchmark | no harness | reproducible run, results + versions committed; REPORT-style decomposition | `benchmark.yml` weekly |

## 4. CI placement

| tier | what | budget |
|---|---|---|
| PR (`ci.yml`) | all unit/integration cells above except 1M scale and live models; agent-eval quick slice (100 queries); perf smoke adds the 100k AG-20/AG-21 cells under the existing 3× budget | adds ≤ 3 min |
| nightly/weekly (`benchmark.yml`) | 1M AG-20/AG-21, full agent eval (1 000 queries), live-model extraction (gated), kind failover, AG-61 | within the existing benchmark owner |
| release (`release.yml`) | the agent-eval baseline must not regress (gate-5 style, ratio to committed baseline) | — |

## 5. Benchmark schema additions

`result.json` gains an `agent` block next to the existing cells:

```json
"agent": {
  "eval_version": "1",
  "recall_at_10": 0.0, "ndcg_at_10": 0.0,
  "extraction_f1": {"deterministic": 0.0, "sampling_fixture": 0.0},
  "contradiction_detection": 0.0,
  "cross_scope_leaks": 0,
  "recall_tokens_p50": 0,
  "tools_list_bytes": {"core": 0, "full": 45455}
}
```

`tools_list_bytes.full = 45455` is the recorded RED for AG-01a; every
other field's first value is its measured baseline, committed with the
harness (AG-61 wave 2) before any milestone claims a delta.
