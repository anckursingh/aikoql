# Wave 3 — unknown (unmeasured, by design or by scope)

Claims that cannot be made from the current evidence set. The release
narrative must not state these as facts.

## Unmeasured: real-model answer quality

- Hallucination rate, groundedness of generated answers, and
  answer-correctness with live LLMs are outside CI: the `answer_gen`
  seam (§53) exists behind the optional feature and its harness is not
  run in the default pipeline. All Wave-3 accuracy numbers are payload
  (evidence-delivery) measurements, not answer measurements.

## Unmeasured: real embeddings

- PR-P NO-GO: real Ollama embeddings stay behind `remote_emb`. The
  semantic-fallback path (`SemanticFallback` status) is tested with the
  mock; real-embedding recall quality is unknown.

## Out of substrate scope (2026-08-25 directive)

- OCR (DOC-002), agent planning loops (W10), working-memory
  compression by paraphrase (needs a lossy LLM step — §40 measured the
  verbatim re-format at ratio 1.07 and said so).

## Unknown: workloads beyond the 12 classes

- W8 (personal memory) is covered by the §32 bench; W12 by W3-LONG-001;
  W10 out of scope. No measurement exists for adversarial/poisoned
  corpora or non-English content.

## Unknown: longitudinal beyond 90 days

- W3-LONG-001 covers 4 checkpoints/90 days on one claim lineage.
  Multi-year retention, many-lineage interference, and retention-policy
  interactions at scale are unmeasured.

## Unknown: scale-to-value

- The 51-task G10 scale and the dogfood gate are the largest runs.
  Production-scale retrieval latency/memory (millions of KOs) is
  unmeasured; the plan's scale-to-value experiment is future work.


## Wave 3.1 comparison unknowns (W31-COMP-001)

- **W2 semantic probes**: AIKOQL reaches 8/22 units on zero-overlap
  probes via the entity graph (RAG 0/22 by construction); the other
  14/22 need a real embedding provider — measured only by the gated
  real-model harness (REAL-001).
- **Latency at scale**: p95 AIKOQL 74.9ms vs RAG 6.8ms on 148 tasks;
  no measurement at production scale (millions of KOs) — SCALE-001's
  job.

## Wave 3.1 agent-chain unknowns (W31-REAL-001)

- **W11 false-confidence 25/30**: on unknown-probe tasks the AIKOQL
  agent answered with a healthy non-empty pack 25 of 30 times — fewer
  than RAG (30/30, it has no refusal surface), but each one is a trap
  delivered. UNK-001 (#164) is the formal measurement of these rates.
- **Retrieval retries are structurally 0** in the mechanical slice:
  `compile_context` is deterministic, so a re-query returns the same
  package; the SemanticFallback status (and its refusal arm) only
  arises via `compile_context_semantic` with embedding scores, which
  the mechanical treatment doesn't run. The real-LLM leg measures
  generation retries; the fallback boundary is only measurable with an
  embedding provider deployed.
- **Refusal boundary reach**: 10 aikoql refusals are all empty-pack
  refusals — the policy's fallback arm is not exercised by the
  deterministic sim (see above). No false refusals observed on the
  240 answered tasks.

## Wave 3.1 decision/temporal unknowns (W31-DEC-001 / W31-TEMP-001)

- **Merged-entity citation tags name the first-merged doc**: the
  compiled entity cites `kb-retry-v1` (merge-order first) even after
  the validity boundary drops the v1 mention — per-mention document
  provenance is not in the IR, so the evidence unit is judged at the
  doc-family level ("kb-retry"). A per-mention doc id would be an IR
  change; not needed for MVP.
- **Authority selection is a scripted policy here**: the decision
  script prefers Policy-anchored facts via type_hint; the substrate
  supplies the signals (type_hint, statements, validity boundary), the
  agent-layer judgment is the caller's. Measured as such, not claimed
  as a substrate feature.

## Wave 3.1 epistemic/longitudinal unknowns (W31-UNK-001 / W31-MEM-001)

- **Unsupported-assertion with a live generator is unmeasured here**:
  the deterministic echo is 0 by construction; the real rate is the
  gated LLM leg's column (REAL-001).
- **Baseline evidence retention is n/a**: the chunk-text proxy
  carries no doc ids (G11 convention), so scoring it would rig the
  baseline on an artifact — printed n/a, not measured.
- **Developer-intervention count n/a**: the sim's world updates are
  scripted (corpus + kernel ops on a fixed schedule), so no
  intervention count exists to compare.
- **MEM-001's contradiction is a documentation pair, not a kernel
  CONTRADICTS edge**: both sev1 claims are current with equal
  authority, so the pack discloses both; the kernel-edge conflict
  path is measured by the UNK-001 conflicting probe instead.

## Wave 3.1 build-vs-buy unknowns (W31-DEV-001)

- **Developer hours / defects / time-to-add-source / time-to-change-
  rule are n/a in deterministic CI**: a CI run has no human
  developers, so the test prints n/a with deterministic ops proxies
  (defects: the 6/6 parity battery on both apps; add-source: one
  call each; change-rule: conv 3 statements vs aikoql 4, one
  callsite each). The spec's time columns are only measurable with
  real developers on a real project — not faked here.

## Wave 3.1 cost unknowns (W31-COST-001)

- **All rates are declared conventions, not measurements**: LLM
  pricing (G11/G12 convention), $0.02/M embedding, $100/component/
  100k infra, $0.0005/query retrieval, $0 agent/tool. The comparison
  is as good as its rate table; real provider pricing would move the
  embedding terms (the corpus-scale embedding passes dominate the
  baselines' embed row).

## Wave 3.1 memory-compression unknowns (W31-MEM-002)

- **Answer quality with a live generator is unmeasured here**: the
  mechanical echo cannot prefer one memory's contents over
  another's; the primary metric is correct-tasks-per-token on
  delivered units, not generation quality per treatment.
- **Summarized-memory retention is a property of the verbatim op**:
  the §38/39 summarizer extracts, it does not compress or
  rephrase — a compressing summarizer (LLM) stays out of substrate
  scope, so "compression" here means structural memory, not shorter
  prose.

## Wave 3.1 scale unknowns (W31-SCALE-001)

- **The 1M row is a pointer, not a run**: `benchmarks/benches/scale.rs`
  (criterion, ~4GB at 1M) is asserted to exist and carry its knob;
  the 1M numbers themselves are not measured in CI.
- **Latencies are debug-build**: the per-probe p50/p95 above are
  unoptimized-test-profile numbers; production latencies belong to
  the release criterion bench (R14), which this test points at.
- **"KO" means retrieval-index records here**: the synthetic unit is
  an entity+fact+relation candidate (the IR surface), not a kernel
  KO — declared in the test header so the row's unit is exact.

## Wave 3.1 OSS time-to-value unknown (W31-OSS-001)

- **Human time-to-value is unmeasured**: the baseline bounds only the
  mechanical floor (≈1.4 s of tool legs, 0 s install via released
  binary, 12m49s from-source debug build). A real human-time target
  needs a real fresh developer; none was available, so no wall-clock
  target is set — the spec's rule (targets from baseline, never
  invented) forbids picking one anyway.
- **The flow's ingest leg is `remember`, not `document_ingest`**: the
  doc-pipeline path (D1–D9) has its own evidence surface (TP3, the
  Studio Documents panel); this measurement covers the knowledge-CRUD
  agent flow only.
