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
