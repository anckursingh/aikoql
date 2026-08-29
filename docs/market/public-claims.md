# Public Claim Approval (spec §10)

One rule above the table (spec §10): a benchmark result is **never**
converted into a universal product claim. Every claim below is scoped
to its evidence, and a claim that the evidence does not support is
marked NOT PROVEN rather than reworded. The certification claim-word
ban (W3-G05: "better than rag", "cheaper", "faster", "eliminates
hallucination" flat-banned from evidence docs) enforces this at the
chain level.

| Claim | Required evidence | Status | Scope / caveat |
|---|---|---|---|
| Persistent knowledge | longitudinal benchmark | **APPROVED (scoped)** | MEM-001 longitudinal agent + Wave 2 continuity suite (CONT-001) |
| Temporal knowledge | temporal agent benchmark | **APPROVED (scoped)** | TEMP-001 green; baselines measured packing stale-as-current |
| Multi-hop reasoning | comparative multi-hop benchmark | **APPROVED (scoped)** | COMP-001 multi-hop classes strong-fit; depth-2 leaf ceiling kept in losses.md |
| Evidence-backed answers | provenance benchmark | **APPROVED (scoped)** | Payload-level provenance (source/confidence/verified); §32: baseline provenance 1/4 vs AIKOQL 20/20 |
| Conflict-aware knowledge | decision benchmark | **APPROVED (scoped)** | DEC-001 green; W6 contradiction is only Δ1 (losses.md) |
| Unknown-aware AI | epistemic benchmark | **APPROVED (scoped, with caveat)** | UNK-001 boundary holds at the exact-token gate; false confidence is NOT zero (losses.md W11) |
| Lower developer complexity | valid build-vs-buy | **APPROVED (scoped)** | DEV-001 app-owned LOC per capability; temporal bookkeeping is a measured loss row |
| Easier multi-source integration | source-expansion study | **APPROVED (scoped)** | OSS-001 second-source leg over the real MCP binary |
| Lower cost | cost/success benchmark | **APPROVED (scoped)** | 10/12 classes; the universal cost-leadership claim is **DENIED** (W2/W7 n/a denominators) |
| Faster | controlled latency comparison | **NOT PROVEN** | No controlled latency comparison exists; NEG-001 measured the kernel 3.5–6× *slower* on trivial lookups (kept in losses.md) |
| Better than RAG | scoped workload comparison | **APPROVED (scoped)** | COMP-001: 9 strong-fit classes, 0 regressions, W1 control parity; explicitly not universal |
| Better for agents | real-agent benchmark | **PARTIALLY EVIDENCED** | Deterministic agent-chain sim green; live-LLM leg env-gated (unknown.md) |
| Production-ready | separate production-readiness assessment | **APPROVED** | PRR 1–8 (Docker/GHCR, npm live, encryption-at-rest, v0.3 reality check) |

## Claims a marketing page may carry today

Scoped forms only:

- "On a 148-task, 12-class knowledge-workload corpus, AIKOQL's payload
  hit 258/296 units vs 191 (graph-rag) and 181 (rag), with 9 classes
  at strong fit and the lookup control at full parity."
- "Cost per successful task was 5.2× below rag and 6.1× below
  graph-rag across the 10 classes with comparable success
  denominators."
- "A build-vs-buy with two equivalent apps measures less
  application-owned LOC for retrieval, provenance, conflict handling,
  and infrastructure on the AIKOQL side."
- "Two independent clean-environment passes reproduced the headline
  numbers (direction and conclusions asserted)."

## Claims that must NOT appear

Universal better-than-RAG, universal cheaper, universal faster,
eliminates hallucination — each is banned by W3-G05 and would also be
false (see the DENIED/NOT PROVEN rows above and losses.md).
