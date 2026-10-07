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

## 12. Temporal and provenance scenarios (T-08)

`temporal_scenarios(histories)` (`scenarios/temporal.py`) emits version
questions over REAL version intervals: one scenario per property per
version whose value changed, the question names the version's own
commit month and `as_of` carries the real `commit_ts` — March ⇒ v1,
August ⇒ v2. A property unchanged since the previous version earns no
later question (it would repeat the earlier answer), and a month-label
collision (two versions in one month asking the same question with
different answers) is skipped, first emission wins. Versions without
properties, non-scalar values and malformed records are skipped; the
generator sorts by `(commit_ts, version)` and never invents an
interval.

`provenance_scenarios(kos)` (`scenarios/provenance.py`) emits one
scenario per scalar property whose KO carries citable evidence:
"What evidence supports that the X of A is <value>?" answered by the
citation of the evidence rows. The oracle branch in
`validation/execution.py` proves the anchor KO was recovered with the
property (the citation is not a property value — evidence realness is
`validate_grounding`'s job via `evidence_ids`).

Four T-08 recon findings, all pinned by live cells over the spawned
server:

1. **trace `commit_ts` is the PACKED HLC** (`kernel.rs` `Hlc::now`:
   `(millis << 16) | counter`) — decode with `commit_ts >> 16` before
   feeding AS_OF, which wants plain epoch millis.
2. **Evidence confidence is f32** — the kernel stores `Evidence`
   confidence as f32, so 0.9 round-trips as 0.8999999761581421 and
   breaks canonical evidence identity; fixtures use f32-exact values
   (0.75).
3. **Evidence is kernel-managed**: `remember` rejects the `evidence`
   extension (`INVALID_OBJECT`) — the honest seed is `observe`
   (`tools/knowledge.rs`), which stamps epistemic Observed. `trace` is
   McpClient-surface only; over MCP `Agent._backend` IS the initialized
   client (the adapter's `_call_tool` path).
4. **Evidence has TWO real shapes** — the seam. The kernel stores
   canonical evidence (`source_artifact`/`method`/`location`/
   `revision`, `kom.rs evidence()`), while the compiled context carries
   IR `Evidence` rows (`document_id`/`page`/`source`/`extractor`/
   `model`/`confidence`, `ir.rs`, serialized with nulls — `extractor`
   is required, so a canonical dict deserialized as IR evidence fails
   with "missing field `extractor`"). Accepted examples ground in the
   context, so the provenance generator cites the COMPILED shape
   (`document (extractor) p.N`) and skips canonical entries — citing
   them into a context that can never carry them would ground nothing
   (fail-closed seam, `test_non_citable_evidence_skipped`).

## 13. Uncertainty and conflict scenarios (T-09)

The uncertainty family (design ph 8) is three generators over one
shared format module, `scenarios/answer_formats.py` (no local imports —
used by scenarios, generators and validation alike):

- `unknown_scenarios(kos, missing)` — a missing entity (a name no KO of
  the type carries) or a missing property (an existing KO without it)
  is answered by an `UNKNOWN:` refusal, `koids` empty for missing
  entities, labels grounded=False/answerable=False. An existing
  name/property is skipped: uncertainty is never a false positive.
- `ambiguity_scenarios(kos)` — same-type pairs sharing a scalar anchor
  value are enumerated as `AMBIGUOUS (n candidates): koid -> value; ...`
  sorted by koid, labels ambiguous=True. The asked property is the
  first sorted common scalar with distinct values; pairs whose common
  properties all agree, or whose candidate values contain an
  enumeration delimiter (`"; "`, `" -> "`), are skipped.
- `contradiction_scenarios(conflicts)` — one example per real kernel
  Conflict record; the answer preserves the Conflict metadata verbatim
  (`CONTRADICTED: vA (claim a) vs vB (claim b); conflict c, resolution
  r`), labels contradictory=True, never picking a side. The claims are
  ordered by the record's `claim_a`/`claim_b`, never the input list;
  claims that do not match the record, identical claims, mixed types or
  records with no shared anchor are skipped.

`Scenario` gains `anchor_prop`/`anchor_value`/`candidates`/`type_name`
(all defaulted, additive). `build_queries` emits one anchored MATCH
whose result set PROVES the uncertainty: no row carrying the property
(unknown) or both candidate values (ambiguity/contradiction) — the
`unknown` branch precedes the koids-present check because missing
entities carry no KO. `build_answer` returns `labels` on every family
(T-07/T-08 exact-dict assertions extended); unknown refuses when the
context actually knows the missing name, ambiguity/contradiction refuse
unless every candidate value traces to an evidenced fact.
`validate_grounding` dispatches on the task type: refusals must not be
marked answerable/grounded and must carry no evidence_ids; the
enumeration families require the family label, the prefix, a full
per-candidate trace, and — for contradiction — the conflict metadata
(the parser is fail-closed: an answer without `"; conflict ..., resolution ..."`
parses to nothing).

One seam found live: **the Conflict KO's `resolution` lives under
`extensions`** on the `get()` envelope (kernel `ops.rs`), while
operator-shaped records carry it top-level — the generator reads both.
`extensions.assertions` (the per-claim authority/evidence/timestamp
snapshots) is not surfaced into examples; the claims' evidence traces
through the compiled context instead.

## 14. Authorization scenarios (T-10)

The authorization family (design ph 9) runs verdict questions through
the REAL ACL path — `evaluate_policies` over the kernel's policy KOs,
never a Python re-implementation of checks. `authorization_scenarios`
(`scenarios/authorization.py`) consumes decision records — the
kernel's own policy evaluations for a (principal, action,
resource_type) tuple: the verdict (allowed bool) and, for denials,
the kernel's reason — and pairs each decision with every KO of its
resource type. The question names the object; the answer is the
machine-readable verdict (ALLOWED:/DENIED: prefix) with the reason
preserved verbatim. `verify_scenario` re-evaluates policies live and
asserts the verdict prefix agrees; `validate_grounding` requires the
verdict prefix, the `policy.authorization_required` schema flag,
verdict-shaped labels, the generic grounded trace, and — fail-closed
on leakage — that a DENIED example's context carries the decision
fact and nothing else that names the denied object.

Three kernel contracts pinned by live probes (kernel.rs
`deploy_policy`/`evaluate_policies`, kom.rs `Action`):

1. **Policy `action` is the enum's Debug spelling.** The policy KO
   stores `action` as text; `evaluate_policies` compares it to
   `format!("{:?}", action)` — "Read"/"Write"/"Admin"/"Evolve"/
   "Delete". A policy deployed with a lowercase action never matches.
2. **The default is deny.** With no matching policy the reason is
   "No matching policy found" and `allowed` is false — an ALLOWED
   verdict requires an explicit Allow policy, never just the absence
   of a Deny.
3. **The verdict is the kernel's, not the operator's.** The engine
   emits what `evaluate_policies` returned; it never derives a
   verdict from the policy text itself (the oracle re-checks the
   prefix against the live engine at validation time).

## 15. Dataset layer — splitter + writer (T-11)

The dataset layer (`dataset/splitter.py`, `dataset/writer.py`) is the
publication boundary between generation (T-03..T-10) and
validation/gates (T-12):

- **Holdout assignment is the split_key's hash.** `assign_splits`
  buckets every example by `sha256(seed:split_key)[0] % total` over
  integer weights (default 8/1/1 train/val/test). Near-duplicate
  questions (template variants of the same fact) share a key, so they
  can never straddle a holdout — structurally, under any seed or
  input order (FZ-T7) — and assignment is a pure function of
  (split_key, seed): determinism law 3 holds by construction. Weights
  must be three positive ints (DatasetError otherwise); an example
  without a split_key is refused, not silently bucketed.
- **Cross-holdout leakage is reported, not raised.** Example pairs in
  different splits sharing any `expected.koid` are returned as
  (example_id_a, example_id_b, koid) violations — the §26 leakage
  gate counts them at dataset validation, the splitter itself never
  moves an example.
- **Publication is atomic and manifest-last.** Split files are the
  canonical single-line `to_json` (models.py) sorted by example_id,
  written to temp names and `os.replace`d — a reader never sees a
  half-written file. Stale `*.tmp*` files from an interrupted run are
  swept at start; `manifest.json` is written LAST (its presence is
  dataset visibility) with per-split count/file/sha256 plus
  example_count and the identity fields (dataset_id, schema_version,
  generator_version, seed, snapshot_id, configuration_hash,
  created_at). `created_at` is an EXPLICIT operator parameter —
  wall-clock in the manifest would break byte-identical regeneration.
- **Reading is verification.** `read_dataset` re-hashes every split
  file, re-counts every split and refuses any mismatch with
  `DatasetError` (new typed error in errors.py — the T-13 §28 model
  extends the set). FZ-T2: a tampered file, a truncated file, a
  tampered manifest or a count mismatch all refuse; arbitrary mutated
  content either refuses or round-trips, never reads silently wrong.
- **FZ-T6 seam, found by the property:** `str.splitlines()` splits on
  U+0085/U+2028/U+2029, which canonical `ensure_ascii=False` JSON
  emits RAW inside strings — the reader must split on `"\n"` only.
  Surrogates (Cs) are outside the text domain entirely (no real KB
  value can carry one — Rust strings are valid UTF-8) and are
  excluded from the adversarial strategy, not handled by the writer.

## 16. Dataset validation — gates + CLI (T-12)

`validate_dataset` (`dataset/gates.py`) enforces all eleven §5 gates
fail-closed. The report shape is
`{publishable, gates: {name: {ok, status, count, detail}}, example_count}`.
Statuses: `passed`/`failed` when evaluated, `skipped` when its inputs
are absent (no `db` → compiler/execution/scenario_match; no
`reference` → determinism), `disabled` when the operator turned it off
(secret_scan: false). **`publishable` is the AND of every EVALUATED
gate — skipped and disabled never veto.**

- **Static gates** run on the artifact alone: schema (models.validate,
  catching `TrainingDataError` — SchemaError and DatasetError are
  siblings), grounding + evidence coverage (one `validate_grounding`
  pass feeding two counts), authorization (task-type vs
  authorization-required flag mismatch = fail, in both directions),
  secrets (fixed local pattern set; the config rule is FZ-T3:
  absent `secret_scan` means ON — an explicit `false` is the only way
  off), leakage, duplicates (id rate bound from the config).
- **Leakage recomputes, never trusts.** The split assignment is
  re-derived from the manifest seed over each example's split_key.
  Two teeth: every example's RECORDED split file must be its
  recomputed home, and the cross-holdout shared-koid pair count must
  be zero. This gate caught a real generator bug in its first run:
  relation examples carried the checkout koid but split on the
  settlement one. The T-12 split_key (all of an example's koids,
  sorted and joined) then claimed cross-holdout pairs "impossible by
  construction" — wrong for mixed-cardinality koid sets, and the gate
  proved it at T-13 (see §17).
- **Live gates** run with `db`/`token` (the §24 client boundary):
  compiler = raised aikoql counts against all three of
  compiler/execution/scenario_match; execution = no `results` in the
  compiled env; scenario_match follows the ORACLE's rule — hop
  TARGETS recovered, not the source. **The reason is runtime, not
  taste: `RowSet::Traversal` rows carry only the reached objects, so
  a TRAVERSE result NEVER contains the source KO** (probe-pinned
  against the spawned server). Single-koid result shapes check the
  koid itself; a hop with any missing target koid fails.
- **Determinism** is a byte gate: the reference dataset's manifest
  must equal the candidate's, then every split file is compared byte
  for byte (the CLI's generate re-run already proved
  regeneration; the gate re-proves it against the artifact).

The CLI (`cli.py`, console script `aikoql-training`) has five
subcommands: `snapshot`, `generate`, `validate`, `stats`, `export`.
`generate` is the end-to-end pipeline on a live fixture DB: seed two
services + a DEPENDS_ON edge, capture the snapshot, factual +
relation scenarios, every query proven through the oracle (a failed
scenario raises), context compiled per question through the server
Context Compiler over a mocked-ir KnowledgeSnapshot, answers
certified (refused examples are never emitted), examples split by
component-root keys (union-find over the edges, §17), written canonically, re-run to a scratch dir to
prove byte-identical regeneration, validated against that scratch as
the reference — **exit 0 iff publishable**. `validate`/`stats`/
`export` share the same reader/writer; stats and export never judge.

## 17. Observability + errors + benchmark (T-13)

The §28 typed error model (`errors.py`): `TrainingDataError` with four
optional category fields — `stage` (pipeline phase), `scenario`
(scenario id), `code` (machine-readable short tag), `example_id` —
and a JSON-serializable `to_info()` carrying all of them. SchemaError
and DatasetError stay siblings with no category fields; the pipeline
raises carry the ones their stage knows (oracle_failed carries
stage+scenario+code, split_leakage stage+code, no_examples stage+code).
The CLI's `main` catches `TrainingDataError` and exits 1, so the
categories are for callers and tests to observe, not for the user to
parse.

The §27 `Metrics` accumulator (`metrics.py`): `count(name, n=1)`,
`rate(name, numerator, denominator)`, `as_dict()` →
`{counts, rates}`. **The no-sensitive-content rule is structural:**
every leaf value of a metrics dict is a number under a fixed key
name (the accumulator only ever adds ints and ratios), so no
question, answer, fact statement or KO text can land in one. An
undefined rate is None, not zero — a zero denominator is absence of
signal, not a measured 0.0. generate counts scenarios / unexpressible
/ refused / emitted / per-split sizes and derives the refusal rate;
`--metrics FILE` writes the JSON after validation succeeds.

`scripts/benchmark_dataset.py` is the §27 benchmark surface: it runs
the full generate pipeline as a subprocess against a live server
(PYTHONPATH = src) and reports one JSON object with three cells —
throughput (wall seconds, examples, examples/second), rates (the
`--metrics` rates verbatim) and size (dataset bytes on disk,
per-split counts). Laptop scale by design; the corpus-scale cell
arrives with T-14.

**The split-key correction (the T-13 regression catch).** The
koid-set join keys factual `{s}` and relation `{s,c}` differently
for the same knowledge component, so fresh HLC koids drew straddling
buckets on live runs (the T-12 greens were bucket-lottery luck).
`component_ids` in `splitter.py` is union-find over the edges
(each with from/rel/to): path-compressed find, union by min koid, so
a component's id is its lexicographically smallest koid —
deterministic and independent of edge order. The builder stamps the
component root as the example's split_key (koids outside the edge
graph fall back to the set join), so every example touching a
knowledge component shares ONE key and cross-holdout pairs are
impossible under every seed. Regression-pinned both sides:
component keys violation-free across a 50-seed sweep; the old
set-join keys straddle some seed.

## 18. Corpus + eval set + mutation leg (T-14)

`scripts/generate_corpus.py` is the §35–37 pipeline over a live
server, one process, one Agent connection for the whole run (a second
opens only for validation):

- **the seed graph** (`_seed_slice`, §35 counts per slice): 24
  services, 6 teams, 12 persons, 8 accounts, 4 regions; edges OWNS
  24, DEPENDS_ON 23, WORKS_IN 12, IN 8; three services versioned on
  the same KOID after a ~1s real-time gap (packed-HLC millis differ,
  so AS_OF distinguishes v1/v2); one service contradicted via the raw
  MCP `contradict` tool, producing a counter-claim and a persisted
  Conflict KO;
- **scenario assembly** (`_generate`): the T-03..T-10 families run
  over the live graph; each candidate passes the live oracle
  (`verify_scenario`), context compile, and `build_answer` (refused
  candidates never become examples); the target is reached in seed 0
  alone (~507 examples/slice), the 10K sweep is seed 1..19 on CI;
- **determinism**: examples are koid-free (question multiset +
  task-type histogram), so two independent servers must produce the
  same corpus — pinned by `test_corpus_regenerates_identically_across_servers`;
- **eval set** (`validation/eval_set.py`): E1–E9 as machine-checkable
  cases over the generated dataset (compile rate, recall, grounding,
  refusal, leakage, security, determinism, schema, duplication);
- **mutation leg** (`scripts/mutation_leg.py`, §37): mutant datasets
  (tampered manifest, planted secret, dropped fact, forged id) must
  each fail their gate with `publishable=false`.

**Three traps, all root-caused here:**

1. **`remember()`-with-koid replaces caller-created edges wholesale**
   (kernel.rs update path — deliberate semantics). A versioned
   re-remember AFTER linking orphans the edges from the relationship
   index (the update restates only kernel-managed edges), and TRAVERSE
   then goes empty. The corpus orders every slice versioning-first,
   linking-after; `relate` restates the full relationship list, so
   post-update links survive.
2. **An undrained stderr pipe can hang the validator's connect.** The
   tantivy commit storm after seeding writes thousands of log lines;
   a full pipe blocks the logging thread, the next handler to log
   ("client connected") stalls before it ever reads initialize, and
   the client waits out its socket timeout. The test fixture now
   sends server stderr to a file (no backpressure; the CI-15
   early-exit diagnostic reads the file instead of the pipe). The
   validator also connects with a generous timeout — post-seed index
   churn is real.
3. **The manifest sha256 check must be a gate, not a raise.** The
   writer's tamper refusal (`read_dataset`, FZ-T2) raised out of the
   validator, so a tampered dataset produced no report at all —
   `validate` died with a stderr line and the security test's
   expected report never appeared. `tampered_splits()` is now the one
   comparison, shared by `read_dataset(verify=True)` (raises) and the
   validator (`verify=False`, reports it as the `integrity` gate);
   every other gate — secret_scan included — still runs over the
   tampered content.

Multi-hop emits **same-rel paths only**: the example contract stores
ONE query, and a mixed-rel path needs one query per hop, so its
stored query could never recover the far hop under the
scenario_match gate (which checks `koids[1:]` against the TRAVERSE
closure — the runtime never returns the source KO). Same-rel paths
ride a single DEPTH-n query whose closure covers every hop. The
mixed-rel generator stays exercised by the scenario unit tests; only
the corpus subset is filtered.

The §6 CI wiring lands here: `training-data.yml` (fast-exit on
`training/**` + workflow paths per the CI-03 pattern) runs the unit
leg, the corpus cells, the eval set and the mutation leg.

## 19. Fine-tune run + scorecard (T-15)

Phase 18–19 (§32/33/34): the scorecard precedes any training run —
as code, not just prose. `scripts/finetune.py train` calls
`_require_scorecard()` and exits unless an artifact exists under
`training/artifacts/scorecards/`.

- **`src/aikoql_training/scorecard.py`** — `compute_scorecard(predictions,
  ds, split)` joins prediction records to examples by example_id and
  computes eight metrics (each `{value, detail}` with counts; §28):
  query_compile_rate (the live `compiled` flag when recorded, else
  the E3 static head check), ko_recall / ko_precision (oracle
  targets rule: TRAVERSE never returns the source KO, so multi-koid
  examples check `koids[1:]`), groundedness (predicted answer re-run
  through `validate_grounding` as a pseudo-example — the T-07
  deterministic ceiling), unknown_recall / unknown_precision /
  false_refusal_rate (the refusal trio), secret_leak_rate
  (gates' `_SECRET_PATTERNS` over query+answer — one list, never a
  second). Missing predictions fail every metric.
- **`src/aikoql_training/inference.py`** — the §40 prompt/parse
  contract: `build_query_prompt` / `build_answer_prompt` (context
  bullets + UNKNOWN: refusal instruction) and `parse_model_reply`
  (marker split; no QUERY marker → the whole reply is the answer).
  ponytail: plain marker search — a marker inside a query literal
  would mis-split; the corpus grammar values never carry them.
- **`scripts/finetune.py`** — two skill rows per example,
  completion-only labels (-100 prompt masking; a BPE merge across
  the boundary mislabels one token), `_collate` pads labels with
  -100, left-padded batched greedy generation sliced back by
  `attention_mask.sum(dim=1)` so the prompt's own instruction text
  never confuses `parse_model_reply`. `--device` defaults to cuda
  when torch sees the GPU (fp32 — the 1650's 4 GB holds the 0.5B
  base; no bf16 autoconversion), cpu otherwise; `predict
  --db/--token` runs live: each predicted query compiles and
  executes against the corpus server, recording real
  `compiled`/`retrieved` per example.
- **`scripts/scorecard.py`** — joins dataset + predictions.jsonl
  into a committed artifact (`model_id`, `model_class`, adapter,
  dataset_id, seed, split, git revision, created_at + the eight
  metrics).

**One-slice corpora can land train-only.** Split assignment hashes
`f"{seed}:{split_key}"` into ratio buckets with the seed pinned by
the snapshot; a single slice produces six component keys, and under
the pinned seed all six hash into train — the test split is empty,
predict writes zero records and the scorecard never forms. Three
slices give 18 keys and a real 8:1:1 spread (24/24/1802 on the
laptop POC); the CI 10K sweep (seed 1..19) was never at risk. A
stale HF OAuth token was the second trap: an expired refresh_token
poisons even public model downloads (401 → "Repository Not Found");
`huggingface_hub.logout()` clears it. The third: a `+cpu` torch
wheel keeps a present GPU invisible (`torch.cuda.is_available()`
false) and pip skips the same-version swap — force-reinstall the
cu126 wheel (`--force-reinstall --no-deps`); the first CPU training
attempt (0 steps in 20 minutes) made the GPU the only sane path.


## 20. Inference wrapper — the chat path (T-16)

The design §40 end-state: question → model (intent/query) → AIKOQL →
context → model (grounded answer), with every refusal machine-readable.

- **`src/aikoql_training/chat.py`** — `chat(question, *, generate,
  run_query) -> record`. The two model calls go through the §19
  prompt/parse seam; the one retrieval is a live `aikoql()` call and
  the context statements are built from its results only — a wrapper
  reading context from the dataset would bypass the oracle, so the
  tests pin that the answer prompt carries the run_query statements.
  The record: question, query, compiled, retrieved koids, statements,
  answer, refused (answer starts with the T-09 `UNKNOWN:` prefix).
  Refusal paths: no query produced / query failed to compile or
  execute (the exception path) / query returned no results / the
  model's own UNKNOWN: refusal passed through verbatim.
- **`scripts/chat.py`** — the POC chatbot: the T-15 model (LoRA
  adapter optional, `--device` cuda default), a live server
  (`Agent.connect`, the wide 60 s timeout for post-seed tantivy
  churn), one-shot `--question` or an interactive stdin loop.

The deterministic test ceiling (T-07's rule): the seams are stubbed
in unit tests — the query step prompt, the answer step prompt over
the results-derived statements, the four refusal paths, and the
eval-set routing (a grounded example answers, an unanswerable one
refuses). The live smoke on the seeded corpus server closes the
loop: a grounded question compiled, retrieved and answered
end-to-end; an out-of-knowledge question produced a hallucinated
query, the compile failed and the wrapper refused. RED archived as
`t-16-chat`.

## 21. Schema v2 — structured semantic target (T-17)

The PR9 TDD-01 P0.2 gap: `semantic_target` carried an opaque intent
string and no plan, so a model could never be trained or graded on
*how* an answer is reached. Schema v2 makes the reasoning path a
first-class, machine-checkable structure.

- **`src/aikoql_training/plan.py`** — `plan_of(scenario)` derives
  `(intent, entities, requirements, plan)` from the scenario alone:
  intent is the task type; entities are the koid set in scenario
  order, each with a closed role — `subject` (the path origin),
  `target` (the path destination), `intermediate` (every edge in
  between), `candidate` (a member of a multi-answer set when there
  is no path). Requirements are the sorted set of asked-for
  properties plus every edge relation the path traverses. The plan
  is the executable spine — `resolve_entity` for the subject, one
  `traverse` per edge, a final `project` when a property is asked
  for — with `temporal: {"as_of": ...}` exactly when the scenario
  is time-bound. `policy_of(scenario)` pairs it with the policy
  section: the ACL pair (subject, action) when the task type is
  authorization, `authorization_required: false` otherwise.
- **`models.py`** — the closed sets: `PLAN_OPS` (resolve_entity,
  traverse, project), `ENTITY_ROLES` (subject, target,
  intermediate, candidate); the plan keys close to `{steps,
  temporal}`; an authorization-required policy must carry a
  non-empty subject/action pair (fail-closed both directions); and
  context evidence entries must be dicts. The validation demands
  a plan — v2 rejects what v1 silently accepted.
- Both assemblers (`cli.py`, `scripts/generate_corpus.py`) consume
  the same two helpers, so the corpus and the CLI can never drift
  on what a semantic target means.

The derivation is pure — no oracle call, no I/O — so the tests pin
it exhaustively: the four roles, requirement closure over edges and
properties, the temporal arm, the empty/no-path shape, ambiguity
candidates, never mentioning entities outside the scenario, and
determinism across calls. The schema contract tests pin every
closed set with a mutated example that validation must reject.
RED archived as `t-17-schema-v2`.

## 22. Plan→renderer seam (T-18)

Before T-18 the plan (T-17) and the query text (T-05) were two
independent walks of the same scenario — `plan_of` derived the path
roles while `build_queries` re-walked `expected_path`, so the
example's semantic plan and its query_target could drift apart. The
seam makes the plan the single source of truth.

- **`render_queries(plan, scenario, kos)`** in
  `generators/query.py` — emits TEXT aikoql from the plan only:
  the path comes from the plan's `traverse` steps (same-rel →
  one DEPTH-n query, mixed-rel → the chained one-per-hop form),
  the RETURN property from the `project` step, the temporal bound
  from `plan["temporal"]["as_of"]`. The scenario's
  `expected_path`/`property`/`as_of` are never re-read for the
  structured families.
- The anchor-probe families (unknown/ambiguity/contradiction/
  authorization) have no knowledge path — their plan legitimately
  carries no path steps — and the renderer emits the probe from the
  scenario's anchor triple (`anchor_prop`/`anchor_value`/`type_name`),
  exactly as the T-09 generators seed it. An empty plan is the
  semantic truth for those families, not a rendering gap.
- **`build_queries(scenario, kos)`** stays as the public entry
  point, defined as `render_queries(plan_of(scenario)[3], ...)` —
  so the example's query_target and its semantic_target are
  structurally the same derivation.

The seam tests pin both directions: through-the-seam rendering
reproduces the pinned query strings for every family, and a
hand-built plan that disagrees with the scenario (different path,
property, or as_of) wins — the renderer follows the plan, proving
the scenario fields are dead input for the structured families.
A path-family plan without steps never renders a query
(fail-closed, same as the unrepresentable-input rule). RED
archived as `t-18-renderer`.

## 23. Leakage dimensions + group union (T-19)

PR9 Finding #4 (TDD-10/FZ-08): the holdout must be checked along
the review's leakage axes — entity, identifier, template,
relation-pattern, graph-component, semantic-duplicate, answer. The
T-12 leakage gate counted only two failure modes: recorded placement
disagreeing with the recomputed hash assignment, and cross-holdout
`expected.koid` pairs. T-19 adds the missing dimensions, split by
whether they veto.

- **Component group union** — `component_ids(edges, groups=())`:
  ambiguity groups share an anchor value (two KOs with the same
  name ARE one entity from the model's view) yet have no edge
  between them, so the T-12 union-find left them in separate
  components — ambiguity examples about {a, b} and factual examples
  about a or b could straddle the holdout under some seed. A
  declared group now unions its members onto the min-root, and the
  corpus passes `[s.koids for s in ambiguity scenarios]`. Edgeless
  group members form a component too (the group is knowledge
  before any relation exists).
- **HARD: canonical-question overlap == 0** — the exact question
  text appearing in two splits (same question, two scenarios) means
  the model saw the answer in one split, so the other can no longer
  measure it. New tooth on the leakage gate.
- **Diagnostic dimensions** — `identifier` (entity names in the
  context), `normalized` (question with quoted refs masked),
  `answer`, `relation_pattern` (sorted traverse relations from the
  plan) — counted per split pair and reported on the gate entry as
  `gates["leakage"]["dimensions"]`, never vetoed. Template corpora
  share these STRUCTURALLY — "Payments Team" owns many services,
  every factual question shares its template — and the review
  itself consigns embedding/similarity overlap to the diagnostic
  class. A hard normalized-question gate would veto every
  template-generated corpus, the T-14 corpus included.
- The dimensions report is a pure function of the example set
  (shuffle-invariant, FZ-08).

The poisoned-dataset tests plant exactly one dimension per test —
canonical overlap fails the gate while answer/identifier overlap
passes with the counts reported; a clean dataset reports all-zero
dimensions. RED archived as `t-19-leakage-dims`.

## 24. Structured JSON model protocol (T-20)

PR9: the §40 seam spoke to the model in marker prefixes (QUERY: /
ANSWER:) and the pipeline split replies on them — prose the parser
sniffs for, with a bare-prose fallback that accepted anything. The
model now speaks a schema-validated JSON protocol; refusal and
grounding are FIELDS, not prose.

- **Two shapes** — query: `{"query": str|null, "refusal_reason":
  str|null}` with exactly one of the two set; answer: `{"answer",
  "grounded", "claims": [{"statement", "evidence_ids"}],
  "refusal_reason"}`.
- **Fail-closed parser** (`inference.py`:
  `parse_query_reply` / `parse_answer_reply`) — the first JSON
  object in the reply is decoded (stdlib `raw_decode`, so a prose
  wrapper is tolerated), then validated strictly: unknown fields,
  wrong types, and every cross-constraint in BOTH directions —
  refusal_reason set ⇔ UNKNOWN: answer with no claims and
  grounded=false; a refusal cannot be grounded; grounded ⇔ claims
  non-empty. Any deviation raises `ModelOutputError`
  (errors.py) and the caller refuses — the T-16 bare-answer
  fallback is gone.
- **Chat path** — every refusal is a field: the model's own
  refusal_reason passes through machine-readably; an unparseable
  reply becomes `UNKNOWN: model reply was not valid protocol
  JSON`; the record carries `claims`/`grounded`/`refusal_reason`.
- **Fine-tune contract** — `finetune.py` emits JSON completions
  (refusals and grounded answers with per-fact claims carrying
  their evidence ids) and predicts through the parsers, fail-closed:
  an unparseable reply predicts nothing.

The protocol tests pin the shapes, the prose-wrapper tolerance, and
each cross-constraint direction; the chat tests route refusals
through the fields. RED archived as `t-20-json-protocol`. T-21 fuzzes
this parser.

## 25. Model-output fuzz (T-21)

PR9 FZ-10 / P0.5: the protocol parser must hold the property
`invalid model output -> typed validation failure -> safe refusal`,
never best-effort execution. `training/tests/test_protocol_fuzz.py`
pins it on the T-20 seam:

- **Arbitrary text** (hypothesis) either raises `ModelOutputError` or
  returns a reply satisfying every protocol constraint — re-checked
  by an invariant half, so a leaked KeyError/TypeError fails the
  property.
- **FZ-10 families** land exactly where the contract pins them:
  missing fields, duplicate keys (top level AND inside claims),
  truncated JSON, non-objects, nested values and wrong types raise;
  code fences, prose wrappers, injection prose, delimiter-carrying
  query strings, adversarial Unicode and large payloads parse — a
  query string may carry JSON delimiters because the parser is a
  JSON parser, not a marker split; extra objects after the first are
  the documented first-object-wins semantics.
- **Parser hardening** (inference.py): duplicate keys reject via an
  `object_pairs_hook` (a model emitting dupes is emitting garbage,
  not a vote); missing fields raise a typed error instead of a
  KeyError; JSON nested past the decoder's limit (~100k) becomes a
  typed refusal, not a RecursionError.
- **Chat seam** — an uncompiled reply never reaches `run_query`
  (never best-effort execution), verified by fuzz and a
  deterministic pin.

RED archived as `t-21-model-output-fuzz`.

## 26. Claim-level grounding (T-22)

PR9 TDD-07 / P1.3 / Finding #2: grounding was one substring check per
answer. Grounded answers now carry the claim decomposition, and the
validator walks the claims — Level 1 of the review's three-level
architecture (structural; claim/evidence alignment; semantic
entailment — the latter two stay out, and no LLM becomes the
production truth oracle).

- **Representation** — `build_answer` emits one
  `{"statement", "evidence_ids"}` claim per supporting fact (the
  claim text IS the fact statement; paraphrase is the fine-tuned
  model's job). `expected.claims` is schema-optional; refusals carry
  none; the corpus assembler forwards it.
- **The walk** (`_validate_claims`, wired into the generic and
  authorization branches of `validate_grounding`) — every claim must
  trace to an evidenced fact; every cited id must attach to a
  supporting fact of that claim (a forged id fails); a claim without
  ids fails; every answer-supporting fact must be covered by a
  claim; the claims' evidence union equals `expected.evidence_ids`
  exactly; claims on an ungrounded example fail. Examples without
  claims keep the answer-level trace.

RED archived as `t-22-claim-level-grounding`. T-23 mutates this.

## 27. Grounding mutation fuzz (T-23)

PR9 FZ-07: the validator must reject every mutation of a valid
example that breaks the claim/evidence relationship — much more
meaningful than fuzzing JSON syntax. `training/tests/test_grounding_fuzz.py`
holds the mutant matrix over a valid claims-carrying example, one
row per review axis (answer, fact statement, evidence, evidence_id,
KOID, entity name, relation, label):

- **Relationship-breaking mutants all fail** — dropped or forged
  evidence ids, evidence absent or mutated in context, swapped fact
  statements, claims dangling or swapped onto non-supporting facts,
  malformed/empty claim decompositions, and label flips: grounded
  False on a traced answer; answerable False or ambiguous True on a
  grounded answer (two new fail-closed checks in the generic branch,
  mirroring E7/E9 at the per-example boundary).
- **Boundary pins** — koid swaps, entity-name swaps, relation rows,
  added non-supporting facts and duplicated claim ids keep
  validating: those fields are the leakage/scenario-match gates'
  job, and grounding rejecting them would double-count.
- **Properties** — arbitrary answer mutations that remove the
  answer's supporting evidence fail; every fact-statement mutation
  fails (it either removes the support or dangles the claim). An
  answer mutation that keeps support may validate: answer identity
  is the oracle's job, not grounding's.

RED archived as `t-23-grounding-mutation-fuzz`.

## 28. Unknown precision/recall — refusal metrics (T-24)

PR9 Finding #6 ("Unknown / Refusal Is Under-Tested"): the scorecard's
single `refusal_rate` could not show refusal *quality* — a model that
answers unknown examples with a refusal but also refuses answerable
ones scored the same as a model that refuses nothing. The combined
number is replaced by a refusal trio, each with an explicit
denominator (detail carries the counts) and `None` when its
denominator is absent from the split (§27: an undefined rate never
reads as 0.0):

- **unknown_recall** — refused / unknown: unknown examples answered
  with the `UNKNOWN:` prefix. None when the split has no unknown
  examples.
- **unknown_precision** — refused / (refused + false_refusals): of
  every `UNKNOWN:` answer, the share that landed on an unknown
  example. None when the model issued no `UNKNOWN:` answers at all —
  a model that never refuses scores None, never a fake 0.0.
- **false_refusal_rate** — false_refusals / answerable: `UNKNOWN:`
  answers on answerable examples (the false-refusal damage). None
  when the split has no answerable examples.

The committed T-15 artifacts migrate in place: their refusal detail
counts (unknown=0) become unknown_recall/unknown_precision `null`
and a defined false_refusal_rate 0.0; the artifact test accepts
`None` alongside the 0..1 range.

RED archived as `t-24-unknown-precision-recall`.

## 29. Live-oracle authorization (T-25)

PR9 Finding #7 ("Authorization Untested Against the Live Server") +
P0.6: authorization examples asserted the ACL *structurally* (a
flag + subject/action) but were never re-proved against the kernel.
Authorization is now a two-stage live oracle:

- **At generation**: the policy section carries the complete
  machine-readable verdict — `subject`, `action`, `resource`, the
  kernel's `decision` (bool) and, for denials, the preserved `reason`
  — straight from the deployed policies' `evaluate_policies` results.
  `models.validate` fails closed on any missing/wrong-typed piece
  (TDD-01 "invalid authorization metadata"), and the grounding
  validator cross-checks verdict prefix ⇔ `decision` plus the
  preserved reason appearing in a denied answer.
- **At verification** (`verify_authorization_examples`): every
  committed authorization example is re-evaluated through the live
  `evaluate_policies` — `policy.decision` must equal the current
  verdict and a denial's `policy.reason` the live reason verbatim.
  The corpus builder runs the leg over the written dataset and
  aborts fail-loud on any disagreement; the report carries
  `authorization.{ok,errors,checked}`.

The mutation leg registers the review's §26 authorization mutants —
always-allow (grounding), ignore-subject / ignore-action /
ignore-resource (schema) — each killed by its own suite, and the leg
now targets per-mutant (file, suite) pairs instead of a single
hardwired validator.

RED archived as `t-25-live-oracle-authorization`.

## 30. Per-capability scorecard (T-26)

PR9 Finding #5 ("Scorecard Is Useful but Currently Too Coarse"): the
eight T-15 metrics were aggregate-only, so a model that fails one
capability while passing the rest could still look viable — not
sufficient for AIKOQL-native model selection. §28 already demanded
the breakdown (overall / factual / one-hop / multi-hop / temporal /
provenance / unknown / ambiguity / contradiction / authorization).

`compute_scorecard` now returns the aggregate plus `by_task`
(task.type) and `by_difficulty` (task.difficulty) cells — each the
same shape as the aggregate (`example_count`,
`missing_predictions`, the eight metrics), only present capabilities
listed, and §27 holds per cell: an absent denominator is None there
too, never a silent 0.0. The per-example loop extracted as `_cell`
and reused for every group; the artifact script picks the breakdown
up automatically via `**score`, so the next training run commits it
with no schema change.

RED archived as `t-26-per-capability-scorecard`.

## 31. Multi-domain corpus (T-27)

PR9 P1.1 (Level 3/4 of the generator hierarchy): an AcmePay-only
corpus cannot show schema generalization — the generators could pass
by hardcoding the payments vocabulary. The corpus now seeds one slice
per sweep seed per domain: AcmePay (payments) plus NovaEnergy
(utilities), a second org over the same kernel types with a disjoint
property schema (operator/uptime_pct/site vs owner/tier/status,
head/specialty vs lead/focus, credit/tariff vs balance/currency,
grid_code vs sla). Every scenario family walks the schema, so each
domain drives factual/relation/multi-hop/ambiguity/contradiction/
authorization questions in its own vocabulary; temporal/provenance
stay acmepay (only acmepay seeds versioned services — the
multi-domain tooth is schema breadth).

Two cross-cutting seams: `_domain_of(ko)` discriminates a KO's domain
by its property schema (the two schemas share no key) and prefixes
the per-KO/group/conflict document ids; the seeding loop iterates
`_DOMAINS`. The orgs' name vocabularies are disjoint by construction
(plant/crew/worker/meter/zone vs svc/team/person/acct/region) — a
shared stem would let one domain's fact text contain the other's
anchor name, which the T-25 denial-gate's strict substring check
would flag as a denied-object leak (the trap the RED caught).

RED archived as `t-27-multi-domain`.

## 32. Held-out organizations (T-28)

PR9 Finding #3 (P1.2): a corpus whose eval splits share the training
orgs can only measure memorization — the eval questions must
reference entities the model never saw. NovaEnergy is the held-out
org: every example carries an `org` stamp (schema-optional, present
on corpus examples), `assign_splits` hashes held-out examples into
val/test only (their relative weights — the assignment law extends to
a pure function of (split_key, org, seed)), and the manifest declares
`held_out_orgs` so the leakage gate recomputes the placement with the
same declaration (a held-out example recorded into train is
misplaced, fail-closed). The corpus test pins the end-to-end tooth:
no train context may reference a held-out entity.

Two seam fixes the held-out split demanded: the org-neutral unknown
scenarios anchor on a training org's service (a held-out anchor would
leak its entities into a train context), and the authorization
policy docs are keyed per (principal, action, verdict, domain) — the
shared policy doc used to carry every org's decision lines, so an
acmepay train example's context named held-out services. Temporal
and provenance stay acmepay: only the training org seeds versioned
services, so those capabilities are not yet measured on held-out
data (a documented ceiling, not a leak). The mutation leg registers
two mutants: the holdout force disarmed, and the gate's manifest
declaration ignored — each dies in test_gates.

RED archived as `t-28-held-out-orgs`.


## 33. CI reshape — installed package + nightly sweeps (T-29)

PR9 §33/§34. Two changes, one principle: the CI matrix must exercise
the shipped artifact, not the source tree.

**The package is the execution path (§34).** The pytest
`pythonpath=src` config made every CI unit run pass against the
source tree instead of an installed package — a packaging break
would stay invisible until a bare script died at import. The config
is gone; every training-data.yml leg installs
`pip install -e training` into its venv, and the tests (run without
any path smuggling) exercise the installed copy. Production entry
points follow: `chat.py` no longer prescribes
`PYTHONPATH=training/src` and `benchmark_dataset.py` no longer
injects the source path into its subprocess — the installed package
is the execution path everywhere outside the tests themselves (test
fixtures may still point subprocesses at `training/src`; that is
test infrastructure, explicitly tolerated by the review). The
mutation leg keeps its `-o pythonpath=` flag as a defensive no-op:
if a pytest path config ever reappears, it cannot shadow the mutant
trees.

**Nightly cells leave the PR matrix (§33).** The 10K corpus sweep
was bolted onto the integration leg, making every training push pay
the 90-minute corpus budget. It now lives in a `nightly` leg gated
on `github.event_name == 'schedule' || 'workflow_dispatch'` (push
and PR runs skip it; a manual dispatch runs it deliberately), and
the workflow declares a weekly cron. The integration leg keeps the
full suite including the corpus contract. The fuzz-estate leg now
runs the whole fuzz estate (schema, scenario, protocol and
grounding fuzz files), not the two files it accreted with. The
leakage and security gates stay inside the unit leg by design —
they are unit validators (test_gates.py runs all eleven §5 gates,
secret scan included), and splitting one file's tests across legs
by `-k` selection would be more fragile than the coverage it buys.
Benchmark-at-corpus-scale is not wired (the nightly list's
benchmark cell); the laptop-scale benchmark test rides the unit
leg, and the 10K cells are the corpus sweep itself.

The hygiene script pins all of it: workflow test 33 fails while the
pyproject pytest section carries a path smuggling word or any
training-data leg lacks the package install; workflow test 34 fails
while the integration leg carries the 10K sweep, no nightly leg or
schedule trigger exists, the nightly leg is unguarded, or the
fuzz-estate leg drops a fuzz file. RED archived as
`t-29-ci-reshape`.

## 34. Architecture hygiene — training import boundary (T-30)

PR9 §35. The training package sits on the SDK's public surface only;
a dependency on private internals is architectural drift that no test
catches until it breaks in a user deployment. The boundary is now
deny-listed, not just convention: `scripts/training-import-boundary.txt`
holds one grep-`-E` anchor per forbidden import shape, and workflow
test 35 scans `training/src` + `training/scripts` `.py` files against
every anchor — any hit fails the architecture gate.

Three anchors cover the whole private surface:

- `aikoql.<sub>` — deeper than the documented top level (SDK
  submodules, adapters, checkpointer, native bindings);
- underscore-private names pulled through the top level
  (`_aikoql`, `_mnemosyne` native modules);
- storage engine bindings (`redb`, `rocksdb`, `rocksdict`) — training
  data talks to the engine only through the SDK.

Allowed by absence: the bare `import aikoql` and `from aikoql import
<public name>` top level. The scan is fail-closed by design — the
absence-of-guard RED (boundary file missing) is archived first, then
plant-tests prove each anchor fires on a real violation before the
plants are removed. A future dependency on a private module dies in
the gate, not in a user's environment. RED archived as
`t-30-import-boundary`.

## 35. PyPI publish wiring (T-31)

The training package ships as its own PyPI project (`aikoql-training`),
not folded into the maturin-built `aikoql` wheel — release coupling and
build-system friction, two versioning clocks for one artifact. The
package metadata is complete (readme, Apache license file, classifiers,
project urls) because PyPI rejects incomplete metadata at upload time,
not at PR time. `release.yml` gains `training-pypi-publish`, the OIDC
trusted-publishing twin of the SDK's `pypi-publish` job — but the
version truth is `training/pyproject.toml`, not the aikoql tag: the two
projects version independently, and a training release is a plain
re-dispatch (the skip guard turns an already-published version into a
no-op, the rescue-over-an-existing-cut pattern). The job install-checks
the exact wheel before upload — the §33 principle, CI exercises the
shipped artifact. Workflow test 36 pins the job (action + OIDC
permission) and the three metadata teeth. RED archived as
`t-31-pypi-publish`. The PyPI-side pending publisher (project
aikoql-training, repo anckursingh/aikoql, workflow release.yml) is a
manual one-time step.

## 36. Query-layer breaks — numeric promotion + Grouped tool rows (T-32)

DI-006 re-verification against a repo build surfaced two query-layer
breaks the estate missed, both at the seam between the runtime and the
tool surface. (1) Numeric predicates read empty on Float-stored
properties: JSON floats remembered through the MCP tool land as
`Value::Float`, while integral query literals arrive as `Value::Int`;
the runtime `compare_values` returned `None` on the mixed pair and
`Eq` used derived `PartialEq`, so every numeric predicate failed
closed on the type boundary. `compare_values` now promotes Int/Float
(the same rule the kernel helper always applied) and Eq/Neq route
through `values_equal` — derived equality stays for the shapes
comparison does not order (List/Map/Bytes), and Neq is exactly the
negation of Eq on every pair. (2) GROUP BY returned empty through the
MCP `aikoql` tool: the interpreter computed `RowSet::Grouped`
correctly, but both tool conversion matches dropped it via a
catch-all empty arm; the streaming and non-stream paths now convert
Traversal/Grouped/Joined rows the way http/shell already did. Why the
estate missed both: runtime tests call `Interpreter::execute`
directly and never crossed the MCP `RowSet`-to-JSON conversion, and
no filter test paired an Int literal with a Float-stored property —
the exact shapes the remembered-data flow produces. The regression
tests live at both layers now (runtime promotion pin + tool-layer
grouped-row pin). RED archived as `t-32a-numeric-promotion` and
`t-32b-grouped-tool`.

## 37. AS_OF counter inclusivity — same-millis commits (T-33)

PR #9 CI failed corpus generation since Oct 4 with a `StopIteration`
in `generate_corpus.py::_history`: the per-version snapshot re-reads
each version through `MATCH service AS_OF (commit_ts >> 16) RETURN *`
and the target koid was absent from the results. Root cause sits in
the packed-HLC round-trip, not the generator: the HLC packs
`(millis << 16) | counter`, and a commit that shares its millisecond
with a sibling carries counter bits. `get_as_of` packed the snapshot
at `millis << 16` (counter 0), so the MVCC predecessor walk —
`obj_key(koid, snap)` as the seek key — skipped that version's key
entirely; for a first version there is nothing older to fall back to,
the AS_OF row was dropped and the generator crashed. Same-millis
commits are a timing race: CI runners hit it routinely (the Oct 4
run failed both the determinism and integration legs), the laptop
did not — which is exactly why the estate passed locally.

`get_as_of` now fills the counter to `0xFFFF`: `AS_OF T` selects the
newest version committed at any point during wall-clock millis T,
matching the docstring contract ("packs to the HLC layout
`millis << 16 | counter`") and making the trace()/AS_OF round-trip
hold for every version a client can ever be told about. The
deterministic pin is a kernel test, not a timing gamble:
`as_of_sees_versions_committed_within_the_same_millisecond` runs on a
frozen `ManualClock` — a warmup commit occupies counter slot 0, the
version under test commits with counter bits, and `AS_OF` at that
millis must still return it. The live temporal wire test
(`test_temporal.py`) had the same latent race in a silent form (two
back-to-back remembers whose v2 snapshot could read v1's properties);
the kernel fix closes that too. RED archived as
`t-33-asof-counter-inclusive`.


## 38. Semantic-enrichment write path — N2/N3 (T-34)

The device-identity eval (aikoql-issues.md, repo build at ebe7389) found
two bugs on the same write path, both invisible to the estate:

- **N2 — enrichment wipes caller edges.** `enrich_one` wrote back through
  `RememberRequest::update` carrying only `properties` + `semantic`.
  `remember_locked`'s update path replaces the edge set wholesale
  (kernel-managed SUPERSEDES/DERIVED_FROM/CONTRADICTS carried forward,
  caller edges restated-or-deleted), so the serve-start catch-up silently
  destroyed every `relate`-created edge in the KB. The estate missed it
  because no test ever ran enrichment against a KO that already carried
  caller edges. Fix per the issue's option 1: a dedicated
  `attach_semantic` kernel path that mutates ONLY the semantic field —
  commit carries `prev_rels = head.relationships`, so the relationship
  index sees no removals; identical re-attach is a no-op (restarts do not
  churn versions). The enricher never enters the edge-replacement path at
  all, so the class of bug is closed rather than patched.

- **N3 — prove vs. the enricher's appends.** `prove` walked `scan_events`
  (a snapshot-less KV scan) and then compared the chain tail against
  `journal_head` read separately — an append between the two made an
  untampered chain report `chain_valid: false` with the same event count.
  The eval's "superseded claims fail, live ones pass" was timing: the
  superseded claim sat earlier in the scan, buying the enricher more
  append windows. Fix: `prove` holds the pipe lock (every writer routes
  through it), so the walk sees a quiescent journal; point-in-time at the
  cost of writers stalling for the scan (ms at KB scale).

Pins: `enrichment_update_preserves_caller_edges_and_prove_chain` (edges
survive catch-up, prove stays valid) and `attach_semantic_boundary_sweep`
(NotFound/VersionConflict/AccessDenied/no-op) in the kernel tests; the N3
race is made deterministic, not timed — `prove_isolation.rs` wraps the
engine in a `SlowScanEngine` whose `ke/` scans sleep 50 ms while a writer
appends every millisecond, which without the lock fails every run. RED
archived as `t-34-enrichment-wipes-edges` and `t-34b-prove-not-isolated`.

## 39. Enrichment catch-up must not degrade vector queries — N4 (T-35)

The eval's degraded case: `USING EMBEDDING` against a KB whose semantic
catch-up had not finished returned hits for every KO with scores that had
no vector meaning. Root cause was two layers deep:

- **Kernel (the silent lie).** The coordinator's slim vector leg scored
  every head without an embedding at 0.0 — during the catch-up window a
  vector query "matched" the whole store. Fix: the leg skips unembedded
  KOs. A KO without an embedding has no vector score; only embedded KOs
  can answer a vector query, and the caller sees an honest empty instead
  of fabricated zero-score hits.
- **Runtime (the silent text fallback).** The delegate's brute-force path
  returned an empty vector side and the hybrid fusion then masqueraded as
  a text-only answer. Fix: when the query vector embedded (the provider
  ran) but no in-scope KO carries an embedding, the arm fails closed with
  `KError::Retryable` — enrichment is not ready, retry once
  `semantic.state == "ready"`.

Ceiling, by design: a hybrid query (BM25 + USING EMBEDDING) during the
window also fails Retryable rather than answering from the text side
alone — fail-closed beats silent partial. The kernel's honest empty is
the truth; the runtime guard is the contract.

Pins: `ann006_unembedded_kos_never_score_as_zero_hits` (mixed population
answers exactly the embedded KO; all-unembedded answers empty),
`ann_search_errors_when_no_ko_in_scope_has_an_embedding` and the
`ann_search_readiness_sweep` matrix (provider × enriched → Retryable
exactly when a provider ran and nothing was enriched) in the runtime.
RED archived as `t-35-embedding-degrade`.
