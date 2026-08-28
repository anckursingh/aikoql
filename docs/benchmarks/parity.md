# Wave 3 — parity (measured)

Workload classes where AikoQL and the lexical RAG baseline tie on the
token-containment judge. Kept per plan §29 — parity is evidence, not a
reason to drop the class.

## Win-zone classes at parity

| Class | AikoQL | RAG | Notes |
|-------|--------|-----|-------|
| W1 control | 2/2 | 2/2 | By design — the rig check (`w3_win_001` asserts W1 full parity, or the bench is rigged in AikoQL's favor) |
| W3 synthesis | 2/2 | 2/2 | Single-doc cross-entity synthesis: answer tokens live in chunks both treatments can reach; AikoQL costs fewer tokens (75 vs 145) |
| W5 temporal-probe | 4/4 | 4/4 | Corpus-level probe: both claims current in the static corpus, nothing superseded — the RAG failure mode needs a **timeline**, measured in W3-TEMP-001 |
| W6 contradiction-probe | 2/2 | 2/2 | Both claims present: static-corpus contradiction — the value is measured in W3-CONF-001 |
| W9 policy | 2/2 | 2/2 | Policy extraction parity; AikoQL costs more tokens (349 vs 248) — the entity cluster packs whole |

Source: `w3_win_001_workload_classification` table output.

## Cost (G12 comparative)

- AikoQL retrieval is deterministic with 0 LLM calls (compile path); RAG
  wins on raw payload bytes by less than an order of magnitude, but pays
  one LLM turn per question. At G12 rates the per-query cost delta is
  measured in the G11/G12 table outputs; the honest reading: cost is
  parity-competitive, not a headline win (0.600 deterministic per the
  G12 bench).

## Answer quality

- Hallucination rate 0.0 for **all** treatments by construction (every
  payload copies corpus text verbatim) — the generative step is out of
  the substrate and measured only by the optional `answer_gen` seam
  (§53). AikoQL's edge is payload quality, not this parity row.

## Fairness of the corpus

- `w3_mkt_001_market_corpus_integrity`: every answer unit is
  verbatim-backed by chunk text (doc-id units by IR evidence), so the
  RAG baseline could in principle win every question. Parity rows above
  are not rigged losses for RAG.
