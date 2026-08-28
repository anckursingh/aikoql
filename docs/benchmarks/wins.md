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
