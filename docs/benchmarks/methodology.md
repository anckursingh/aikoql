# Wave 3.1 evaluation methodology

The frozen evaluation contract every Wave 3.1 headline number follows.
Written once, applied everywhere — thresholds are never adjusted to fit
a measurement (spec §4 TDD operating rule).

## Judge

- `units_hit`: a task delivers if all tokens of each of its 2 answer
  units appear in the payload (0–2 units per task).
- Unknown-probe inversion: for `kind == "unknown-probe"` tasks the
  score is `2 - units_hit` — silence is correct (the UNK-001 refusal
  boundary).
- Groundedness proxy: the payload cites at least one corpus doc id
  (ids tokenize at `-`).

## Budget

300 tokens for every treatment, every task (G12 convention). The
ambiguity render is deliberately unbudgeted (honesty over truncation —
recorded in losses.md).

## Treatments

| Treatment | Definition |
|---|---|
| AIKOQL | merged IR → `compile_context` → markdown render |
| Graph-RAG | embedding rank → budget pack → transitive entity expansion |
| RAG | embedding rank → budget pack |
| Plain (NEG-001 only) | keyword-overlap rank → budget pack |

## Corpus rules

- Every fact/unit verbatim-backed by chunk text (`assert_integrity`) —
  the RAG baseline could in principle win every question.
- 12 workload classes W1–W12; ≥100 tasks; ≥20% multi-source, ≥20%
  relationship-dependent, ≥10% temporal/contradictory/unknown.
- Holdout: exactly one pass (`wave31_comparison::w31_comp_002`),
  printed and pinned into the evidence docs — no scoring threshold may
  live in a dev assertion (that would leak holdout signal).

## Cost convention (G11/G12)

Input $0.15/M, output $0.60/M, 100 answer tokens per answered query;
embedding $0.02/M corpus tokens (treatments that embed); infra
$100/component/100k tasks; retrieval $0.0005/query (vector treatments);
agent/tool calls $0 (mechanical slice). Rates are declared conventions,
not provider measurements (unknown.md).

## Honesty rules (NEG-001 law, generalized)

- Verdicts are computed from measured columns by a pinned classifier —
  never reclassified after the fact.
- AIKOQL must be allowed to lose: simple-workload scenarios run against
  a plain keyword baseline; wins/parity/losses/unknown are all kept.
- A "win" on equal delivery requires no more tokens; equal delivery
  with more tokens is "no-advantage", recorded as such.
- Latency is measured but never part of a headline claim (debug-build
  wall-clock).
