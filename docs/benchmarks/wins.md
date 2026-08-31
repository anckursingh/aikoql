# Wave 3 — AIKOQL wins (measured)

Negative evidence is mandatory (plan §29): this file carries only measured
results, each pinned by a committed test. Mechanical slice, no LLM
(G12 convention) unless stated.

## Multi-hop retrieval (W4) — Strong Fit

- AikoQL 7/8 units vs lexical RAG 3/8 (Δ +4), fewer delivered tokens
  (225 vs 258 mean). Zero-overlap answer facts the chunk retriever cannot
  rank are reached via the entity graph + relation boost.
- Test: `w3_win_001_workload_classification` (W3-G04 asserted).
- Corroborating: G10 agent-efficacy D 1.000 (20/20), G11 comparative
  D 15/16 vs B 10/16 vs C 13/16.

## Provenance (W7) — Strong Fit

- AikoQL 2/2 vs RAG 1/2: the source-document unit (`kb-policy`) is only
  delivered by a payload that cites its sources.
- Test: `w3_win_001_workload_classification`; G11 pin: D prov 2/2.

## Temporal accuracy — the G11 0.0 row closes

- 2026 kernel timeline (assert → supersede → supersede): the compiled
  context carries the current claim + the historical incident fact and
  suppresses 2/2 superseded claims; the RAG baseline packs 2/2
  superseded claims (temporal accuracy 0, reproducing G11).
- Kernel history preserved: valid_to stamps + lineage readable.
- Test: `w3_temp_001_temporal_market_reality`.

## Contradiction value

- Unsafe issue-note + outdated ADR superseded onto the policy KO: the
  pack carries 1/3 claims (policy only); RAG delivers 3/3 including the
  unsafe instruction. Superseded claims stay readable (authority
  selection, not data loss).
- Test: `w3_conf_001_contradiction_value`.

## Longitudinal value

- 90-day capacity evolution (100→200→500→900): AIKOQL 4/4 checkpoints
  correct with flat tokens [27,27,27,27]; stateless RAG 1/4 (correct only
  until the world changes); conversation-history 1/4 with growing tokens
  [6,18,36,61]. Kernel history readable at day 90.
- Test: `w3_long_001_longitudinal_value`.

## Unknown handling

- Four classes asserted: known (answer fact in a healthy non-empty pack),
  unknown (healthy EMPTY pack — the `ContextPackage.status` contract, a
  genuine absence distinguishable from a degraded lookup), conflicting
  (KNOW-007: both claims, no silent pick), historical-only (stale
  boundary → empty pack while `get()` still serves history).
- Test: `w3_unk_001_unknown_handling_classification`.

## Debuggability

- Five injected failures (wrong source, stale fact, wrong relationship,
  conflicting sources, missing evidence), each surfaced by a
  deterministic kernel read; root-cause identification rate 5/5.
  Evidence-free assertions fail closed (P0-1).
- Test: `w3_debug_001_observability_root_cause`.

## Memory

- §32 memory bench: D 20/20 vs LLM+RAG 12/20, 577 tokens/7.5s vs
  1203/27.2s. Retention boundary halves recall billing (§40).
- Tests: `agent_efficacy_bench::agent_memory_bench`,
  `kernel/tests/memory_compression_bench.rs::sec40_memory_compression_measurement`.

## Build-vs-buy (W3-DEV-001/002)

- The retrieval-only application (tests/common: rank, pack, corpus,
  judge) is 1,042 LOC and delivers 0 temporal accuracy, no provenance,
  no conflict handling — every machinery gap above exists in the
  substrate and costs zero application lines.
- Source expansion: one `compile_file` dispatch, 6+ formats (md, pdf,
  rust, python, typescript, java, images) + storage adapters
  (redb/memory/sqlite/rocksdb/neo4j/postgres/mongodb/vector) behind one
  kernel API.
- Full table: `docs/WAVE3-MARKET-EVIDENCE.md` §Build-vs-buy.

## Cost

- G12 comparative cost bench: AIKOQL deterministic compile path, 0 LLM
  calls on retrieval; measured USD/query table in the G11/G12 outputs
  (see parity.md for the honest deltas).


## Wave 3.1 three-way comparison (W31-COMP-001, 148 tasks)

Predefined acceptance (written before first measurement): ≥1 Strong Fit
class, no class worse than RAG by >2 units, W1 control at full parity.
Measured: **9 Strong Fit classes, 0 regressions, control parity**.

| Class | AIKOQL | Graph-RAG | RAG | Δ |
|---|---|---|---|---|
| W3 synthesis | 18/24 | 7/24 | 7/24 | +11 |
| W4 multi-hop | 24/26 | 15/26 | 14/26 | +10 |
| W5 temporal | 30/32 | 25/32 | 24/32 | +6 |
| W7 provenance | 18/20 | 3/20 | 3/20 | +15 |
| W10 planning | 20/20 | 12/20 | 9/20 | +11 |
| W11 unknown | 26/30 | 21/30 | 21/30 | +5 |
| W12 longitudinal | 16/20 | 10/20 | 10/20 | +6 |
| W6 contradiction | 27/30 | 27/30 | 26/30 | +1 |
| W9 policy | 22/24 | 20/24 | 19/24 | +3 |

- Totals: AIKOQL 253/296 units vs Graph-RAG 191 vs RAG 181; grounded
  119/253 vs 0/191 vs 0/181 (only AIKOQL's payload cites sources;
  correct refusals are ungrounded by construction).
- Re-measured 2026-08-29 after the epistemic coverage gate shipped
  (gap item 10): 258 → 253 units. The gate converts vocabulary-overlap
  packs into correct refusals — W11 unknown improves 23 → 26 (the
  traps now refuse) and W2 semantic drops 8 → 4 (zero-overlap probes
  correctly refuse); the ident_parts root-cause fix rides along
  (+2 W10, +1 W9). Against that: −2 W3, −1 W5, −2 W7, −2 W12 where
  half-unexplained packs were emptied (rows in losses.md). All nine
  Strong Fit classes hold, 0 regressions, control parity — the verdict
  is recomputed by the test, never hand-edited.
- W7 is the structural win: the doc-id unit is only deliverable by a
  payload that cites its sources — raw chunks carry no citation.
- Holdout (Northwind, scored once): AIKOQL 38/48 vs Graph-RAG 39/48 vs
  RAG 38/48; grounded 20/38 vs 0/39 vs 0/38 (one W10 planning unit
  emptied by the gate, re-measured 2026-08-29).
- Tests: `wave31_comparison::w31_comp_001_three_way_comparison`,
  `wave31_comparison::w31_comp_002_holdout_evaluation`.

## Wave 3.1 deterministic agent chain (W31-REAL-001, 50 tasks × 5 reps)

Predefined acceptance (written before first measurement): no Sev-1
behavior, no unauthorized action, unknown tasks produce no unsupported
authoritative answers, repeatable advantage on the W7 class. Verdict:
**all true** — aikoql 405/500 vs RAG 290/500.

| Column | AIKOQL agent | RAG agent |
|---|---|---|
| Task success (win-zone) | 405/500 | 290/500 |
| Answers | 240 | 250 |
| Refusals (epistemic boundary) | 10 | 0 |
| Grounded answers (cite sources) | 240/240 | 0/250 |
| Unsupported tokens in answers | 0 (asserted) | 0 (echo) |
| Tool calls | 250 (1/task) | 250 |
| Retrieval retries | 0 | 0 |
| W11 false-confidence | 25/30 | 30/30 |
| Cost (G11 rates) | $0.0241 | $0.0234 |

- All 5 action-request probes Refused in every rep by both treatments
  (the policy has no Act arm — structural, not measured leniency).
- W7 (provenance) advantage repeats in all 5 reps; the mechanical sim
  is deterministic by construction and cross-rep equality is asserted.
- Only AIKOQL's answers are grounded (240/240 cite a source doc id);
  RAG answers are 0/250.
- Test: `wave31_agent_sim::w31_real_001_deterministic_agent_sim`.
  Gated real-LLM leg: `wave31_agent_sim_llm` (feature `answer_gen`,
  `AIKOQL_ANSWER_MODEL`) — same chain with a live generator, and the
  generation-retry surface the deterministic slice structurally lacks.

### Real-LLM leg (llama3.1 via ollama, 50 tasks × 5 reps, 2026-08-29)

Same chain, live generator. The first run was discarded: the endpoint
was the bare ollama host, which 405s the chat API — 500/500 generation
failures (the endpoint fix is in the test; the discard is recorded,
not hidden).

| Column | AIKOQL agent | RAG agent |
|---|---|---|
| Task success (win-zone) | 182/500 | 163/500 |
| Grounded answers (cite sources) | 43/210 | 0/250 |
| Unsupported tokens in answers | 3212 | 5452 |
| Generation retries | 0 | 0 |
| W11 false-confidence | 5/30 | 30/30 |
| W7 advantage per rep | 3/5 × 5 reps | 0/5 × 5 reps |

- The live generator is the weak point of both chains — 2-unit win-zone
  success is a high bar for llama3.1's free generations; the advantage
  (182 vs 163) is evidence delivery, and W7 repeats 3/5 in all five
  reps vs RAG 0.
- Aikoql's refusals hold with a live generator: the W11 traps refuse
  25/30 (the 5 are the false-confidence rows); RAG has no refusal
  surface and answers all 30.
- Unsupported tokens: 3212 vs 5452 — the real generator emits
  unsupported tokens on both treatments; aikoql's packs are leaner and
  grounded.

## Wave 3.1 evidence-to-decision + temporal answer (W31-DEC-001 / W31-TEMP-001)

Predefined acceptance (written before first measurement): DEC-001's five
items each judged independently; TEMP-001's four dimensions each judged
independently — no aggregate can hide a failure. Verdict: **all true,
2/2 on all four TEMP dimensions**.

W31-DEC-001 (deployment-window scenario, kernel supersession lineage
v1→v2→v3 + live conflicting runbook):
- current authoritative evidence selected — the decision leads with the
  v3 policy statement; superseded v1/v2 statements appear nowhere;
- historical evidence preserved — superseded claims still return from
  the kernel with statements and properties intact (valid_to stamped);
- material conflict disclosed — the runbook's conflicting statement is
  delivered as an explicit conflict;
- unsafe instruction rejected — both action probes Refused by both the
  sim policy and the decision script (no Act arm);
- decision supported by evidence — every decision statement comes from
  the package, and the source doc is cited.

W31-TEMP-001 (retry-limit timeline v1/v2/v3, four spec questions):

| Dimension | AIKOQL | Graph-RAG | RAG |
|---|---|---|---|
| historical (Feb) | 2/2 grounded | 1/2 | 1/2 |
| current (now) | 2/2 grounded | 1/2 **stale** | 1/2 **stale** |
| change | 2/2 | 2/2 | 2/2 |
| why | 2/2 | 2/2 | 2/2 |

- On "what is the retry limit now?" both RAG treatments deliver the
  superseded statements as current (stale=true) — the G11 temporal
  finding reproduced at agent-answer level; AIKOQL's validity boundary
  excludes them (zero stale statements on every dimension).
- Only AIKOQL answers are grounded (cite a source); raw chunks carry no
  citations, so the RAG rows miss the evidence unit on historical/current.
- Production fix driven by this test: the validity boundary now filters
  entity mentions whose text is a stale statement
  (`compile_context_semantic_with`), with the pinned unit test
  `stale_mentions_are_suppressed_by_validity_filter`.
- Tests: `wave31_decision::w31_dec_001_evidence_to_decision_correctness`,
  `wave31_decision::w31_temp_001_historical_vs_current_agent_answer`.

## Wave 3.1 build-vs-buy, corrected (W31-DEV-001)

Predefined acceptance (written before first measurement): functional
parity on the shared battery, application-owned complexity strictly
less than the conventional stack's, the conventional app's custom
code genuinely exercised, the spec's human-time columns printed n/a
with deterministic ops proxies. Verdict: **all true** — parity 6/6,
app-owned totals 88 vs 153 LOC.

The Wave 3 report compared a 1,042-LOC retrieval baseline against the
engine's 9,410-LOC surface — the spec's point: that does not prove
developer productivity. Two equivalent applications over the same
scenario (deployment-window conflict, retry timeline, two-day
capacity/ftp memory), measured by source span (`line!()` windows) —
engine-internal LOC excluded by construction, never referenced:

| Capability | Conventional app LOC | AIKOQL app LOC |
|---|---|---|
| config | 11 (8 infra components) | 9 (1 component) |
| ingestion | 16 | 6 |
| retrieval | 52 | 2 |
| temporal | 23 | 65 |
| provenance | 8 | 0 |
| conflict | 16 | 0 |
| memory | 19 | 5 |
| **total (app-owned)** | **153** | **88** |

- Retrieval 52 → 2 (one validity-bounded compile), provenance 8 → 0
  (citations ride the payload), conflict 16 → 0 (the kernel discloses
  conflicts), memory 19 → 5, infrastructure 8 → 1.
- Both applications pass the same parity battery (6/6 probes, day 1
  and day 7, asserted per probe — no aggregate).
- The conventional app's custom code is real, not decorative: the
  conflict handler restores a dropped counterpart (micro-assert) and
  the transcript scrub is asserted through the day-7 forbidden check.
- Measurement is fmt-stable: the AIKOQL impl carries
  `#[rustfmt::skip]` — spans are the instrument, a reflow must not
  reprice them (unpinned, fmt added +44 LOC of formatting alone).
- Test: `wave31_dev::w31_dev_001_application_owned_complexity`.

## Wave 3.1 epistemic boundary (W31-UNK-001)

Predefined acceptance (written before first measurement): each of the
four behavioral probes lands in its mapped outcome, asserted
individually — no aggregate. Verdict: **all four landed**.

- known → answer (the current retry claim delivered, cited, zero
  unsupported tokens);
- unknown → refuse (healthy EMPTY pack — insufficient evidence);
- conflicting → disclose (BOTH current deployment-window statements
  delivered, neither superseded statement present);
- historical-only → not present as current (the superseded TLS claim
  appears nowhere, no current fact anchors the entity, the entity
  bullet carries no mention).

Measured rates over frozen batteries (the spec says "measure" — no
threshold is invented):

- false-confidence: aikoql **3/15** vs rag 15/15 on the union-corpus
  unknown-probe battery (W11) — down from the pre-gate 13/15;
- incorrect-current: aikoql 0/3 (asserted) vs rag 3/3 on the temporal
  battery — RAG delivers the superseded claims as current (scenario
  pin asserted, W3-CONF-001 convention);
- unsupported-assertion: aikoql 0 by construction (deterministic
  echo) — the real rate is the gated LLM leg's (REAL-001).

The drop is the epistemic coverage gate (production change, TDD item
10): on lexical-only compiles, when ranked evidence fails to explain
more than half the question's content tokens — `token_match` or the
≥4-char shared-prefix inflection band covering ≥2/3 of the word —
and no fact is content-anchored by ≥2 exact tokens, the package is
emptied and the agent refuses. The remaining 3/15 are the gate's
honest lexical ceiling: they sit in the exact tie zone that also
holds two frozen Wave 3 pins whose packs are asserted ("How is
rollback done?", "What do deploys require?") — the ties are
lexically indistinguishable (semantics-only separation, see
losses.md / unknown.md). A root-cause fix rode the gate: `ident_parts`
dropped the camelCase boundary capital (`"AlertThreshold"` →
`["Alert","hreshold"]`), so suffix parts never matched — MEM-001's
"What is the alert threshold?" refused all 90 days until fixed (30/30
pinned).

Historical preservation held: the retired claim is still readable
with valid_to stamped; its successor is current. Test:
`wave31_unk::w31_unk_001_four_state_epistemic_boundary`.

## Wave 3.1 longitudinal agent (W31-MEM-001, 90 days)

The spec's six introduction types — new facts, superseded facts,
corrections, contradictions, new relationships, deletions — over five
checkpoints (days 1/7/30/60/90), three treatments on one agent.

| Column | AIKOQL | RAG | Conv. history |
|---|---|---|---|
| Task success | 30/30 | 29/30 | 25/30 |
| Memory accuracy (zero stale statements) | 25/25 | 17/26 | 6/30 |
| Stale-memory rate | 0/25 | 9/26 | 24/30 |
| Context tokens day1→90 (cap lane / hist) | [35,43,43,43,43] / [45,128,237,300,300] | n/a | [45,128,237,300,300] |

- AIKOQL tokens stay flat through three capacity supersessions
  (100→200→500→900) and a deletion; the history transcript grows and
  plateaus at the 300-token budget — crowded with the stale
  statements it cannot drop (24/30 answers carry them).
- Day-90 deletion lane: AIKOQL refuses (kernel tombstone + doc
  dropped); RAG answers the retired fact — its only task miss, and a
  stale one.
- Kernel history preserved through all six transitions: every claim
  readable, capacity current (valid_to none), the ftp claim
  `LifecycleState::Deleted`; retention 1.0; evidence 25/25 cited.
- Judge fairness: citation required only of AIKOQL (the chunk-text
  proxy carries no doc ids — G11 convention); the baselines'
  evidence-retention column is printed n/a, not scored on an
  artifact.
- Test: `wave31_mem::w31_mem_001_longitudinal_agent`; gated LLM leg
  behind `answer_gen` + `AIKOQL_ANSWER_MODEL` (same world, print-only
  totals).

## Wave 3.1 cost per successful task (W31-COST-001)

Five-term cost ÷ successful tasks over the declared scope (148 tasks,
all 12 workload classes), frozen judge, declared rates:

| Treatment | Cost | Success | Cost/success | Failure rate | Tokens/success |
|---|---|---|---|---|---|
| aikoql | $0.16035 | 121/148 | $0.00133 | 18.2% | 277 |
| graph-rag | $0.68078 | 82/148 | $0.00830 | 44.6% | — |
| rag | $0.53170 | 75/148 | $0.00709 | 49.3% | — |

- AIKOQL cost per success is 5.3× below rag and 6.2× below graph-rag
  on the totals; aikoql's cost is strictly lower in 10/12 classes (the
  two exceptions: baselines with 0 successes → n/a, see losses).
- Re-measured 2026-08-29 after the gate: $0.16208/120 → $0.16035/121
  (the gate's correct refusals net +1 task success, failure rate
  18.9% → 18.2%). Verdict unchanged: cost strictly lower in 10/12
  classes, the universal cost-leadership claim stays denied by the
  acceptance gate.
- Composition: llm $0.01235 + infra $0.14800 + embed/retrieval/agent
  $0. The kernel's deterministic index pays one component and no
  per-query retrieval — the infra term carries the compute (the
  product claim, declared in the test header).
- The test asserts the report's internal consistency (total == Σ
  per-class, per treatment) and computes the verdict from the table —
  never rigged.
- Test: `wave31_cost::w31_cost_001_cost_per_successful_task`.

## Wave 3.1 memory compression (W31-MEM-002, day 90)

Three memory treatments over the same 90-day world — the spec's
columns measured, primary metric = correct tasks per 1000 retained
tokens:

| Column | Raw transcript | Summarized | AIKOQL structured |
|---|---|---|---|
| Memory size (tokens) | 300 (budget-bound) | 1871 | 232 (served context) |
| Task success (day-90 battery) | 5/6 | 5/6 | 6/6 |
| Fact retention (day-1 facts) | 1/4 | 4/4 | 4/4 |
| Relationship retention | yes | yes | yes (current) |
| Conflict retention (sev1 pair) | no | yes | yes (current) |
| Primary metric (correct/1000 tok) | 16.7 | 2.7 | 25.9 |

- Structured memory is the cheapest AND the only 6/6: the kernel
  serves the query-scoped current view (232 tokens across the six
  tasks) while keeping every claim readable — day-1 facts, the day-7
  relationship, the day-60 conflict pair all readable and current.
- The summarized memory (kernel `summarize_conversation`, §38/39) is
  the retention winner per token of writing but the most expensive to
  hold — verbatim seven-bucket extraction, no compression.
- Raw history loses the day-1 facts (1/4) and the conflict pair —
  oldest-first truncation drops exactly the early facts.
- Provenance: structured cites a doc on 6/6 answers (kernel evidence
  rides every current answer); summarized keeps doc citations only as
  sentence fragments (measured honestly).
- Test: `wave31_mem2::w31_mem_002_memory_compression`.

## Wave 3.1 knowledge-complexity scaling (W31-SCALE-001)

1K / 10K / 100K synthetic units (entities + facts + relations), 12
probes × 3 reps per scale, debug build:

| n | Retrieval work | Task success | Context/probe | p50 | p95 |
|---|---|---|---|---|---|
| 1,000 | 2,800 records | 36/36 | ~187 tok | 131 ms | 181 ms |
| 10,000 | 28,000 | 36/36 | ~187 tok | 949 ms | 1.9 s |
| 100,000 | 280,000 | 36/36 | ~179 tok | 9.5 s | 18.4 s |

- Success holds at 36/36 through 100× knowledge growth; context per
  probe is flat (~4 words of content + the probe's neighborhood) and
  the absent-entity trap returns a true empty pack at every scale.
- Scaling is near-linear per 10× (7.2× then 10×) — the O(n²) entity
  lookups this test exposed were fixed (see losses) and the measured
  curve confirms the index change.
- Latencies are debug-build numbers; the production numbers live in
  the R14 criterion bench (`benchmarks/benches/scale.rs`, knob
  `AIKOQL_BENCH_SCALE`, default 100k, 1M ≈ 4GB) — the test asserts
  the pointer exists so the 1M row cannot rot silently.
- Test: `wave31_scale::w31_scale_001_knowledge_complexity_scaling`.

## Wave 3.1 falsification wins (W31-NEG-001)

- **Single-source query packs 32 vs the plain scan's 38 tokens** at
  equal 2/2 delivery — the kernel serves exactly the fact; the plain
  baseline packs the whole doc chunk. Classified "win" by the pinned
  law (equal units, no more tokens). **This is a 2-token noise
  margin — no claim rests on it**; the other three mandated scenarios
  are honest no-advantage rows (parity.md). Recorded because the law
  says win, not because it matters.

## Wave 3.1 end-to-end debuggability (W31-DEBUG-001)

Six injected failure classes, six root causes identified using normal
AIKOQL observability only (compiled ContextPackage per-item evidence,
`kernel.get`, corpus text search — nothing test-private):

| Injected failure | Root cause identified | Ops |
|---|---|---|
| wrong source | packed fact's evidence doc ≠ doc its text lives in | 2 |
| stale source | kernel claim disagrees with current corpus doc | 2 |
| wrong relationship | packed relation contradicts its own evidence doc | 2 |
| missing evidence | packed fact carries no source doc | 1 |
| conflicting evidence | two packed facts, different docs, disagree | 1 |
| incorrect context | packed fact shares no meaningful token with task | 2 |

- The deterministic Wave 3 kernel-state experiment diagnosed 5/5
  injected failures; this validates the *application-level* path — the
  diagnosis runs on the same surface an app developer sees.
- Diagnosis cost: 1–2 observability operations per root cause; the
  provenance carried on every packed fact (`RankedFact.evidence`) is
  what makes wrong-source/stale-source/conflict diagnosable at all.
- Test: `wave31_debug::w31_debug_001_end_to_end_debuggability`.

## Wave 3.1 knowledge change propagation (W31-IMPACT-001)

Dependency change — Aurora depends on Beacon becomes Aurora depends on
Cobalt. Re-ingest the edited doc, recompile the same tasks, measure
what the change touched. Ground truth and pins declared in the test
header BEFORE measurement (spec §4):

| Metric | Pinned | Measured |
|---|---|---|
| knowledge records changed | 2 (1 fact + 1 relation) | 2 |
| affected relationships | 1 edge replaced | 1 |
| impact precision | 1.0 | 1.0 |
| impact recall | 1.0 | 1.0 |
| false propagation | 0 | 0 |
| missed propagation | 0 | 0 |
| stale answers | 0 | 0 |

- The changed edge re-routes the relationship-boost (0.65 × anchor
  score): post-change the new neighbor Cobalt ranks and its sla fact
  passes the entity gate, while Beacon's fact drops back out — the
  two-hop answer ("sla of the service Aurora depends on") flips
  99.9 → 99.5 with no re-prompting, no cache invalidation, no
  migration. The propagation IS the recompile over the new state.
- Blast radius is exactly the changed records: T1/T2 (edge + fact) and
  T3 (Beacon's incoming edge leaves its context) change; the unrelated
  control T4 (Dune) compiles to a byte-identical payload.
- A touched entity's own knowledge survives its neighbor's change: T3
  still answers Beacon's sla after losing the incoming edge.
- Test: `wave31_impact::w31_impact_001_knowledge_change_propagation`.

## Wave 3.1 OSS time-to-value (W31-OSS-001)

Fresh-developer contract — README + quickstart + examples only, then
the seven mandated tasks. Mechanical legs driven over the real MCP
binary exactly as the quickstart describes:

| Task | Done |
|---|---|
| install (released binary) | yes |
| start | yes |
| ingest | yes |
| query | yes |
| add second source | yes |
| knowledge-backed agent | yes |
| debug (explain + trace) | yes |

- Completion 7/7, whole mechanical flow 1.37 s (debug profile);
  details and the baseline table in oss-time-to-value.md.
- The measurement caught three real onboarding failures and each was
  closed: README.md missing (created), examples/ missing
  (`hello-agent.ts` created, tool names pinned to the registry),
  MCP rate limit undocumented (config section added to QUICKSTART).
  Support interventions after the fixes: 0.
- Pinned law (`w31_oss_002`): the artifacts exist at the mandated
  paths, the README leads to the other two, the quickstart covers
  all seven tasks, the example uses tools the server actually has.
- Test: `wave31_oss` (crates/services/api/mcp).
