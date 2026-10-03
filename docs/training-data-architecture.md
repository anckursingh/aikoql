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

