# Wave 3 — Market Reality: Implementation Analysis & Evidence

Status: all W3-P0/P1 experiments **implemented and measured** (commits
f7b6fda, 0c23a18). Every number below comes from a committed,
deterministic, LLM-free test (the G12 convention). Negative evidence is
kept in `docs/benchmarks/{wins,parity,losses,unknown}.md` (plan §29).

## 1. Coverage matrix — plan item → status → evidence

| Plan item | Status | Evidence (test / artifact) |
|-----------|--------|---------------------------|
| W3-P0 MKT-001 market corpus | ✅ | `w3_mkt_001_market_corpus_integrity` — 19 docs / 34 chunks / 13 questions, W1-W12 labeled, every unit verbatim-backed |
| W3-P0 COMP-001 A/B/C/D | ✅ | G11 `comparative_chatbot_bench` (D 15/16 vs B 10/16 vs C 13/16, prov 2/2) |
| W3-P0 WIN-001 workload classification | ✅ | `w3_win_001_workload_classification` — W4 Strong Fit 7/8 vs 3/8; W7 Strong Fit 2/2 vs 1/2; W11 Good Fit; W1/W3/W5/W6/W9 Parity; W2 Unknown |
| W3-P0 MH-001 multi-hop | ✅ | WIN-001 W4 class + G10/G11 pins |
| W3-P0 TEMP-001 temporal | ✅ | `w3_temp_001_temporal_market_reality` — 2/2 superseded suppressed vs RAG 2/2 confusion (G11 0.0 row closed) |
| W3-P0 CONF-001 contradiction value | ✅ | `w3_conf_001_contradiction_value` — policy-only pack (1/3) vs RAG 3/3 |
| W3-P0 PROV-001 provenance value | ✅ | WIN-001 W7 + G11 prov pin 2/2 |
| W3-P0 UNK-001 unknown handling | ✅ | `w3_unk_001_unknown_handling_classification` — 4 classes + honest false-confidence probe |
| W3-P0 CTX-001 context efficiency | ✅ | WIN-001 token columns + G12 cost bench + LONG-001 flat tokens |
| W3-P0 DEV-001 build-vs-buy | ✅ | §4 below |
| W3-P0 DEV-002 source expansion | ✅ | §4 below (one `compile_file` dispatch, 6+ formats, 8 storage adapters) |
| W3-P1 LONG-001 longitudinal | ✅ | `w3_long_001_longitudinal_value` — 4/4 vs 1/4 vs 1/4, flat vs growing tokens |
| W3-P1 MEM-001 compression | ✅ (honest) | §40 `memory_compression_bench` — verbatim re-format ratio 1.07; real saver = retention boundary (recall 20→10) |
| W3-P1 TOOL-001 | ✅ | MCP tool surface, TP-3 scenarios (G5 §51) |
| W3-P1 DEBUG-001 debuggability | ✅ | `w3_debug_001_observability_root_cause` — 5/5 injected failures surfaced |
| W3-P1 IMPACT-001 | ✅ | QA2 + TESTING-PLAN coverage matrix (certify.js) |
| W3-P1 OSS-001 | ✅ | GHCR/npm live, PRR-8 (prior milestone) |
| W3-P2 economic model | 📐 measurement only | G11/G12 cost tables; scale-to-value = future work (unknown.md) |
| W3-P2 §28 reproducibility | ✅ | §2 recipe below |
| W3-P2 §29 negative evidence | ✅ | docs/benchmarks/{wins,parity,losses,unknown}.md |
| W3-P2 §30 evidence matrix | ✅ | §3 below |
| W3-G01..G07 release gates | ⏳ | certify.js Wave 3 block + TESTING-PLAN rows — next milestone (this doc is committed first, the gate block pins it) |

Legend: ✅ measured and pinned · 📐 measured but open-ended by design ·
⛔ out of substrate scope (2026-08-25 directive: OCR, agent loops W10,
paraphrase compression).

## 2. §28 Reproduction recipe

```bash
# The full evidence set is one command per instrument, all deterministic:
cargo test -p aikoql-ingestion --test wave3_market_reality -- --nocapture
cargo test -p aikoql-ingestion --test comparative_chatbot_bench
cargo test -p aikoql-ingestion --test comparative_cost_bench
cargo test -p aikoql-ingestion --test knowledge_bench
cargo test -p aikoql-ingestion --test agent_efficacy_bench agent_memory_bench
cargo test -p aikoql-kernel sec40_memory_compression_measurement
cargo test -p aikoql-kernel --test qa2_knowledge

# Certification gate (Wave 1+2+3 matrix + artifact regeneration):
node scripts/certify.js
```

No API keys, no network, no randomness: every number re-measures to the
same value on any machine with the same toolchain. The one exception is
the optional `answer_gen`/`remote_emb` seams (§53/PR-P) which stay
feature-gated and off by default.

## 3. §30 Evidence matrix — headline claim → instrument → measured

| Claim | Instrument | Measured | Verdict |
|-------|-----------|----------|---------|
| "Beats RAG on multi-hop" | WIN-001 W4 | 7/8 vs 3/8 (Δ+4, fewer tokens) | Strong Fit |
| "Beats RAG on provenance" | WIN-001 W7 + G11 | 2/2 vs 1/2; prov 2/2 pinned | Strong Fit |
| "Superseded claims never reach the agent" | TEMP-001 | 2/2 suppressed vs 2/2 RAG confusion | Win |
| "Conflict resolution is auditable, not silent" | CONF-001 | 1/3 vs 3/3; superseded claims readable | Win |
| "Stays correct as the world changes" | LONG-001 | 4/4 flat vs 1/4 both baselines | Win |
| "Healthy empty = genuine unknown" | UNK-001 | 4 classes asserted | Win |
| "Failures are diagnosable" | DEBUG-001 | 5/5 surfaced; P0-1 fail-closed held | Win |
| "Costs competitive" | G12 | deterministic, 0 LLM calls; raw bytes favor RAG | Parity |
| "Zero-overlap recall" | WIN-001 W2 | 0/2 both — mock embeddings, real ones gated | Unknown/loss |
| "Never false-confident on absent answers" | UNK-001 probe | 5/5 facts on an absent answer | **Loss — honest** |

The last two rows are the market-reality correction: the product thesis
holds on temporal/conflict/provenance/longitudinal/multi-hop, is parity
on cost, and has two documented ceilings (semantic recall in the
default build, IR-level false confidence on vocabulary overlap).

## 4. Build-vs-buy (W3-DEV-001/002)

### Track A — the roll-your-own stack (what the plan lists)

Postgres + vector DB + graph/joins + RAG framework + custom sync +
custom provenance + custom conflict handling + custom context compiler.

### The measured proxy in this repo

The retrieval-only application code — `tests/common/{mod,trackb}.rs`
(rank, pack, corpus, judge) — is **1,042 LOC** and delivers, per the
experiments above: 0 temporal accuracy, no provenance citation, no
conflict resolution, no validity boundary, no lineage. Every one of
those missing pieces is what the 9,410 LOC of engine surface (kernel
6,590 + graph 551 + compiler/runtime 2,269) provides **without the
application writing any of it**.

### Moat table

| Track A component | Application LOC in A | AIKOQL application LOC |
|-------------------|---------------------|------------------------|
| Retrieval (rank+pack) | 1,042 (measured proxy) | `compile_context` call |
| Temporal validity | unbuilt in proxy (0.0 temporal accuracy measured) | kernel supersede + validity boundary |
| Provenance | unbuilt in proxy (0 doc-id units) | evidence trail + `explain` |
| Conflict handling | unbuilt in proxy | `contradict`/`supersede` + Conflict KO |
| Storage (8 backends) | one adapter per store | one kernel API: redb, memory, sqlite, rocksdb, neo4j, postgres, mongodb, vector |
| Source expansion (DEV-002) | new parser + sync per format | one dispatch arm: md, pdf, rust, python, ts/tsx, java, images, text |

The plan's critical question — "does AIKOQL eliminate application
infrastructure developers would otherwise build themselves?" — the
measured answer is yes for the four hardest pieces (validity,
provenance, conflict, lineage): the retrieval-only baseline is 1,042
lines *before* any of them exist.

### Honest caveats

- The LOC comparison is mechanical (wc -l) and single-repo: Track A's
  Postgres/vector DB code is not actually written here — its *proxy* is
  the tests/common baseline. The claim is scoped to "application code",
  not total system size (AIKOQL's substrate is large).
- Time-to-first-prototype, defect counts, and schema-modification time
  are not measured (unknown.md).
