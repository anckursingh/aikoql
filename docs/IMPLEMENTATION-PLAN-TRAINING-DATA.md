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
`docs/training-data-architecture.md` §3/§7). (Corrected at T-06:
`compile_context` **is** on the MCP surface — `tools/agent_knowledge.rs`;
the adapter calls it directly.) T-05's builder now targets
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

T-05 shipped 2026-10-03 — `generators/query.py` (`build_queries`) emits
TEXT aikoql against the compiler's real grammar and
`validation/execution.py` (`verify_scenario`) is the oracle: every
generated query passes compile → plan → execute → scenario-match (the
design's 6-point acceptance; auth and evidence join in T-08/T-10).
Grammar pins from the recon: string literals are double-quoted with no
escapes, MATCH predicates address properties only, TRAVERSE is
outbound-only with one rel_type per clause (same-rel paths compile as
one DEPTH-n query, mixed-rel paths chain one query per hop), and a
traverse query must project a field (RETURN * after TRAVERSE comes
back as `{"results": []}`). Fail-closed rendering: a quote in a value,
negative/scientific numbers, non-ident or keyword names, or a KO with
no scalar anchor skips the scenario — a bad query is never emitted, so
the compile gate stays green. RED archived as `t-05-query-builder`
(22 tests blocked). GREEN 83/83: the live cell seeds the 3-service KB
(including a mixed-rel chain) and proves 12/12 generated queries
compile over the spawned MCP server — compile rate 100%. The conftest
schema example was fixed to double-quoted literals (the single-quoted
form does not lex).

T-06 shipped 2026-10-03 — `context/adapter.py` (`compile_context`)
wraps the server's Context Compiler through the public client:
`call_tool("compile_context", ...)` maps the envelope into the
schema's context shape (entities/facts/relations verbatim, evidence =
deduped fact evidence in package order); server errors
(ACCESS_DENIED) propagate, never masked as empty rows. No Python
retrieval logic — the arch assertion holds structurally (the unit fake
exposes ONLY call_tool). The T-01 recon's §7 "no context-compiler
tool" finding was **wrong**: `compile_context` has been on the MCP
surface since MRFC-0070-A6 (`tools/agent_knowledge.rs`) — no server
change was needed. Two live findings: (1) over TCP every authenticated
connection gets agent_id "tcp-agent" (`transport.rs`) — the subject
name is connection-invariant, so the TCP denial boundary is the TENANT
(`tcp_tenant_isolation_across_tokens`), and the unauthorized cell
spawns two tokens in different tenants; (2) staleness is the
IR-version boundary — the compiler reads the live ir_json and its
5-min cache is fingerprint-keyed (CTX-003: update → new fingerprint →
old fact gone). RED archived as `t-06-context-adapter` (11 tests
blocked). GREEN 95/95.

T-07 shipped 2026-10-03 — `generators/answer.py` (`build_answer`)
certifies the scenario's expected answer against the compiled context:
the claim must trace to a fact statement (substring match — the
deterministic ceiling; semantic equivalence is the T-15 model's job)
and every supporting fact's evidence must be present in the context's
evidence rows, or the example is REFUSED (None) — an unsupported claim
is never emitted. `validation/grounding.py` (`validate_grounding`)
enforces the same claim→context→evidence trace fail-closed in both
directions of `labels.grounded`, and `expected.evidence_ids` must trace
exactly to the supporting evidence (identity = the canonical sort-keyed
JSON of the evidence dict, shared between generator and validator).
RED archived as `t-07-grounding` (15 tests blocked). GREEN 110/110,
incl. the hypothesis law "generator accepts ⇒ validator accepts" and a
live cell proving the answer traces to the compiled evidence over the
spawned MCP server.

T-08 shipped 2026-10-03 — `scenarios/temporal.py` emits version
questions over REAL version intervals (design ph 6): one scenario per
changed property per version, the question names the version's own
commit month and `as_of` is the real `commit_ts` — March ⇒ v1, August
⇒ v2; unchanged properties earn no later question and same-month
label collisions are skipped (first emission wins).
`scenarios/provenance.py` emits one scenario per scalar property
citing REAL evidence (design ph 7). Four recon findings: trace
`commit_ts` is the PACKED HLC (`(millis << 16) | counter`, decode
with `>> 16` before AS_OF); kernel evidence confidence is f32
(fixtures use f32-exact values); evidence is kernel-managed
(`remember` rejects the extension — `observe` is the seed; `trace` is
McpClient-surface only, reached via `Agent._backend`); and **evidence
has two real shapes** — canonical kernel entries
(source_artifact/method) vs compiled IR Evidence rows
(document_id/extractor, `extractor` required) — so the provenance
generator cites the COMPILED shape and skips canonical entries
(fail-closed seam: an accepted example's evidence_ids must trace to
context rows). RED archived as `t-08-temporal-provenance` (28 tests
blocked). GREEN 138/138, incl. both hypothesis laws and live cells
for real version intervals and both evidence surfaces over the
spawned MCP server.

T-09 shipped 2026-10-03 — the uncertainty family (design ph 8), three
generators over one shared answer-format module
(`scenarios/answer_formats.py`, no local imports): UNKNOWN: refusals
(`scenarios/unknown.py`), AMBIGUOUS enumerations
(`scenarios/ambiguity.py`) and CONTRADICTED answers
(`scenarios/contradiction.py`). The existing labels block
{grounded, answerable, ambiguous, contradictory} is the
machine-readable label carrier — `build_answer` now returns `labels`
on every family (the T-07/T-08 exact-dict assertions were extended,
not weakened). Uncertainty never becomes a false positive at four
layers: generation (an existing name/property is never "unknown";
ambiguous pairs need distinct values and parse-safe ones;
contradiction input must match the kernel's Conflict record), the
answer generator (unknown refuses when the context actually knows the
missing name; ambiguity/contradiction refuse unless EVERY candidate
value fully traces), the oracle (`verify_scenario` proves absence —
no row carries the property — or recovers both sides), and the
validator (label semantics; the contradiction branch rejects answers
that dropped the conflict metadata). Contradictions preserve the
kernel's Conflict metadata verbatim: both claim koids plus the
conflict koid and its resolution state (read from the live envelope's
`extensions` or the operator-shaped record), never picking a side.
Two new task types (`ambiguity`, `contradiction`) join TASK_TYPES.
RED archived as `t-09-uncertainty` (3 collection errors, exit 2 —
the modules were missing). GREEN 183/183, incl. the ambiguity
hypothesis law, three live cells (real absence, a real same-name
pair, a real `contradict` Conflict), and the unit/live seam for the
conflict envelope.

T-10 shipped 2026-10-03 — authorization scenarios (design ph 9)
through the real ACL path. `authorization_scenarios(kos, decisions)`
(`scenarios/authorization.py`) consumes the kernel's own policy
evaluations — records of (principal, action, resource_type) with the
live verdict and, for denials, the kernel's reason — and pairs each
decision with every KO of its resource type: the question names the
object ("May reader read the service whose name is settlement?"), the
answer is the machine-readable verdict (ALLOWED:/DENIED: prefix)
preserving the reason verbatim (`"Denied by policy: KOID"`). The
engine never re-derives a verdict: `verify_scenario`'s authorization
branch recovers the anchor KO and re-evaluates policies live
(`evaluate_policies`), asserting the verdict prefix agrees — and
malformed decisions, unknown actions, denials without a reason,
decisions over types with no KOs and anchors that would corrupt the
question are skipped. `validate_grounding`'s authorization branch
requires the verdict prefix, the `policy.authorization_required`
flag, verdict-shaped labels and the generic grounded trace — plus the
leak rule: a DENIED example's context may carry the decision fact and
nothing else that names the denied object, so unauthorized knowledge
never reaches the dataset context. `Scenario` gains `subject`/
`action` (additive, defaulted); `build_queries` routes authorization
through the anchored-match helper; `build_answer` needs no branch.
Two live seams found and pinned: **policy KOs store `action` in the
enum's Debug spelling** ("Read"/"Write"/... — `evaluate_policies`
compares against `format!("{:?}", action)`, so a lowercase deployment
never matches) and **the default is deny** — with no matching policy
the reason is "No matching policy found" and `allowed` is false, so an
ALLOWED verdict requires an explicit Allow policy
(`docs/training-data-architecture.md` §14). RED archived as
`t-10-authorization` (1 collection error). GREEN 198/198, incl. the
live cell: a real Deny policy + a real Allow policy deployed, the
kernel's own evaluations denied and allowed, scenarios emitted from
those verdicts, the oracle re-checking the live engine, and the leak
rule proven against a denied object.

T-11 shipped 2026-10-03 — knowledge-level splitter + canonical dataset
writer (design ph 13+14). `assign_splits(examples, seed, ratios)`
(`dataset/splitter.py`) assigns every example to train/val/test by its
`split_key` alone — a seeded stable hash of the key — so
near-duplicate questions (template variants of the same fact) sharing
a key can never straddle a holdout under any seed or input order
(FZ-T7), and assignment is a pure function of (split_key, seed):
reordering or re-shuffling the example list cannot move an example
(determinism law 3). The splitter also reports cross-holdout
violations: example pairs in different splits sharing any
`expected.koid` (recorded, not raised — the §26 leakage gate counts
them at T-12). `write_dataset`/`read_dataset`
(`dataset/writer.py`) are the publication boundary: canonical
single-line `to_json` sorted by example_id per split, temp-file +
`os.replace` atomicity (a reader never sees a half-written file),
stale temp files from an interrupted run swept at start, and
manifest.json written LAST — its presence is dataset visibility —
carrying per-split count/file/sha256 plus example_count and the
identity fields; `created_at` is an explicit operator parameter so
the same inputs regenerate byte-identical output. `read_dataset`
verifies manifest + per-file sha256 + counts and refuses any
tampered/truncated dataset via a new `DatasetError` (fail-closed,
FZ-T2). FZ-T6 found a real seam: `str.splitlines()` splits on
U+0085/U+2028/U+2029, which `ensure_ascii=False` JSON emits raw
inside strings — the reader splits on `"\n"` only. RED archived as
`t-11-split-writer` (2 collection errors). GREEN 219/219.

T-12 shipped 2026-10-03 — dataset validator + gates + CLI (design ph
15, §26). `validate_dataset` (`dataset/gates.py`) enforces all eleven
§5 gates fail-closed; `publishable` is True only when every EVALUATED
gate passes — skipped (no db: compiler/execution/scenario_match; no
reference: determinism) and disabled (secret_scan: false) gates never
veto. Static gates: schema (models.validate), grounding + evidence
coverage (one `validate_grounding` pass, two counts), authorization
(flag-mismatch XOR tooth), secrets (fixed local pattern set — the
ingestion secret-filter binds at corpus time, T-14 — with the FZ-T3
config rule: absent `secret_scan` means ON, explicit `false` is the
only way off), leakage (the split assignment is RECOMPUTED from the
manifest seed — recorded placement must agree AND cross-holdout koid
pairs must be zero), duplicates (rate bound from the config). Live
gates run with `db`/`token`: compiler = raised aikoql, execution = no
`results`, scenario_match follows the ORACLE's rule — hop TARGETS
recovered, because a TRAVERSE result never carries the source KO
(RowSet::Traversal, probe-pinned against the spawned server). The
config parser (`dataset/config.py`, FZ-T3) merges one YAML/JSON file
over fail-closed defaults; unknown keys, wrong types, non-positive
ratios and non-bool `secret_scan` raise `DatasetError`. The CLI
(`cli.py`, `[project.scripts] aikoql-training`): `snapshot` /
`generate` / `validate` / `stats` / `export`; generate is the
end-to-end pipeline on a live fixture DB — seed two services + a
DEPENDS_ON edge, capture the snapshot, factual + relation scenarios,
every query proven through the oracle, context compiled per question
through the server Context Compiler over a mocked-ir
KnowledgeSnapshot, answers certified (refused examples are never
emitted), examples split by **koid-component keys** (the sorted koid
set — any two examples sharing a koid share a bucket, so cross-holdout
pairs are impossible by construction and the leakage gate verifies
it), written canonically, re-run to a scratch dir to prove
byte-identical regeneration, validated — exit 0 iff publishable. The
leakage gate earned its keep during development: the first generate
run grouped relation examples by `koids[0]`, splitting the
settlement→checkout component across holdouts — the gate caught it,
the key rule fixed it. §6's training-data.yml promise stays deferred
to the T-14 corpus (its legs need the POC artifact set; a red gate
never enters CI — CI-04). RED archived as `t-12-gates-cli` (3
collection errors). GREEN 256/256.

T-13 shipped 2026-10-03 — observability + errors + benchmark (design
ph 16, §27/28). The §28 typed error model: `TrainingDataError` grows
four optional category fields (stage/scenario/code/example_id) with a
JSON-serializable `to_info()`; pipeline raises carry the ones their
stage knows (oracle_failed carries stage+scenario+code, split_leakage
stage+code) while the schema raises carry none — the categories are
the pipeline's observability surface, not retrofit noise. The §27
`Metrics` accumulator (counts + derived rates; an undefined rate is
None, not zero) is wired through generate with a `--metrics` file;
the no-sensitive-content rule is structural — every leaf value of a
metrics dict is a number under a fixed key name, so no question,
answer, fact statement or KO text can land in one.
`scripts/benchmark_dataset.py` runs the full generate pipeline
against a live server and reports the three cells as one JSON object:
throughput (wall seconds, examples, examples/second), rates (the
pipeline's derived rates) and size (dataset bytes, per-split counts)
— laptop scale by design, the corpus-scale cell arrives with T-14.

**The new tests caught a real hole in the T-12 split-key rule.** The
koid-set join gives factual `{s}` and relation `{s,c}` DIFFERENT
keys for the same knowledge component — fresh HLC koids drew
straddling buckets on live runs (2 cross-holdout pairs under seed 0;
the T-12 greens were bucket-lottery luck, a flake-by-construction the
T-13 additions surfaced). Fixed at the root: `component_ids`
(union-find over the edges, root = the component's min koid) in the
splitter; the builder stamps the component root as the split_key, so
every example touching a knowledge component shares ONE key and
cross-holdout pairs are impossible under every seed. The regression
pins both sides: component keys are violation-free across a 50-seed
sweep; the old set-join keys straddle some seed (the gate's teeth,
again). The T-12 paragraph's "the key rule fixed it" claim is
corrected by this paragraph. RED archived as
`t-13-observability-errors-benchmark`. GREEN 270/270.

T-14 shipped 2026-10-03 — AcmePay POC corpus + eval set + mutation leg
(design ph 17, §35–37). `scripts/generate_corpus.py` seeds the §35
AcmePay graph per slice — 24 services, 6 teams, 12 persons, 8 accounts,
4 regions; edges OWNS 24, DEPENDS_ON 23, WORKS_IN 12, IN 8 — then runs
the scenario families over the live graph: factual, relation,
multi-hop, temporal (three services versioned on the same KOID after a
real-time gap so AS_OF can distinguish the versions), unknown,
ambiguity, contradiction (one service contradicted through the raw MCP
tool, preserving the Conflict KO), authorization, provenance. The
oracle gate refuses anything the live server cannot answer; the target
is reached in seed 0 alone (~507 examples/slice); determinism is pinned
koid-free (question multiset + task-type histogram equal across two
independent servers), so the 10K seed sweep is CI work. The eval set
(`validation/eval_set.py`, E1–E9 as machine-checkable cases) rides the
corpus, and `scripts/mutation_leg.py` kills validator mutants (the §37
leg: tampered manifest, planted secrets, dropped facts, forged ids —
each must fail its gate and leave `publishable=false`).

**Three traps, all fixed at the root.** (1) `remember()`-with-koid
replaces caller-created edges wholesale (kernel semantics) — seeding
must link AFTER any versioned re-remember or the relationship index
silently orphans the edges and TRAVERSE goes empty; the corpus orders
the slice accordingly. (2) The test fixture fed server stderr into an
undrained pipe: the tantivy commit storm after seeding fills it, the
next handler blocks on its own log write before ever answering
initialize, and the validator's connect times out — logs now go to a
file (no backpressure, CI-15 diagnostic kept). (3) The manifest sha256
check raised out of the validator, so a tampered dataset produced no
report at all — integrity is now a gate and the other gates
(secret_scan included) still run over the tampered content, so the
security test's planted secret is caught and reported. Multi-hop emits
same-rel paths only: the example contract stores ONE query, and a
mixed-rel path's per-hop queries could never satisfy the
scenario_match gate's koid recovery. RED archived as
`t-14-corpus-eval-mutation`. GREEN: corpus 5/5, full training suite
288/288, arch hygiene OK. The §6 CI wiring (`training-data.yml`,
fast-exit per the CI-03 pattern) lands with this commit set — the
"lands at T-12" note above is superseded: the workflow needs the
corpus scripts it runs.

T-15 shipped 2026-10-04 — fine-tune run + scorecard (design ph 18–19,
§32/33/34). `src/aikoql_training/scorecard.py` computes the six
metrics over a split from `scripts/finetune.py predict` records —
query_compile_rate (the live `compiled` flag wins over the E3 static
head check), ko_recall / ko_precision (the oracle targets rule:
`koids[1:]` for TRAVERSE), groundedness (the predicted answer re-run
through `validate_grounding` — the T-07 deterministic ceiling),
refusal_rate (UNKNOWN: prefix, false refusals counted in detail),
secret_leak_rate (the gates' `_SECRET_PATTERNS`, never a second list);
an example with no prediction record fails everywhere.
`src/aikoql_training/inference.py` is the §40 prompt/parse seam — two
skills per example (question → `QUERY:` aikoql, question+context →
answer or `UNKNOWN:` refusal), completion-only labels with -100
prompt masking. `scripts/finetune.py` LoRA-tunes
Qwen2.5-0.5B-Instruct (r=4, all-linear, fp32 — the 1650's 4 GB holds
the 0.5B base, `--device` defaults to cuda when torch sees the GPU)
and
predicts live against the corpus server (each query compiled+executed
for real retrieval numbers); `scripts/scorecard.py` writes the
artifacts under `training/artifacts/scorecards/` — the design law is
enforced as code: `train` refuses to start without a scorecard
artifact. Baseline (raw model, live): compile 0.00, recall 0.00,
precision 0.00, groundedness 0.33, refusal 0.00, leak 0.00 over 24
test examples; finetuned (600 rows, one epoch, live): compile 0.46,
recall 0.375, precision 1.0, groundedness 0.33, refusal 0.00, leak
0.00 — the model learned the query format, retrieval is exact when
it compiles, and the refusal skill needs more data (a 600-row POC
ceiling, not a design gap).

**Three traps, all fixed at the root.** (1) A one-slice corpus can
hash every component key into the train bucket under the pinned
split seed — the test split is empty, predict writes zero records
and the scorecard never forms; the laptop POC seeds three slices
(18 component keys → 24/24/1802), and the CI 10K sweep was never at
risk. (2) A stale Hugging Face OAuth token poisons every download —
an expired `refresh_token` turns even public model repos into 401
"Repository Not Found"; `huggingface_hub.logout()` clears it and
anonymous access works. (3) A `+cpu` torch wheel leaves a present
GPU invisible (`torch.cuda.is_available()` false) and pip skips the
same-version swap — `--force-reinstall --no-deps` against the cu126
index is the fix, and the first CPU training attempt (0 steps in 20
minutes) made the GPU the only sane path. RED archived as
`t-15-scorecard`. GREEN: training suite 294/294.

T-16 shipped 2026-10-04 — the inference wrapper (design §40
end-state). `src/aikoql_training/chat.py` is the thin
question → query → context → answer path, seam-for-seam over the
validated pipeline: the two model skills run through the §40
prompt/parse contract (`build_query_prompt` / `build_answer_prompt` /
`parse_model_reply`) around ONE live `aikoql()` call — the context
statements come from the query results only, so no retrieval can
bypass the oracle. Both seams (`generate`, `run_query`) are
injectable, which is what makes the path testable without a model;
`scripts/chat.py` is the POC chatbot wiring the real model
(T-15 base + LoRA adapter, `--device` cuda default), a live server
and an interactive loop. Every refusal is fail-closed and
machine-readable (the T-09 UNKNOWN: format): no query produced,
query failed to compile/execute (the exception path), query returned
no results, and the model's own UNKNOWN: refusal passes through.
Live smoke on the seeded corpus server: a grounded question
compiled, retrieved and answered end-to-end; an out-of-knowledge
question hallucinated a query, the compile failed and the wrapper
refused — the exact fail-closed behavior the RED column demands.
RED archived as `t-16-chat`. GREEN: training suite 302/302.

First CI round (post-push): every `training-data.yml` run died at 0 s
with "workflow file issue" — the inline Python wait-loops inside the
two corpus jobs sat at column 1, which ends a YAML block scalar and
makes GitHub reject the whole file; the workflow had never parsed,
let alone run. Fixed at the root (bodies indented under the block
scalar) plus a gate tooth: arch-hygiene workflow test 6 rejects any
column-1 line inside a workflow body (RED archived as
`t-16-ci-workflow-parse`), so the class cannot recur silently. The
first genuinely executed run is the T-16 push that carries the fix.

Second CI round (run 37188359354): the first real run failed on the
fresh-runner install class. unit/determinism/integration died at the
Install step with exit 127 — `export VIRTUAL_ENV` does not put
`.venv/bin` on PATH, so the bare `maturin develop` command was never
found (it works on a laptop only because the venv is activated);
fuzz-estate never installed the SDK at all, so `test_scenarios.py`
died at `from aikoql import Agent`. gate-teeth passed — the mutation
leg only needs the pure-Python package. Fixed by calling
`.venv/bin/maturin develop` in all four Install steps and adding the
SDK install to fuzz-estate, plus a gate tooth: arch-hygiene workflow
test 7 rejects a bare `maturin`/`pytest` body command inside any
venv-creating job, scoped per job so release.yml's legitimate system-
python `maturin build` is untouched (RED archived as
`t-16-ci-install-path`).

Third CI round (run 37189254003): the install-path fix itself had the
same class one level down — the corrected steps said
`.venv/bin/maturin develop` *after* `cd crates/sdk/python`, and a
relative path stops resolving after a `cd`, so all four Install steps
died at exit 127 again with "No such file or directory". Fixed by
anchoring the call to `"$GITHUB_WORKSPACE/.venv/bin/maturin"`, plus a
gate tooth: arch-hygiene workflow test 7b rejects a relative `.venv/`
path after a `cd` in the same run block, per block (each `run:` is its
own shell, so a `cd` only poisons the block it lives in), with
`$GITHUB_WORKSPACE`-anchored paths always allowed (RED archived as
`t-16-ci-relative-venv`). The RED run also exposed a harness bug: the
7b check tested `if bad=$(... | awk ...)`, which is true whenever awk
exits 0 — even with no output — so every venv-creating job was flagged
with an empty block; fixed to capture-then-test (`[ -n "$bad" ]`).

Fourth CI round (run 37190350754): unit/fuzz-estate/gate-teeth green —
the install class is closed. determinism/integration died one step
later, in the corpus cells: `training/scripts/generate_corpus.py` runs
with the venv python and imports `aikoql_training`, which pytest finds
via `pythonpath = ["src"]` but a bare script never does (the laptop
venv had the editable install all along, the fresh runner does not).
Fixed by `pip install -q -e training` in every Install step whose job
runs a training/scripts entry, plus a gate tooth: arch-hygiene
workflow test 7c requires the package install in exactly those jobs
(RED archived as `t-16-ci-missing-package`).

### Phase B — model experiments (design phases 18–19)

| id | milestone | RED | GREEN |
|---|---|---|---|
| T-15 | fine-tune run + scorecard (§32/33/34) | scorecard metrics absent | LoRA/FT script for a 0.5B-class open model; eval harness computes query_compile_rate, KO recall/precision, groundedness, refusal rate, secret-leak rate; results committed as artifacts |
| T-16 | inference wrapper (§40 end-state) | question→query→context→answer path fails against the eval set | thin `chat.py`: model (intent/query) → AIKOQL → context → model (grounded answer); refusal/unknown paths; the POC chatbot |

### Phase C — PR9 review gaps (P0.2/TDD-01 + the metric/coverage estate)

| id | milestone | RED | GREEN |
|---|---|---|---|
| T-17 | schema v2: structured semantic target (P0.2/TDD-01) | intent is an opaque string, no plan anywhere | `plan_of`/`policy_of` derivations: (intent, entities, requirements, plan) with closed role/op sets, temporal as_of, policy ACL pair; assemblers share one code path |
| T-18 | plan→renderer seam | query text built inline with plan derivation | a renderer turns the semantic plan into the query_target aikoql text; plan derivation and rendering testable apart |
| T-19 | leakage dimensions | split_key straddles component boundaries (mixed-cardinality koid sets) | component-level split key + ambiguity group union; hard canonical-question tooth; identifier/normalized/answer/relation-pattern reported as diagnostics (arch §23) |
| T-20 | structured JSON model protocol | model speaks free text, parseable only by brittle prefixes | shipped: query/answer JSON shapes, raw_decode+strict schema, ModelOutputError fail-closed, refusal/grounding as fields; bare-prose fallback killed; finetune emits JSON completions (arch §24) |
| T-21 | model-output fuzz | parser accepts garbage silently | shipped: FZ-10 family pins + arbitrary-text properties over both parsers and the chat seam; duplicate keys/missing fields/deep nesting now typed refusals (arch §25) |
| T-22 | claim-level grounding | grounding checked once per answer | shipped: build_answer emits one claim per supporting fact, expected.claims schema-optional, validate_grounding walks claims (dangling/forged/uncovered/union-mismatch all fail); examples without claims keep the answer-level trace (arch §26) |
| T-23 | grounding mutation fuzz | grounding validator never sees adversarial inputs | shipped: FZ-07 mutant matrix over a valid claims-carrying example — every claim/evidence-breaking mutation fails (forged ids, dropped evidence, swapped facts, dangling claims, label flips incl. answerable/ambiguous on grounded), grounding-external fields stay accepted (boundary pins), answer/statement mutation properties (arch §27) |
| T-24 | unknown precision/recall | UNKNOWN: refusals unmeasured | shipped: refusal trio replaces refusal_rate — unknown_recall (refused/unknown), unknown_precision (refused over all UNKNOWN: answers), false_refusal_rate (over answerable), each None when its denominator is absent from the split; committed artifacts migrated, artifact test accepts None (arch §28) |
| T-25 | live-oracle authorization | authorization scenarios never run against the live server | shipped: policy carries the machine-readable verdict (subject/action/resource/decision/reason), schema fails closed on it, grounding cross-checks verdict⇔decision + preserved denial reason, and `verify_authorization_examples` re-proves every committed example against the live `evaluate_policies` — corpus aborts fail-loud on disagreement; mutation leg registers the four §26 authorization mutants per (file, suite) (arch §29) |
| T-26 | per-capability scorecard | six aggregate metrics hide capability failures | shipped: compute_scorecard gains by_task (task.type) and by_difficulty cells — same shape as the aggregate, present capabilities only, §27 None-convention per cell; per-example loop extracted as _cell, the artifact script picks the breakdown up via **score (arch §30) |
| T-27 | multi-domain | AcmePay-only corpus can't show schema generalization | shipped: a second synthetic org (NovaEnergy, utilities schema — operator/uptime_pct/site/head/specialty/credit/tariff/grid_code) seeds alongside AcmePay via _DOMAINS per sweep seed; _domain_of discriminates by the disjoint property schema and prefixes per-KO/group/conflict doc ids; org vocabularies disjoint by construction (a shared stem would leak one domain's anchor into the other's denied-object check); temporal/provenance stay acmepay (arch §31) |
| T-28 | held-out orgs | train/test share component keys | shipped: novaenergy is the held-out org — examples carry an org stamp (schema-optional), assign_splits hashes held-out orgs into val/test only, the manifest declares held_out_orgs and the leakage gate recomputes with it (a held-out example in train is misplaced); unknown scenarios anchor on a training org's service and policy docs are per-(principal, action, verdict, domain) so no train context names a held-out entity; 2 §26 mutants (force disarmed / declaration ignored) die in test_gates; temporal/provenance stay acmepay (documented ceiling) (arch §32) |
| T-29 | CI reshape | legs were accreted one failure-class at a time | shipped: the pytest pythonpath=src smuggling is gone — every training-data leg installs `pip install -e training` and the tests exercise the installed package (clean-venv proof: 403/403 with aikoql pulled from PyPI); production scripts (chat.py, benchmark_dataset.py) no longer inject PYTHONPATH; the 10K corpus sweep moved from integration to an event-guarded nightly leg with a weekly cron + the artifact upload; fuzz-estate runs the whole fuzz estate; leakage/security gates stay in the unit leg by design (arch §33) |
| T-30 | architecture hygiene | training code may import private SDK internals without a reviewer noticing | shipped: the deny-anchor boundary file (`scripts/training-import-boundary.txt`) + workflow test 35 pin the import surface — deeper-than-top-level `aikoql.<sub>` imports, underscore-private names pulled through the top level, and storage bindings (redb/rocksdb/rocksdict) all fail the hygiene scan; bare `import aikoql` / `from aikoql import <public name>` stay allowed (arch §34) |

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
