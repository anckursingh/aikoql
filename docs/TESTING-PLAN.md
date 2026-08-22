# aikoql — Testing & Certification Plan

**Status:** Draft v1 (2026-08-22) · **Companion to:** [IMPLEMENTATION-PLAN.md](IMPLEMENTATION-PLAN.md) · **Branch:** feature/mvp-launch

This document turns the two QA certification suites — [AIKOQL-Agent-Knowledge-Certification-Test-Suite.md](AIKOQL-Agent-Knowledge-Certification-Test-Suite.md) (QA-AIKOQL-AGENT-MEMORY-001) and [AIKOQL-Chatbot-Memory-Knowledge-Certification-Test-Suite.md](AIKOQL-Chatbot-Memory-Knowledge-Certification-Test-Suite.md) (QA-AIKOQL-CHATBOT-001) — into an execution plan: what we already cover, what is missing, and how the suites become our end-to-end and acceptance layer.

---

## 1. What the certification suites demand

| Suite | Focus | Structure |
| --- | --- | --- |
| QA-AIKOQL-AGENT-MEMORY-001 | AIKOQL as a knowledge substrate for agents (repository compiler, KO model, ontology, provenance, temporal state, constraints, programs, security, agent memory) | Priority model P0–P3 · gates G1–G7 · release levels 1–4 · test groups KB/INC/KO/MM/ONT/PROV/TEMP/CON/CST/QL/EXE/MEM/RET/AGENT/DOC/SEC/DB/PRG/EVO/IDX |
| QA-AIKOQL-CHATBOT-001 | AIKOQL as durable memory + knowledge layer for chatbots (memory, classification, consolidation, personalization, contradiction, provenance, LLM-dependency reduction, context compilation, safety, benchmarks) | Levels C1–C8 · baselines (LLM-only / RAG / Graph-RAG / AIKOQL) · test groups CHAT-MEM/CLASS/CONS/PERS/SEM/EP/TEMP-CHAT/CONTR-CHAT/PROV-CHAT/COMP/LLM/CTX/AUTH-CHAT/PROC-CHAT/PROG-CHAT/SAFE-CHAT/EVO-CHAT/FRESH/CONT |

**Priority model (adopt verbatim):**

| Priority | Meaning | Release rule |
| --- | --- | --- |
| P0 | Data integrity, security, core correctness | 100% pass |
| P1 | Core product capability | ≥98% pass; no Sev-1/Sev-2 |
| P2 | Advanced capability | ≥95% pass |
| P3 | Experimental / optimization | Informational |

**Hard rule from the suites (respect it):** *"A test for an architectural target is not evidence that the feature already exists."* GAP rows below stay failing/absent until the capability is delivered — no stub tests, no fake coverage.

**The two hypotheses under certification:**

1. AIKOQL preserves *truth, context, provenance, temporal state, constraints and executable knowledge* end to end (agent suite §37 lifecycle).
2. AIKOQL-backed chatbots *use less raw context, need fewer LLM reasoning steps, and maintain memory* measurably better than RAG / Graph-RAG / LLM-only (chatbot suite §54).

---

## 2. Coverage verdict

Substrate-level (agent suite levels 1–3) coverage is strong: ~80% of P0/P1 IDs have a real test. The chatbot layer (a consumer of AIKOQL, not part of the binary) has **no conversation-level acceptance scenarios** — its substrate mechanisms are covered, its acceptance stories are not. The comparative experiments (LLM-only / RAG / Graph-RAG baselines) and the agent-efficacy benchmark do not exist yet and are the flagship P2/P3 work.

---

## 3. Coverage matrix — Agent suite (QA-AIKOQL-AGENT-MEMORY-001)

Status: ✅ covered · 🟡 partial · ❌ gap. *Location* names the existing test/bench/script.

### G1 Knowledge integrity · G2 Repository knowledge · G3 Semantic knowledge

| ID | Pri | Status | Location / note |
| --- | --- | --- | --- |
| KB-001 Empty repository | P0 | ✅ | `crates/ingestion/tests/e2e_pipeline.rs` (zero entities, no stale state) |
| KB-002 Single source file | P0 | ✅ | `e2e_pipeline.rs`; artifact KO + hash + provenance |
| KB-003 Multi-file dependency | P0 | ✅ | Phase A2 Rust parser (`DEPENDS_ON`); `e2e_pipeline.rs` |
| KB-004 Cross-module duplicate symbols | P0 | ✅ | Phase A3 entity merging; `multi_source_ontology.rs` |
| KB-005 Duplicate filenames | P0 | ✅ | containment tree v0.1.8; `e2e_pipeline.rs` |
| KB-006 Unsupported/binary file | P1 | ✅ | ingest-dir classifier; scan continues |
| KB-007 Malformed source | P0 | ✅ | invalid-file handling; valid files still processed |
| KB-008 Generated code distinguishable | P1 | 🟡 | no marker test yet |
| KB-009 Repository manifest | P1 | 🟡 | scan metadata exists; versioned manifest untested |
| INC-001 Rescan idempotent | P0 | ✅ | `ingest_incremental.rs`; stable IDs |
| INC-002 Modify one file | P0 | ✅ | `ingest_incremental.rs`; affected-KO recompute |
| INC-003 Rename file | P1 | 🟡 | identity semantics implemented, untested |
| INC-004 Delete file | P0 | ✅ | source deletion → stale/versioned (EVO-002) |
| INC-005 Branch/revision change | P1 | ✅ | A8 git-diff reconciliation |
| KO-001..006 Round-trip, restart, types, updates, edges | P0 | ✅ | `crates/kernel/tests/conformance.rs`, `durability.rs`, `proptest_kom.rs` |
| MM-001..004 Per-source models → KO | P1 | 🟡 | fixtures `tests/fixtures/{postgres_sample.sql,mongo_sample.js,neo4j_sample.cypher}` + A9 bridge; automated per-connector matrix missing |
| MM-005 Cross-model identity resolution | P0 | ✅ | `multi_source_ontology.rs` (config-driven resolution) |
| ONT-001 Explicit ontology validation | P0 | ✅ | `ontology_integration.rs`, MRFC-0060 registry |
| ONT-002 Auto-discovery | P1 | 🟡 | A9 candidate ontology exists; confidence gating untested |
| ONT-003 Invalid relationship | P0 | ✅ | constraint engine C3/C4 |
| ONT-004 Ontology evolution | P1 | 🟡 | no migration test |

### G3/G4 — Provenance · Temporal · Contradiction · Constraints · Query

| ID | Pri | Status | Location / note |
| --- | --- | --- | --- |
| PROV-001 Derived-fact evidence chain | P0 | ✅ | `evidence_wiring.rs`, `derivation.rs` |
| PROV-002 Provenance survives restart | P0 | ✅ | `durability.rs` |
| PROV-003 Conflicting sources retained | P0 | ✅ | `epistemic.rs`, `evals.rs` (e03) |
| PROV-004 Authority ranking | P1 | ✅ | trust spine (R5/R8); authority ordering deterministic |
| TEMP-001..003 Validity windows, history, future | P0/P1 | ✅ | `temporal.rs`, `scripts/e2e-k2-temporal.js` |
| CON-001..003 Contradictions | P0/P1 | ✅ | `epistemic.rs`, `evals.rs` (e03) — claims + evidence + resolution |
| CST-001..004 Schema/cardinality/precondition/policy constraints | P0 | ✅ | `conformance.rs` + MRFC-0060 phases C3–C9 |
| QL-001..009 Parser determinism, diagnostics, injection | P0/P1 | ✅ | `crates/compiler/tests/golden_snapshots.rs`, `grammar_coverage.rs`, `fuzz_parser.rs` |
| QL-007 Constraint-aware program query | P1 | 🟡 | parser path exists; no dedicated oracle |
| EXE-001..006 Query execution oracles | P0/P1 | ✅ | `conformance.rs`, `evals.rs`, `indexes.rs` |

### G5/G6 — Agent memory · Programs · Retrieval

| ID | Pri | Status | Location / note |
| --- | --- | --- | --- |
| MEM-001 Working memory (ephemeral by default) | — | 🟡 | ingest_observation op (63c1f43); no working-memory construct |
| MEM-002 Episodic memory with links | — | ✅ | `experiences.rs`, `scripts/e2e-k5-experience.js` |
| MEM-003 Semantic memory from experience | — | ✅ | `derivation.rs` |
| MEM-004 Procedural memory | — | ✅ | `experiences.rs` |
| MEM-005 Program-as-KO surface | — | ✅ | `experiences.rs` (inputs/outputs/permissions/pre/post/side effects) |
| MEM-006 Consolidation (3 episodes → procedure) | — | ✅ | `derivation.rs` (cites source episodes, confidence) |
| MEM-007 Failed experience | — | ✅ | `experiences.rs` (CONS-004 semantics) |
| MEM-008 Staleness detection | — | ✅ | `evals.rs` (e02 staleness distribution) |
| PRG-001..008 Program lifecycle | P0/P1 | ✅ | `experiences.rs`; PRG-007 idempotency/retry 🟡 |
| RET-001 Semantic retrieval | P1 | ✅ | `retrieval_quality.rs` (fusion + degrade paths) |
| RET-002 Exact structured retrieval | P0 | ✅ | `conformance.rs`, prefix-scan audit (R6) |
| RET-003 Hybrid retrieval | P1 | ✅ | PR-L RRF fusion, §60 matrix |
| RET-004 Reranking with labeled set | P1 | 🟡 | fusion only; no learned reranker (HLD §60 decision: mock stays) |
| RET-005 Evidence/authority-aware ranking | P0 | ✅ | v0.1.15 relation boost + trust-aware ranks |
| AGENT-001..005 Repository-agent scenarios | — | 🟡 | `mcp_real_world.rs` workflow + A8 engine cover mechanics; comparative scoring absent |

### G7 Security · Persistence · Documents · Indexes · Evolution

| ID | Pri | Status | Location / note |
| --- | --- | --- | --- |
| SEC-001..006 Encryption at rest, keys, fail-closed, crash | P0 | ✅ | `crates/kernel/tests/encryption.rs`, `encryption_load.rs`, `scripts/e2e-enc-smoke.js` (DEK-before-object, wrong/missing key fails closed, no plaintext fallback) |
| SEC-007 Agent authorization | P0 | ✅ | R9 tenant isolation (authorize() confinement) |
| DB-001 Restart after large write | P0 | ✅ | `durability.rs` |
| DB-002 Kill during write | P0 | ✅ | `crash_kill.rs`: real taskkill/SIGKILL mid-write via `crash_writer` loop mode, reopen → journal head ≥ observed, all KOs + audit chain intact |
| DB-003 Concurrent writers | P0 | ✅ | `transactions.rs` |
| DB-004 Concurrent readers/writers | P1 | 🟡 | isolation semantics documented; stress test absent |
| DOC-001..007 PDF/OCR/tables/images/versioning | P1 | ✅ | `multimodal_golden.rs` (19 DoD rows), `multimodal_acceptance.rs`; DOC-002 OCR is feature-gated (`vlm`) 🟡 |
| IDX-001..003 Derived index rebuild/orphans | P0 | ✅ | `indexes.rs` i09 (delete + rebuild = identical results), i10 (tombstone-while-down swept on rebuild), i11 (canonical update propagates deterministically) |
| EVO-001..005 Evolution | P0/P1 | 🟡 | derivation (001), stale-on-source-delete (002), extraction version in evidence (005), ontology evolution (004, t06zw) ✅; schema migration (003): AC-22 pre-validation + versioned coexistence + version gate tested (t06zr/s/zt) — apply/migrate op is feature work |
| Negative/adversarial (§28) | P0 | ✅ | `fuzz_codec.rs`, `fuzz_parser.rs`, `proptest_kom.rs`, adversarial secret tests (Slack/Stripe/OAuth/mongodb+srv, disk-/sha256 false positives) |

### Benchmarks (§29, §31, §32)

| Item | Status | Location / note |
| --- | --- | --- |
| Micro-benches: scan, KO/sec, queries | ✅ | `benches/ingest_benchmark.rs`, `kom_benchmark.rs`, `parse_bench.rs` |
| Scale bench (100K→1M keys, 16 scenarios) | ✅ | R14 infrastructure + weekly regression CI (>20% alert) |
| Knowledge-quality instruments (§30) | ✅ | `semantic_extraction_quality.rs` (entity/relation P/R, fact/event accuracy), `retrieval_quality.rs` (P@K/R@K/MRR/nDCG), `e2e_answer_quality.rs` (§53 answer/citation/evidence), §60 real-model harness |
| Agent efficacy benchmark (50–100 tasks, A–D treatments) | ❌ | flagship gap — see Phase 4 |
| Agent memory benchmark vs conventional memory | ❌ | requires the efficacy benchmark's task corpus |

---

## 4. Coverage matrix — Chatbot suite (QA-AIKOQL-CHATBOT-001)

AIKOQL is the memory/knowledge layer, not the chatbot. "Substrate" = the mechanism AIKOQL must provide is covered; "Acceptance" = the conversation-level scenario from the suite is scripted.

| Group | Status | Substrate (ours) | Acceptance (suite scenario) |
| --- | --- | --- | --- |
| CHAT-MEM-001..005 same/cross-session, persistence, explicit vs ephemeral | ✅ | observation/episodic ops, epistemic guard P0-1 (ephemeral ≠ durable), `e2e-restart.js` | `mcp_real_world.rs::chatbot_memory_certification_scenarios` — same-session, cross-session, real server restart, explicit remember with evidence, ephemeral stays "observed" |
| CLASS-001..005 fact/preference/episode/procedure/program | ✅ | KO types exist (Phase A0 + experience/program types) | same test classifies fact→SemanticFact, preference→UserPreference, episode→experience, procedure→experience+reuse_conditions, program→aikoql:program |
| CONS-001..004 consolidation incl. failed experience | ✅ | `derivation.rs`, `experiences.rs` | covered by MEM-002..007 |
| PERS-001..004 preference, provenance, conflict, scope | ✅ | provenance + temporal versioning ✅; scope types exist (R9 confinement) | same test: preference drives recall; provenance names source+confidence (explain() now falls back to kernel-managed evidence); supersede keeps history, current-truth returns only the new value; other-user recall + point read denied |
| SEM-001..003 facts, multi-hop, deterministic no-LLM path | ✅ | structured retrieval, graph traversal, MCP tools answer without LLM (SEM-003 = LLM calls 0) | — |
| EP-001..004 episode retrieval, timeline, chain, provenance | ✅ | `experiences.rs` + temporal ordering + evidence links | — |
| TEMP-CHAT-001..003 | ✅ | `temporal.rs`, `e2e-k2-temporal.js` | — |
| CONTR-CHAT-001..003 | ✅ | `epistemic.rs`, `evals.rs` e03 | — |
| PROV-CHAT-001..003 source-backed, unsupported claim, confidence | 🟡 | citation/evidence instruments (PR-R); ContentTrust fail-closed (003 ✅) | "insufficient information" phrase test absent |
| COMP-001..005 RAG/Graph-RAG comparisons | ❌ | retrieval baselines measured internally | system-level baselines not built |
| LLM-001 deterministic path | ✅ | MCP tools resolve without LLM | — |
| LLM-002..004 context reduction, no re-derivation, no doc dump | 🟡 | context compiler (A5: ranking, budget, dedup) | token-reduction benchmark absent |
| CTX-001..003 permission/time/update-sensitive context | ✅ | compiler respects temporal + authorization | `mcp_real_world.rs::ctx_differential_scenarios` — same question, two users: owner gets the context, no-grant user gets ACCESS_DENIED; same question at two times: TTL'd experience present then dropped; same question after knowledge update: new facts/entities in, replaced ones out |
| CTX-MIN-001..003 20/1000 relevance, no irrelevant history, dedup | ✅ | budget + ranking + dedup machinery | `context.rs::ctx_min_*` — 1000-KO IR: only the 20 relevant entities pack (irrelevant score-0 → cut), budget trims the fold by rank; duplicate entities/facts/relations packed once (dedup added at pack time) |
| AUTH-CHAT-001..004 | ✅ | R9 tenant isolation + authorize() confinement | — |
| Sensitive memory (PII/financial/credentials) | ✅ | A7 PII filter (11 secret types), encryption policy | — |
| RET-CHAT-001 auto-expiry | ❌ | no retention/expiry policy yet | — |
| RET-CHAT-002..003 deletion semantics | 🟡 | deterministic deletion exists | audit-metadata policy untested |
| PROC-CHAT-001..004 procedure version, MFA constraint, why | ✅ | procedure KOs + CST-003 precondition blocking | explanation text is LLM-level |
| PROG-CHAT-001..004 program discovery/approval/postconditions | 🟡 | `experiences.rs` (pre/postconditions, denied execution) | intent→program discovery + approval flow absent |
| SAFE-CHAT-001..004 explain vs execute, denial | 🟡 | authorize() denial ✅ | explain/execute disambiguation is chatbot-level |
| EVO-CHAT-001..003 correction, conflicting user input, authoritative change, no retrain | ✅ | temporal versioning + trust policy (claims vs authoritative) + re-ingest without retrain (`e2e-k3-lineage.js`) | — |
| FRESH-001 freshness SLA | 🟡 | pipeline measured informally | SLA measurement absent |
| CONT-001..003 restart continuity, schema upgrade | 🟡 | `e2e-dogfood.js` + `e2e-restart.js` (CI) ✅ | schema-migration test absent |
| Memory isolation (§30) / multi-agent shared knowledge (§31) | 🟡 | R9 tenant isolation ✅ | agent A/B scenario absent |
| Memory explainability (§33) | 🟡 | provenance machinery answers what/why/where/when | packaged explainability test absent |
| Hallucination / boundary / retrieval-failure (§34–36) | 🟡 | epistemic boundary P0-1, lexical degrade fallback | unknown-vs-failed-retrieval distinction test absent |
| Index independence (§37) | ✅ | `indexes.rs` | i09–i11: rebuild parity, tombstone sweep, update propagation |
| Summarization + provenance (§38–39) | ❌ | markdown/doc compilers exist; conversation→summary not implemented | — |
| Memory compression (§40) | ❌ | measurement target only | — |
| Cache correctness (§42), races (§43–44) | 🟡 | `transactions.rs` concurrency ✅ | chatbot-level determinism scenarios absent |
| Token/latency/cost benchmarks (§45–48) | 🟡 | criterion benches + one-off token numbers | comparative benchmark absent |
| Agent quality benchmark (§49), golden dataset (§50) | ✅ | unified golden dataset `common/golden_dataset.rs` (17 §50 questions: answer/KOs/relations/evidence, 15 textual + 2 visual-only) consumed by all corpus instruments + `golden_dataset_integrity.rs` cross-check gate (grounding, extraction, annotation-list agreement) | §49 agent benchmark = G10 (TP-4) |
| §51 Critical e2e scenario | ✅ | `mcp_real_world.rs::critical_e2e_scenario_51_chatbot_memory` — full script: 3 memories → recall (AWS, provenance/scope) → org directive (organization_policy) → supersede (Azure + reason + evidence) → program → policy allow/deny → execute → postconditions → episode; surfaced + fixed 2 boundary bugs (parse_origin human, evidence path via assert_knowledge) | — |
| §52 Ultimate comparative experiment | ❌ | §60 matrix = internal equivalent | A/B/C/D treatment table absent |

---

## 5. Gap analysis (ranked)

### Tier 1 — Certification P0 gaps (close before claiming anything beyond MVP)

| # | Gap | Suite IDs | Effort | Notes |
| --- | --- | --- | --- | --- |
| G1 | **Traceability + certification runner** — map every suite ID to its test, mark P0–P3, wire to CI tiers | all | ~1 week | This document is the draft; add a machine-readable matrix + a `certs` test that fails on known-GAP P0s |
| G2 | **DB-002 kill-during-write harness** | DB-002 | ✅ done | `crash_kill.rs` d05 + `crash_writer` loop mode: taskkill/SIGKILL mid-write, reopen → journal head ≥ observed progress, all KOs + audit chain intact |
| G3 | **IDX-001/003 rebuild consistency + orphan sweep** | IDX-001..003 | ✅ done | `indexes.rs` i09–i11: rebuild = identical results; tombstone-while-down swept; canonical update propagates (zero lag) |
| G4 | **Schema/ontology migration tests** | EVO-003/004, CONT-003, ONT-004 | ✅ done (honest slice) | t06zt (v1→v2 bump: old data + versions preserved, v2 writes coexist, v1 write rejected), t06zw (ontology v1→v2: old knowledge readable, v2 rules bind); CON-003 was already covered; EVO-003 stays open: no apply/migrate op, codec wire format unversioned — feature work |

### Tier 2 — Acceptance scenarios (substrate exists; script the story)

| # | Gap | Suite IDs | Effort | Notes |
| --- | --- | --- | --- | --- |
| G5 | **§51 critical e2e scenario as scripted test** (memory → temporal → authority → program → episode) | §51, CHAT-MEM-*, EVO-CHAT-* | ✅ done | `critical_e2e_scenario_51_chatbot_memory` in `mcp_real_world.rs`; mechanical judges; surfaced 2 boundary bugs (parse_origin `human` unreachable → Origin::Agent; evidence only via assert_knowledge, not remember) |
| G6 | **Chatbot memory scenarios** (classification, preferences, consolidation, isolation, explainability) | CLASS-*, PERS-*, §30–33 | ✅ done | `chatbot_memory_certification_scenarios` in `mcp_real_world.rs` (§8 CHAT-MEM-001..005 incl. real restart, §9 CLASS-001..005, §11 PERS-001..004); CMEM-001/003/006/007 → covered; surfaced + fixed explain() provenance gap (asserted evidence reported as "Source: unknown" — explain now falls back to the kernel-managed EXT_EVIDENCE) |
| G7 | **CTX differential tests** (two users, two times, post-update) + 1000-KO minimization | CTX-001..003, CTX-MIN-* | ✅ done | `ctx_differential_scenarios` over MCP (permission differential via ACL-gated IR fetch, temporal differential via experience TTL, post-update via versioned ir_json snapshot) + pure-compiler `ctx_min_*` tests; surfaced + fixed missing pack-time dedup (duplicate entities/facts/relations were packed twice) |
| G8 | **Connector contract matrix** (postgres/mongo/neo4j positive/negative/timeout/auth/schema-change/incremental) | MM-001..004, §22 | ~2 weeks | fixtures exist; needs per-connector harness |
| G9 | **Unified golden dataset** (expected KOs/relations/evidence/temporal/authorization per question) | §50 | ✅ done | `tests/common/golden_dataset.rs`: one `GOLDEN` table (17 questions × §50 fields: expected answer, KOs, relations, evidence qrels — temporal/authorization/action stay scenario-shaped in the §51 scripts), `SEMANTIC_GOLD` (per-fixture complete extraction gold), `multimodal_expected_entities` (human annotation lists); all corpus instruments (retrieval, semantic, multimodal golden, PR-R e2e) now consume it — the index-aligned `GOLDEN_ANSWERS` const is gone. `golden_dataset_integrity.rs` gates the dataset itself: unique ids/questions, answers grounded in qrel chunks, expected KOs/relations actually extracted, per-question KOs ⊆ human annotation lists. Pinned baseline unchanged (0.867/0.867/0.867, queries=15) |

### Tier 3 — Flagship benchmarks (P2/P3, post-MVP)

| # | Gap | Suite IDs | Effort | Notes |
| --- | --- | --- | --- | --- |
| G10 | **Agent efficacy benchmark** — 50–100 engineering tasks, treatments A (repo-only) / B (RAG memory) / C (code graph) / D (AIKOQL), measured success/tokens/tool calls | §31, AGENT-* | ~4–6 weeks | the suite's own flagship; needs task corpus + baseline harnesses |
| G11 | **Chatbot comparative experiment** — A/B/C/D treatments on the chatbot corpus, §52 table | §52, COMP-* | ~4 weeks | reuse G5 corpus; honest measured-results-only table |
| G12 | **Token/latency/cost benchmarks** vs RAG baseline | §45–48, LLM-002 | ~2 weeks | extend R14 bench infra |
| G13 | **Retention/expiry policy** (RET-CHAT-001) + conversation summarization (§38–39) | RET-CHAT-001, §38–39 | feature work + tests; plan via IMPLEMENTATION-PLAN | product features first, then their certification |

### Deliberately out of scope for certification claims

- Anything the suites mark as *architectural target* where the feature is deferred (e.g. read replicas, native storage engine, Cloud KMS) — no tests until the feature lands (suite's own hard rule).
- LLM-quality judging — judges stay mechanical (PR-R architect verdict); live models only in the feature-gated §53/§60 instruments.

---

## 6. E2E & acceptance strategy

### 6.1 Execution tiers (adopt the suite's own layout)

| Tier | When | Content | Status today |
| --- | --- | --- | --- |
| **PR gate** | every PR | P0 unit + P0 integration + parser + storage + security regression + dogfood e2e | ✅ exists (check/test-linux/lint/e2e-dogfood jobs); needs the P0 traceability wiring from G1 |
| **Nightly** | scheduled | full P0/P1, connector matrix, performance, fuzz | 🟡 partial (weekly bench regression exists); add connector matrix + full P0/P1 sweep |
| **Release** | tag | full certification, agent benchmark, memory benchmark, recovery, cross-platform, Docker, security | ❌ — build from G1–G4 + G8; benchmarks land with G10–G12 |

### 6.2 Harness principles

1. **Mechanical judging everywhere.** Golden answers + qrels + key-token matching (PR-R judges). No LLM judges — deterministic, CI-reproducible, no self-judging bias.
2. **Mock LLM for acceptance scenarios.** Chatbot-suite scenarios run with a stub that verbalizes AIKOQL results (LLM-001: LLM calls = 0 on deterministic paths). Live models only in feature-gated instruments (`answer_gen`, `remote_emb`, `vlm`, `transform`) — never the default build (HLD §56).
3. **Golden fixtures over generated corpora.** Extend the multimodal 10-fixture golden suite pattern to the chatbot corpus (§7 fixtures: users/conversations/products/policies/procedures/episodes/contradictions/temporal).
4. **One runner, three tiers.** A single certification entry point selects by priority: `P0` for PRs, `P0+P1` nightly, `all` at release — instead of duplicating suites per tier.
5. **Measured claims only.** The §52/§54 tables stay empty until benchmarked (suite rule: *"measured results only"*).

### 6.3 How the suites become acceptance tests

- **P0/P1 IDs that are ✅ today** → acceptance = the existing test, registered in the traceability matrix (G1). Registration, not rewrite.
- **P0/P1 IDs that are 🟡** → close the specific gap (G2–G9), then register.
- **Chatbot-suite scenarios** → the G5/G6 scripted harness over MCP tools; each §51-style script is an acceptance test with golden expectations.
- **Benchmark rows** → G10–G12; the suites explicitly forbid arbitrary thresholds — establish baselines first, then set floors (same pattern as our §60 gates).

---

## 7. Implementation phases

| Phase | Content | Depends on | Target |
| --- | --- | --- | --- |
| **TP-1 — Traceability & gates** | suite-ID → test matrix (machine-readable), P0 registry test that fails on known gaps, wire PR/nightly tiers | — | ✅ implemented (2026-08-22): `crates/kernel/tests/certification.rs` — 121 gate IDs (94 agent P0/P1 + 27 chatbot release claims) as the registry; `certification_matrix_integrity` runs in every PR (fails on unregistered ID, missing test path, note-less gap); `certification_p0_closure` is `#[ignore]` and runs in the weekly `cargo test --workspace -- --ignored` sweep (benchmark-nightly.yml) — currently red on 2 P0s by design (DB-002, EVO-003) |
| **TP-2 — P0 gap closure** | G2 (kill harness), G3 (index rebuild), G4 (migration) | TP-1 | before next release tag |
| **TP-3 — Acceptance scenarios** | ✅ COMPLETE: G5 (§51 script) ✅, G6 (chatbot memory) ✅, G7 (CTX differential) ✅, G9 (unified golden dataset) ✅ | TP-1 | ✅ done (2026-08-22) |
| **TP-4 — Flagship benchmarks** | G10 agent efficacy, G11 comparative experiment, G12 token/latency/cost | TP-3 corpus | post-MVP, ~6–8 weeks |
| **TP-5 — Connector matrix** | G8 per-connector contract tests | connector workstream in IMPLEMENTATION-PLAN | post-MVP, ~2 weeks once connectors land |
| **TP-6 — Product features** | G13 retention/summarization → then their certification | IMPLEMENTATION-PLAN roadmap | post-MVP |

Post-MVP sequencing note: TP-1/TP-2 are cheap and buy the "certification-grade" claim; TP-3 gives the chatbot suite's acceptance stories; TP-4 is the flagship empirical claim ("agents solve tasks more reliably with AIKOQL while consuming less budget") and is the largest single workstream — it also produces the evidence packs the compliance workstream needs.

---

## 8. Immediate next steps

1. **TP-4**: flagship benchmarks — G10 agent efficacy (task corpus + A/B/C/D treatments), G11 chatbot comparative (§52 table), G12 token/latency/cost vs RAG baseline. TP-3 is complete; TP-4 is the largest single workstream and produces the compliance evidence packs.
2. **TP-5** (once connectors land): G8 per-connector contract matrix.
3. Keep the two suite docs in `docs/` as the source of truth for ID numbering; the registry (`certification.rs`) is the enforcement layer — add a row there when a gap closes, never remove a gap row without its test.

---

## Appendix — Existing instruments that serve the suites

| Instrument | What it measures | Serves |
| --- | --- | --- |
| `golden_dataset_integrity` | §50 dataset cross-check: unique ids, answers grounded in qrel chunks, expected KOs/relations extracted, annotation lists consistent | all corpus instruments (G9) |
| `semantic_extraction_quality` | entity/relation P/R, fact/event accuracy vs unified `SEMANTIC_GOLD` | §30 Extraction, KB/ONT/MEM-003 |
| `retrieval_quality` | P@K, R@K, MRR, nDCG vs the unified dataset's 15-query qrels | §30 Retrieval, RET-* |
| `e2e_answer_quality` | answer/citation/evidence correctness, gate verdict | §53 of the multimodal HLD (our §53), PROV-CHAT-* |
| `multimodal_golden` | 19 DoD rows over 10 PDF fixtures | DOC-001..007 |
| `real_model_bench` | §60 six-metric model-decision harness | P3 §60 decisions |
| `e2e-dogfood.js` + `e2e-restart.js` (CI) | install → ingest → search → retrieve → restart → continuity | CONT-001..002, KB-*, release gate |
| e2e-k1..k5 scripts | ingest, temporal, lineage, transactions, experience | TEMP-*, EVO-*, MEM-*, DB-* |
| R14 scale bench + weekly regression CI | 16 scenarios at 100K–1M keys | §29 Performance |
