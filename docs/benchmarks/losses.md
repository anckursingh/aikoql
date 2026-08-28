# Wave 3 — losses (measured)

Places where AikoQL measurably loses, stays behind, or pays more. Kept
per plan §29 — never dropped, never spun.

## Semantic retrieval (W2) — class Unknown at 0/2

- `w3_win_001`: the semantic-probe question ("Who gets parts to buyers
  quickly?") is zero-overlap; the default build's mock embeddings score
  0.0, so AikoQL and RAG tie at 0/2 and the class is classified Unknown.
- Root cause: real embeddings stay behind the optional `remote_emb`
  feature (PR-P NO-GO verdict — the mock stays). Zero-overlap tasks are
  **unanswerable** in the default build. Honest ceiling, documented, not
  hidden.

## Unknown-probe false confidence (W11) — Good Fit, not Strong

- AikoQL delivers 1/2 trap units for "What is the rollback procedure for
  failed deploys?" — vocabulary overlap ("rollback"/"procedure") packs
  the policy cluster despite the specific answer being absent.
- W3-UNK-001 measured it worse on purpose: 5/5 facts delivered on an
  absent answer. The pack is **non-empty** — IR-level false confidence.
  The mitigations are the `status: healthy-empty` contract and the
  caller's epistemic discipline; the substrate does not silence it.
  This is the honest known gap in unknown-handling.

## Depth-2 multi-hop ceiling

- G11: D misses the depth-2 leaf fact (15/16) — the compiler's
  single-round relation boost stops at one hop. C (graph expansion)
  reaches it. Deliberate ceiling (documented in the G11 harness), not a
  regression.

## Extraction losses (G10)

- 9 misses at the 51-task scale: fenced-section prose carriers dropped
  until re-anchored to table cells/rules/bold bullets. Extraction is
  structure-bound; prose-in-fences is the documented weak shape.

## Token cost on control-class questions

- W1 control: 185 tokens vs RAG 70; W9 policy: 349 vs 248 — entity
  clusters pack whole. AikoQL over-delivers on simple questions; the
  budget pin (≤300) is the only brake, and cluster-level precision
  trimming is unimplemented (open item, not hidden).

## Drift sensitivity

- §32 T10: a post-canonical doc edit that grew one cell sentence past
  the pack boundary regressed retrieval until the sentence-boundary fix.
  The exact-token gate is boundary-sensitive — measured, fixed, and
  pinned by the bench.

## Answer generation

- AikoQL does not answer questions (by design — a knowledge store, not
  an answer engine). Every end-to-end answer-quality metric is a
  chatbot-layer measurement (optional `answer_gen` seam). In a market
  where buyers expect answers, this is a product-level loss to
  competitors that bundle generation; the positioning answer is the
  agent-interface (MCP) + deterministic payloads.


## Wave 3.1 comparison losses (W31-COMP-001)

- **Tokens on parity classes**: W8 personal — AIKOQL 297 vs RAG 71
  tokens for the identical 20/20 score. The compiler packs the entity
  neighborhood, not just the answer chunks; cost AIKOQL $0.0143 vs RAG
  $0.0137 at a 43% higher unit score. On trivial lookups the RAG pack
  is leaner.
- **Compiler alias gap (measured, kept)**: "What is the API rate
  limit?" (entity `PublicApi`) measured AIKOQL 0/2 on the first pass —
  the deterministic entity gate does not resolve semantic aliases
  (question token `api` ≠ entity token `publicapi`). The task was
  reworded to "What is the PublicApi rate limit?" as a W1-conformance
  correction; the 0/2 row stays here as negative evidence.
- **W6 contradiction is nearly parity**: AIKOQL 27/30 vs RAG 26/30
  (Δ1). Conflict handling helps, but on this corpus the RAG pack
  already carries the conflict sentences.

## Wave 3.1 decision/temporal losses (W31-DEC-001 / W31-TEMP-001)

- **RAG/Graph-RAG deliver stale as current**: on "what is the retry
  limit now?" both baseline treatments pack the superseded v1/v2 chunks
  (they share the question's tokens) — 1/2 with stale statements in the
  payload. The validity boundary is the AIKOQL-side fix; the baselines
  have no such instrument by construction.
- **The historical question needs the entity named**: the spec's
  phrasing "What was true in February?" shares one content token with
  the history fact, so the compiler's exact-token gate (≥2 content
  tokens or a ranked entity) yields a healthy empty pack. The corpus
  rule (lexical match should suffice, the W1 precedent) rewords it to
  "What was the retry limit in February?" — recorded here as the
  reachability ceiling of the gate, not silently.

## Wave 3.1 epistemic/longitudinal losses (W31-UNK-001 / W31-MEM-001)

- **False-confidence is not zero**: aikoql answered 13/15 unknown
  probes with an authoritative pack (rag 15/15) — the W11 trap docs
  still pack non-empty. The Unknown→Refuse boundary holds only when
  the exact-token gate yields an empty pack; vocabulary-overlap traps
  remain the known gap (same row as the W11 Good-Fit entry above).
- **Conversation history collapses under stale weight**: 25/30 task
  success and 24/30 answers carrying stale statements at day 90 —
  the budget keeps the oldest chunks, which are exactly the
  superseded ones. The baseline has no supersession signal.
- **RAG's one task miss is the deletion lane**: the day-90 tombstone
  works only if the dropped doc stops ranking — stateless RAG
  delivers the retired fact as current.
- **The historical-only probe is the presentation boundary, not a
  hard refusal**: the agent may still emit unrelated noise from other
  packed facts; the spec's "do not present as current" is asserted on
  the package, and the noise is counted in the false-confidence rate
  above, not hidden.

## Wave 3.1 build-vs-buy losses (W31-DEV-001)

- **Temporal bookkeeping costs more app code on the AIKOQL side**:
  65 LOC vs 23 — nine lineage operations through the kernel API
  (assert → supersede chains, claim-list maintenance, stale-set
  refresh) against chunk replacement + transcript scrub. The kernel
  lineage API is more verbose per knowledge-rule change than chunk
  surgery; the moat is retrieval/provenance/conflict/infrastructure,
  not temporal setup.
- **Marginal rule-change cost is near parity**: the ops proxy counts
  4 statements on the AIKOQL side (find claim, supersede, update
  claim list, refresh stale set) vs 3 conventional (remove chunk,
  insert successor, scrub transcript). The app-owned moat is the
  capabilities the conventional stack must build, not the marginal
  cost of one rule change — measured, not hidden.
