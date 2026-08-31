# Wave 3.1 QA Report — MVP-QA-003A

- **Spec:** docs/qa/WAVE3-1-TDD-TEST-SPECIFICATION.md
- **Branch:** feature/mvp-launch
- **Method:** §4 TDD loop (Hypothesis → Golden dataset → Baseline → RED →
  fix → GREEN → regression → raw evidence → comparative result →
  negative-result classification → claim approval)
- **Machine-readable matrix:** qa-wave3-1-results.json
- **Gate:** qa-wave3-1-release-gate.md — APPROVED (all 9 clauses)

## Result summary

18 tests: **13/13 P0 pass, 5/5 P1 pass.** Zero Sev-1, zero Sev-2.
Certification chain: MVP GO → Wave 2 GO → Wave 3 GO.

| ID | Pri | Verdict (one line) |
|---|---|---|
| W31-MKT-001 | P0 | Corpus frozen: 55 docs / 148 dev tasks / 12 classes / 24-task holdout |
| W31-COMP-001 | P0 | 258/296 units vs graph-rag 191 vs rag 181; 9 strong-fit, 0 regression, W1 control 28/28 |
| W31-REAL-001 | P0 | Deterministic agent-chain sim green; live-LLM leg env-gated, prints-only |
| W31-DEC-001 | P0 | Evidence-to-decision scenarios green on frozen judge |
| W31-TEMP-001 | P0 | Historical-vs-current green; baselines pack stale-as-current (losses.md) |
| W31-UNK-001 | P0 | Epistemic boundary holds at the exact-token gate; false-confidence rows kept |
| W31-MEM-001 | P0 | Longitudinal agent scenario green |
| W31-DEV-001 | P0 | Two equivalent apps, app-owned LOC per capability; moat asserted per row |
| W31-COST-001 | P0 | 5.2×/6.1× lower cost per success; 10/12 classes; universal claim DENIED |
| W31-REPRO-001 | P0 | Two independent passes identical on all mechanical columns |
| W31-BIAS-001 | P0 | Zero leaks, sets disjoint; constructible-to-favor: yes, counter-measures pinned |
| W31-NEG-001 | P0 | 4 mandated adversarial scenarios: 3 no-advantage kept, 1 noise-margin win |
| W31-REG-001 | P0 | Full workspace regression green; certify chain green |
| W31-MEM-002 | P1 | Structured memory 8× cheaper than summarized, wins the primary metric |
| W31-DEBUG-001 | P1 | 6/6 injected failures found with app-level observability, 1–2 ops each |
| W31-IMPACT-001 | P1 | Blast radius exactly the changed records; precision/recall 1.0, 0 stale |
| W31-SCALE-001 | P1 | 100k world near-linear after the O(n²) fix it exposed; 1M is a pointer |
| W31-OSS-001 | P1 | 7/7 onboarding tasks over the real MCP binary; 3 doc failures closed |

## The five questions (spec §13)

**1. Does AIKOQL win on sufficiently diverse real workloads?**

Yes, scoped. 12 workload classes, 148 dev + 24 holdout tasks, judged by
the frozen units_hit judge with a 300-token budget shared by all three
treatments. AIKOQL leads 258/296 units; 9 classes are strong-fit, 0 are
regression, and the W1 lookup control is full parity (28/28). The scope
limit is measured, not hidden: 10/12 classes for cost leadership (W2,
W7 have no comparable denominator), 18.9% own failure rate.

**2. Does that advantage survive a real LLM/agent?**

Evidenced at the payload level, gated at the generation level. The
mechanical slice judges exactly the payload an LLM would receive, so
the measured advantage is what a model has to work with; the
deterministic agent-chain sim (REAL-001) is green. The live-model leg
requires an API key the measurement machine does not have — it is
env-gated, prints totals, and asserts nothing, per spec. The honest
answer is "payload-level yes, generation-level unmeasured" (unknown.md).

**3. Does AIKOQL reduce application-owned complexity?**

Yes, with one measured exception. DEV-001 builds two equivalent apps
(conventional vs AIKOQL) and counts application-owned LOC per
capability — engine-internal LOC excluded by construction. The moat
pins hold per row (retrieval, provenance, conflict handling,
infrastructure). The exception is temporal bookkeeping, which costs
more app code on the AIKOQL side (65 vs 23 LOC; losses.md) — kept as a
measured loss, not smoothed over.

**4. Does the advantage survive cost/economic measurement?**

Yes, scoped. Cost per successful task is 5.2× below rag and 6.1× below
graph-rag; AIKOQL's cost is strictly lower in 10/12 classes. The
universal cost-leadership claim is DENIED by the acceptance gate
(W2/W7 n/a denominators) and recorded as such — the certification
claim-word ban (W3-G05) held the evidence docs to the scoped wording.

**5. Can an independent developer reproduce the evidence?**

Yes. REPRO-001 ran a clean-environment second pass: identical results
on all mechanical columns, with direction and conclusion assertions
(not equality assertions — the spec forbids asserting the exact numbers
in a rerun). Corpus, judge, budget, and recipe are frozen
(corpus-version.md, methodology.md, reproduction.md).

## Final QA decision

```text
PRODUCT QA APPROVED
for the declared release scope,
as scoped, reproducible product claims —
not as a universal-better-than-RAG verdict.
```

Per the spec's final ladder: technically validated OSS project →
validated product → defensible product thesis. Wave 3.1 supplies the
workload diversity, agent evidence, developer-value evidence, economic
evidence, independent reproducibility, and preserved negative evidence
the ladder demands.

## Remain-open rows (non-blocking, on the record)

Live-LLM leg (REAL-001), human wall-clock for onboarding (OSS-001),
1M-row scale run, zero-false-confidence unknown handling. Each is
recorded in unknown.md or losses.md with its reason.
