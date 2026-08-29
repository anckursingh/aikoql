# Wave 3.1 benchmark bias audit (W31-BIAS-001)

For every headline result the spec asks: *could this test have been
deliberately constructed to make AIKOQL win?* Dimension by dimension,
with the counter-measures. The checkable structural laws are pinned in
`wave31_bias.rs` (green: zero leaks, sets disjoint); this is the
judgment half.

## 1. Question construction

- **Law (pinned):** no answer unit's tokens are fully contained in its
  own question's tokens — a question carrying its own answer makes
  every treatment trivially correct. Zero violations across 148 union
  + 24 holdout tasks.
- **Measured ceiling:** max question↔answer token overlap is 0.80 in
  one case ("Who processes card payments and is PCI-DSS certified?") —
  lexical adjacency on a lookup task, not a leak (the units are not
  fully contained). No task hits 1.0.
- Unknown-probe inversion means silence scores for W11 tasks. A rigged
  bench would make *everything* an unknown-probe so refusing always
  wins; W11 is 1 of 12 classes (~14% of tasks).

## 2. Corpus construction

- **Honest admission:** the corpus IS constructed to exercise the
  kernel's claimed strengths (multi-hop, temporal, contradiction,
  provenance-heavy classes dominate). Constructible-to-favor: **yes**.
- **Counter-measures (the spec's mandated response):**
  - W1 lookup control class — full parity required across all three
    treatments or the comparison test FAILS (28/28 held).
  - NEG-001's four mandated adversarial scenarios, judged against a
    plain keyword baseline — 3/4 measured no-advantage.
  - Frozen holdout (24 tasks) gets exactly one pass, numbers pinned in
    the evidence docs, zero scoring assertions in dev.
  - Every fact/unit verbatim-backed by chunk text (`assert_integrity`)
    — AIKOQL gets nothing the RAG baseline could not in principle
    retrieve.

## 3. Baseline implementation

- The baselines run mock (deterministic token-overlap) embeddings. For
  synthetic corpora with verbatim answers, lexical overlap is the
  strongest signal a retriever can have — real embeddings could only
  be weaker on lookups, so this choice favors the *baselines*, not
  AIKOQL.
- Same 300-token budget, same judge, same corpus, single shared
  measurement code path (`wave31_sim::measure_task`) — a baseline
  cannot be accidentally disadvantaged in one place and advantaged in
  another.

## 4. Prompt wording

- No prompt exists: the mechanical slice judges the *payload* the LLM
  would receive, not generated text. No prompt-injection bias is
  possible; the honest cost is no generation-quality signal either
  (unknown.md). The real-model leg (REAL-001) is env-gated, prints
  totals, and asserts no scoring.

## 5. Evaluation criteria

- Frozen `units_hit` judge, win-zone contract, applied identically to
  all treatments. Verdicts are computed from measured tables by
  classifiers pinned BEFORE measurement (COMP strong-fit/regression
  bounds, COST's cheaper-claim gate, NEG's win/loss law) — never
  reclassified after the fact.
- Wins, parity, losses, and unknown rows are all kept. A claim gate
  that refuses to record losses is the primary rigging vector; it does
  not exist here (NEG-001).

## 6. Data leakage

- Dev and holdout doc sets are disjoint (pinned). Holdout questions
  run once, printed, pinned in docs — no threshold may live in a dev
  assertion (that would leak holdout signal into development).
- No question names a corpus doc id (pinned — zero violations).

## 7. AIKOQL-specific optimization

- The only production change made during Wave 3.1 measurement is the
  name→score index in `context.rs` — discovered BY SCALE-001's 100K
  world (measured O(n²), fixed, near-linear curve re-measured in
  losses.md). It is semantics-preserving (highest-scoring duplicate
  wins, matching the old first-hit on the score-sorted list) and
  changes retrieval speed, not what the judge sees.
- No judge, budget, or fixture was touched to fit a measurement.

## Verdict

Constructible-to-favor: **yes — the corpus exercises the kernel's
claimed strengths.** Per the spec's mandated response, counter-tasks
exist and are pinned (W1 control, NEG-001 four scenarios, disjoint
holdout), so the headline results are usable as **scoped** public
claims — not as universal ones. No headline result is reclassified
exploratory; none is presented without its class scope.
