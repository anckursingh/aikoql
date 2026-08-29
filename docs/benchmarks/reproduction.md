# Wave 3.1 reproduction pack (W31-REPRO-001)

How an independent execution reproduces the **direction and conclusion**
of the headline result (spec §5 W31-REPRO-001: the direction, not the
exact numbers — latencies are excluded by design).

## Frozen inputs

| Item | Value |
|---|---|
| Dataset version | This repository's git HEAD — the corpus is versioned in-repo (`crates/ingestion/tests/common/trackb*.rs`). Corpus lineage: `docs/market/corpus-version.md` |
| Task set | 148 tasks, 12 workload classes W1–W12 (`trackb::QUESTIONS` + `MARKET_QUESTIONS_31`) |
| Judge | `trackb::units_hit` — token containment of 2 answer units per task, unknown-probe inverted (win-zone contract) |
| Budget | 300 tokens for every treatment (G12 convention) |
| Embeddings | `MockEmbeddingProvider` — deterministic |
| Model | **None** — this is the mechanical slice: the comparison judges the payload the LLM would receive, not generated answers. The real-model leg is REAL-001's harness, gated behind `answer_gen` + `AIKOQL_ANSWER_MODEL` |
| Hardware | Any — units/tokens/grounding/cost are fully deterministic; latency is excluded from the claim (debug-build wall-clock varies with load) |

## Treatments

| Treatment | Pipeline |
|---|---|
| AIKOQL | `merge_knowledge_ir` → `compile_context(task, merged, 300)` → `render_context_markdown` |
| Graph-RAG | mock-embedding rank → budget pack → transitive entity expansion (`graph_expand`) |
| RAG | mock-embedding rank → budget pack |

All three judged by the same `units_hit` judge. The shared measurement
(`common/wave31_sim::measure_task`) is executed by both the COMP-001
test and the REPRO-001 test — the reproduction runs the same code it
claims to reproduce.

## Commands

```text
# full per-task table + class rollup (raw output)
cargo test -p aikoql-ingestion --test wave31_comparison -- --nocapture

# the automated reproduction (determinism + direction + conclusion)
cargo test -p aikoql-ingestion --test wave31_repro -- --nocapture
```

## Headline result (pinned from the last clean run)

```text
aikoql  258/296 units (87.2%)   strong_fit=9   worst_regression=0
graphrag 191/296 (64.5%)
rag      181/296 (61.1%)
```

## What an independent run must reproduce

1. **Direction** — aikoql total units ≥ rag AND ≥ graph-rag.
2. **Conclusion** — ≥1 Strong Fit class, W1 control at full parity,
   worst class regression ≤ 2 units.
3. **Determinism** — two passes produce identical per-task mechanical
   columns.

`wave31_repro.rs` asserts all three; it fails loudly if any breaks.

## Evaluation code locations

```text
crates/ingestion/tests/common/trackb.rs          — corpus, questions, judge
crates/ingestion/tests/common/wave31_sim.rs      — measure_task + treatments
crates/ingestion/tests/wave31_comparison.rs      — COMP-001 run + rollup
crates/ingestion/tests/wave31_repro.rs           — REPRO-001 reproduction
```
