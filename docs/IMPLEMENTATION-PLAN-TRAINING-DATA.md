# AikoQL Training Data Engine — implementation plan

Branch: `feature/aikoql-training-data` (created at 271c0e3, the v0.2.2
re-stamp tip of `feature/aikoql-db-launch`).

Review input consumed: `AIKOQL-Training-Data-Engine-Production-Implementation-Plan.md`
("the design", 2026-10-03) — analyzed as senior ML architect; this plan is
the disposition of that analysis. The design's §39 engineering rules 1–10
are adopted verbatim as implementation laws.

Sibling plan: `docs/TESTING-PLAN-TRAINING-DATA.md`.

## 1. What this is (and is not)

The design proposes a **thin, deterministic Training Data Engine over the
existing AIKOQL knowledge platform** — AIKOQL ingests, resolves, retrieves,
compiles and authorizes; the new layer synthesizes validated training
examples; a small model is instruction-tuned on them to serve as a
natural-language interface over the knowledge base.

Scope reconciliation with `no-llm-agentic-substrate` (2026-08-25 decision:
AIKOQL is a knowledge OS *for* agents, not an agentic app): **not reversed**.
The Tiny LLM is a client of the knowledge base — it emits intents/queries/
refusals and reads AIKOQL-provided context; it never owns truth, never
stores enterprise facts in weights, never executes code. AIKOQL remains the
source of truth. This branch changes nothing in the core; it adds a sibling
consumer tree.

### Model strategy (senior-ML disposition)

| decision | ruling |
|---|---|
| pretrain a model from scratch | **out.** A 10K-example synthetic corpus pretrains nothing; no evidence gate exists. |
| fine-tune an existing small decoder | **stage 2, experimental (T-15).** LoRA or full FT on a 0.5B–1.5B open model (Qwen2.5-0.5B / SmolLM2-class). Cheap, local, meets "not a frontier model". |
| frontier API as teacher | **optional, never the truth.** Allowed only for paraphrase/answer polish (§13/§16); AIKOQL wins every disagreement (§31). The POC must ship with zero API calls. |
| the shipped product of this branch | **the dataset engine + the validated POC corpus + the eval set (T-01..T-14).** Training (T-15) and the serving wrapper (T-16) are experimental follow-ons. |

## 2. What the design gets right (accepted as-is)

- **AIKOQL-as-oracle** (§14, §39 rules 4–5): generated queries are accepted
  only through compile → plan → execute → scenario-match → auth → evidence.
  This converts dataset generation from a modeling problem into a
  verification problem. Non-negotiable.
- **Determinism first** (§12, §24): template question generation, seeded
  RNG, content-derived example IDs, no LLM in the critical path.
- **Snapshot model + knowledge-level splits** (§10, §18): entity/relation/
  template/multi-hop/temporal holdouts — the leakage discipline is the part
  toy pipelines get wrong; here it is mandatory.
- **Security S1–S8** (§20): generation consumes already-filtered, authorized
  knowledge through the public client; never storage internals, never
  generate-then-redact.
- **Non-goals** (§5): no second ingestion/RAG/graph/vector/context/auth
  implementation; no training in Rust; no model as knowledge store.
- **Thin Python boundary** (§7, §41): Python owns synthesis/training only,
  and talks to AIKOQL through the supported client (the Python SDK +
  MCP), the same surface production agents use.

## 3. Gaps the design leaves open (resolved here)

| gap | resolution |
|---|---|
| **Scale honesty** — 10K examples from ~100 AcmePay entities are ~90% template variants of the same facts: they teach *form* and refusal behavior, not generalization. | The POC goal is pinned as form-learning + refusal + query discipline. Breadth comes from a seed sweep (N seeds regenerate the KB scenarios with different question/paraphrase picks); the manifest records the seed. No claim beyond that. |
| **Serving path missing** — the design stops at training. | T-16 adds a thin inference wrapper reusing the validated pipeline (question → intent/query → AIKOQL → context → grounded answer + refusal paths). That is the "readymade chatbot". |
| **Evaluation before training** — E1–E9 exist but no scorecard precedes the first run. | T-14 produces the dataset-level eval set (E1–E9 as machine-checkable cases); T-15's first artifact is the scorecard (query_compile_rate, KO recall, groundedness, refusal rate) — no training run without one. |
| **Teacher cost/rate limits** (§16, §29). | Bounded teacher pool; teacher optional at every stage; fallback = the fine-tuned small model itself for later paraphrase bootstrapping. |
| **Phase 0 is mandatory** — the design assumes §3 anchors. Verified at this tip: `crates/ingestion/src/{resolution,embedding,secret_filter,context,pipeline,merge,ingest_dir}.rs`, `crates/compiler/src/{parser,planner,semantic}`, Python SDK `{mcp_client,agent,prepared,pool}.py` + embedded-server test hatch all exist. The exact public surface (snapshot/revision exposure, client API shape, KO serialization) is pinned by T-01's recon doc, never guessed. |
| **Canonical schema detail** (§9). | Add one field: `split_key` (the holdout dimension the example belongs to) — the splitter and the leakage validator both need it; IDs stay content-derived. The `query_target.query` payload is produced against the compiler's real contract (pinned at T-01). |

## 4. Layout

The design's §8 tree is adopted, adjusted to the repo's Python conventions
(the SDK's pyproject pattern, pytest + hypothesis):

```text
training/
├── README.md
├── pyproject.toml                # aikoql-training; deps: aikoql (the SDK), pyyaml; dev: pytest, hypothesis
├── src/aikoql_training/
│   ├── __init__.py  config.py  errors.py  models.py  client.py  snapshot.py
│   ├── scenarios/                # factual, relation, multi_hop, temporal, provenance,
│   │   ...                       # contradiction, ambiguity, authorization, unknown
│   ├── generators/               # question, query, answer, reasoning
│   ├── context/adapter.py
│   ├── dataset/                  # writer, manifest, splitter, mixer
│   ├── validation/               # schema, execution, grounding, leakage
│   └── cli.py                    # snapshot / generate / validate / stats / export
├── tests/                        # mirrors src/ one-to-one + test_determinism.py + test_fuzz_estate(_pin).py
├── datasets/                     # gitignored; manifest+statistics of the POC set committed under artifacts/
└── scripts/                      # generate_dataset.py  validate_dataset.py  benchmark_dataset.py
```

Top-level `training/` (not under `crates/`) — it is pure Python, not a Rust
workspace member. The installed `aikoql` SDK is the only AIKOQL dependency;
the package must NOT import the SDK source tree (sys.path hacks) — the
v02-cert-python-native trap in reverse.

## 5. Milestones

One milestone = one commit set (test RED → feat → docs). The design's
phases are mapped 1:1; ids are T-xx. REDs are archived
(`docs/red-archive/t-xx-*.red.log`) per the P0-1 mechanism; every stream
ends with the dogfood re-stamp tip; the user pushes.

### Phase A — deterministic engine over the oracle (design phases 0–15)

| id | milestone (design phase) | RED (against the current tree) | GREEN (after) |
|---|---|---|---|
| T-01 | recon + canonical schema (ph 0+1) | `tests/test_schema.py` — required fields, unknown fields, types, stable serialization, content-derived example IDs — fail against the empty package | `models.py` typed models; `docs/training-data-architecture.md` records the *discovered* interfaces (SDK surface, MCP query path, KO serialization, auth flow, fixtures); `split_key` in the schema |
| T-02 | snapshot adapter (ph 2) | snapshot tests fail: db identity, knowledge revision, config hash, seed captured; same-state ⇒ same snapshot identity | `snapshot.py` + `client.py` over the real SDK/MCP; manifest reproducibility pinned |
| T-03 | factual + relation scenarios (ph 3+4) | one-KO/one-property/one-question/one-answer; one-hop, inverse, relation-filter, missing-relation tests fail | deterministic generators; expected path stored and validated |
| T-04 | multi-hop scenarios (ph 5) | A→B→C fixture: generated scenario must carry the exact path; fabricated-edge case fails | graph-path generator; no fabricated edges (assertion over real graph data) |
| T-05 | query builder + oracle (ph 10) | expected-query tests fail; the builder's output does not compile | builder emits TEXT aikoql against the compiler's real grammar (recon §3); EVERY generated query passes compile → plan → execute → scenario-match (the design's 6-point acceptance) |
| T-06 | context adapter (ph 11) | context tests fail: required entities/facts/relations + evidence present, no unauthorized/stale data | adapter calls the existing Context Compiler via the client; no Python retrieval logic (arch assertion) |
| T-07 | grounded answers + grounding validator (ph 12) | unsupported-claim rejection fails | deterministic answer generator; validator enforces claim→context/evidence tracing; 100% of accepted examples grounded |
| T-08 | temporal + provenance scenarios (ph 6+7) | March/August version questions; evidence-ID expectations fail | temporal generator over real version intervals; provenance examples point at real evidence |
| T-09 | unknown/ambiguity/contradiction (ph 8) | missing entity/property, ambiguous pair, conflicting-fact fixtures fail | generators emit machine-readable labels; uncertainty never becomes a false positive; contradictions preserve Conflict metadata |
| T-10 | authorization scenarios (ph 9) | authorized vs unauthorized subject fixtures fail | scenarios run through the real ACL path; unauthorized knowledge never reaches dataset context |
| T-11 | knowledge-level split + writer (ph 13+14) | near-duplicate questions split apart; JSONL determinism, manifest, checksums, atomicity, interrupted-run cleanup fail | splitter on holdout dimensions; writer with manifest + sha256 + atomic output |
| T-12 | dataset validator + gates + CLI (ph 15, §26) | validator rejects a poisoned dataset only if each gate is enforced; CLI absent | `aikoql-training snapshot/generate/validate/stats/export`; fail-closed gates: compile < 100%, unauthorized > 0, secrets > 0, invalid schema > 0, leakage > 0 ⇒ publishable=no |
| T-13 | observability + errors + benchmark (ph 16, §27/28) | metrics/error-category tests fail | typed error model (§28 categories), structured metrics (§27, no sensitive content), `scripts/benchmark_dataset.py` (throughput, rates, size; laptop = quick cells) |
| T-14 | AcmePay POC corpus + eval set (ph 17, §35–37) | determinism-across-seeds, leakage, security, E1–E9 dataset checks fail | seeded AcmePay KB (§35 counts), 10K-example generation (seed sweep), eval set, artifacts committed; mutation leg: validator mutants are killed |

T-01 shipped 2026-10-03 — `models.py` fail-closed schema validation
(required/unknown/typed fields, task/difficulty enums, JSON-serializable
content, forged-ID rejection), canonical sort-keyed `to_json`, and
content-derived `example_id` (design §24, sha256 over schema_version +
snapshot + task_type + question + query + scenario_id). RED archived as
`t-01-schema` (ModuleNotFoundError, 21 tests blocked). The recon doc
landed with two findings: **query_target is TEXT aikoql** (tool_aikoql →
`aikoql_compiler::parser::parse`; the design's aikoql-json payload does
not exist) and **no context-compiler tool exists on the MCP surface** —
T-06 must compose retrieval primitives or propose a server change (see
`docs/training-data-architecture.md` §3/§7). T-05's builder now targets
the text grammar.

T-02 shipped 2026-10-03 — `snapshot.py` captures the design §10
DatasetSnapshot record: content-derived `snapshot_id` (database_id +
knowledge_revision = journal_seq:audit_hash from the public health
tool), canonical `configuration_hash`/`source_manifest_hash`, recorded
seed/generator/schema versions, fail-closed on missing identity
inputs. Acceptance pinned: two captures of the same immutable state
produce the same identity (created_at is metadata). RED archived as
`t-02-snapshot` (13 tests blocked). Live cell green over a spawned
aikoql-mcp server. Recon added two findings: **embedded
`Agent.health()` is a stub** (journal_seq/audit_hash are MCP-only) and
**no database identity is exposed** (initialize carries only
serverInfo) — `database_id` is an explicit operator parameter
(`docs/training-data-architecture.md` §4).

T-03 shipped 2026-10-03 — `scenarios/` deterministic generators over
actual knowledge (design §11): `factual_scenarios` (one scenario per
scalar property; nested/blank/None skipped; sorted koid then property)
and `relation_scenarios` (forward + inverse per edge; verb map for the
POC relation types; dangling edges skipped; missing relation → empty).
Every `Scenario` stores the exact `expected_path` used — validated,
never invented. RED archived as `t-03-scenarios` (16 tests blocked).
Live cell green over the spawned MCP server: seeded KOs + DEPENDS_ON
edges, `scan_edges` recovers the edges through the public traverse
surface, and every generated answer equals the live property value.
Recon finding: **the traverse envelope is shape-inconsistent across
surfaces** (MCP returns `{"hits": [...]}`, embedded returns a flat
list) and **hits carry direction "outbound"/"inbound"** —
`scan_edges` normalizes both (`docs/training-data-architecture.md`
§7/§8).

T-04 shipped 2026-10-03 — `templates.py` (the FZ-T4 template engine:
refs escape quotes/backslashes, strip control chars, own the verb
map) and `multi_hop.py` (design Phase 5): every two-edge path
A→B→C yields one scenario with the exact path in `expected_path` and
the intermediate as the answer; paths are built only from the scanned
graph — a fabricated edge never generates. Question grammar pinned by
the RED: first verb base ("What does A own…"), second verb
third-person ("…that depends on C?"). RED archived as `t-04-multihop`
(20 tests blocked). GREEN 63/63: unit + the FZ-T4 hypothesis
escape-round-trip property + the live 3-KO chain over the spawned MCP
server.

### Phase B — model experiments (design phases 18–19)

| id | milestone | RED | GREEN |
|---|---|---|---|
| T-15 | fine-tune run + scorecard (§32/33/34) | scorecard metrics absent | LoRA/FT script for a 0.5B-class open model; eval harness computes query_compile_rate, KO recall/precision, groundedness, refusal rate, secret-leak rate; results committed as artifacts |
| T-16 | inference wrapper (§40 end-state) | question→query→context→answer path fails against the eval set | thin `chat.py`: model (intent/query) → AIKOQL → context → model (grounded answer); refusal/unknown paths; the POC chatbot |

Execution order is the table order — the oracle (T-05) lands before
probabilistic generation anywhere, exactly as the design's §38 prescribes.

## 6. CI integration (when, not now)

`training-data.yml` (fast-exit on `training/**` + workflow paths, per the
CI-03 pattern): unit, fuzz-estate, determinism, integration legs. It lands
**at T-12**, and only after its legs are green locally — a red gate never
enters CI (CI-04 lesson); the arch-gate pins (workflow tests) are registered
in the same commit set. Until then: local-only `pytest` runs.

## 7. Traps (anticipated; T-01 recon validates each)

- SDK import discipline: use the installed `aikoql` package, never a
  `sys.path` insert to the source tree (the v02 cert trap in reverse).
- Windows server lifecycle: embedded/MCP spawn-kill patterns from the SDK's
  `test_agent_embedded.py`; never blanket-kill processes.
- Determinism: dict/set iteration and float formatting — canonical
  serialization (`sort_keys`, stable encodings) everywhere content-derived
  IDs touch.
- Hypothesis: fixed profile / `derandomize` in CI legs; no unseeded RNG in
  tests (the repo's test-env-hygiene rule, Python edition).
- Console encoding: cp1252 kills Python CLI output on Windows — the CLI
  writes UTF-8 explicitly.
- Snapshot drift: dataset regeneration must pin `schema_version` +
  `knowledge_revision` + `generator_version`; a question is never a cache
  key (§30).
