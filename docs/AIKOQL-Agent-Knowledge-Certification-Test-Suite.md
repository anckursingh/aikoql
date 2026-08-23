# AIKOQL — Agent Knowledge & Knowledge Base Certification Test Suite

**Document ID:** QA-AIKOQL-AGENT-MEMORY-001  
**Role:** Senior QA / Quality Engineering  
**Status:** Proposed certification suite  
**Scope:** Knowledge Objects, repository knowledge compilation, ontology, provenance, constraints, temporal knowledge, AIKOQL querying, procedural memory / Programs-as-KO, agent memory, persistence, incremental updates, connectors, and agent-facing retrieval.

---

## 1. Purpose

This document defines the acceptance and certification suite for validating whether AIKOQL can act as a **knowledge-native substrate for AI agents**, rather than merely as a graph, vector store, document store, or repository index.

The current PR identifies encryption-at-rest and the Agent Knowledge OS roadmap as major workstreams, including Knowledge Continuity Test criteria. The repository also has dedicated compiler, ingestion, kernel, provider, storage, benchmark, runtime, service, and cluster areas. The suite therefore tests both the current implementation and the capabilities required by the stated architecture. fileciteturn3file0L2-L2

> **Important:** A test for an architectural target is not evidence that the feature already exists. Tests marked P0/P1 are certification gates; unsupported features remain failing/unimplemented until delivered.

---

# 2. Quality Objective

AIKOQL should demonstrate:

> **Given heterogeneous engineering or agent knowledge, AIKOQL can compile, persist, query, relate, constrain, version, retrieve, and evolve that knowledge with deterministic provenance and predictable behavior.**

For agent memory:

```text
Observation
    ↓
Working Memory
    ↓
Episode / Evidence
    ↓
Knowledge Object
    ↓
Semantic Relationship
    ↓
Ontology
    ↓
Durable Knowledge
    ↓
Procedure / Program-as-KO
    ↓
Planning / Execution
    ↓
Outcome
    ↓
Knowledge Evolution
```

Certification must prove each transition independently.

---

# 3. Priority Model

| Priority | Meaning | Release rule |
|---|---|---|
| P0 | Data integrity, security, core correctness | 100% pass |
| P1 | Core product capability | ≥98% pass; no Sev-1/Sev-2 |
| P2 | Advanced capability | ≥95% pass |
| P3 | Experimental / optimization | Informational |

---

# 4. Certification Gates

## G1 — Knowledge Integrity

- KOs survive restart.
- IDs remain stable.
- Relationships remain valid.
- No silent data loss.
- Corruption fails closed.
- Provenance is retained.

## G2 — Repository Knowledge

- Repository can be scanned.
- Artifacts are discovered.
- Entities and relationships are extracted.
- Source locations are traceable.
- Incremental changes update affected knowledge.

## G3 — Semantic Knowledge

- Ontology entities/types are represented.
- Semantic and structural relationships are distinguishable.
- Entity resolution works across sources.
- Contradictory facts are retained rather than silently overwritten.

## G4 — Queryability

- Valid AIKOQL parses.
- Invalid AIKOQL fails deterministically.
- Query semantics match expected results.
- Filters, relationships, temporal state and constraints are respected.

## G5 — Agent Memory

- Episodic, semantic and procedural memory are representable.
- Memory can be consolidated.
- Stale knowledge can be detected.
- Conflicting knowledge can be explained.

## G6 — Programs-as-KO

- Typed inputs/outputs.
- Preconditions.
- Constraints.
- Permissions.
- Postconditions.
- Execution outcome as knowledge.

## G7 — Security

- Encryption at rest.
- Wrong/missing keys fail closed.
- No plaintext fallback.
- Access boundaries are enforced.

---

# 5. Test Environment

Required platforms:

1. Linux
2. macOS
3. Windows
4. Native binary
5. Docker
6. Clean installation
7. Upgrade from previous database state

Required fixture repositories:

```text
fixtures/
├── tiny-rust/
├── medium-rust/
├── python-service/
├── typescript-service/
├── polyglot/
├── repository-with-docs/
├── repository-with-tests/
├── repository-with-duplicate-symbols/
├── repository-with-generated-code/
└── repository-with-invalid-files/
```

Agent-memory fixture:

```text
agent-memory-fixture/
├── users/
├── episodes/
├── facts/
├── procedures/
├── programs/
├── constraints/
├── policies/
└── contradictions/
```

---

# 6. Canonical Test Model

```text
KnowledgeObject
├── id
├── type
├── properties
├── relationships
├── provenance
├── confidence
├── source
├── created_at
├── valid_from
├── valid_until
├── version
└── status
```

```text
Relationship
├── source_ko
├── predicate
├── target_ko
├── confidence
├── provenance
├── valid_from
├── valid_until
└── status
```

```text
Evidence
├── artifact
├── location
├── content_hash
├── extractor
├── extraction_version
└── timestamp
```

---

# 7. Repository Knowledge Compiler

| ID | Priority | Test | Expected |
|---|---|---|---|
| KB-001 | P0 | Empty repository | Scan succeeds; repository KO exists; zero entities/relationships; no stale state |
| KB-002 | P0 | Single source file | Artifact KO, symbols, location, hash and provenance exist |
| KB-003 | P0 | Multi-file dependency | Correct dependency relationships are created |
| KB-004 | P0 | Cross-module duplicate symbols | Distinct symbols are not incorrectly merged |
| KB-005 | P0 | Duplicate filenames | Artifacts remain distinct |
| KB-006 | P1 | Unsupported/binary file | Classified/ignored safely; scan continues |
| KB-007 | P0 | Malformed source | Diagnostic produced; valid files still processed; invalid source not authoritative |
| KB-008 | P1 | Generated code | Generated artifacts are distinguishable where detectable |
| KB-009 | P1 | Repository manifest | Identity, revision, scan/compiler/schema/extraction versions and counts recorded |

### Required repository-level assertion

```text
repository
 → artifact
 → entity
 → relationship
 → provenance
```

must be traversable end-to-end.

---

# 8. Incremental Knowledge

| ID | Priority | Test | Expected |
|---|---|---|---|
| INC-001 | P0 | Rescan unchanged repository | Idempotent; no duplicate KOs/edges; stable IDs |
| INC-002 | P0 | Modify one file | Only changed/affected knowledge is recomputed |
| INC-003 | P1 | Rename file | Identity semantics are deterministic; no silent duplicates |
| INC-004 | P0 | Delete file | Artifact becomes deleted/stale; dependent knowledge is invalidated/versioned |
| INC-005 | P1 | Branch/revision change | Incompatible knowledge is not silently combined |

Measure first-scan vs no-change rescan time and affected-KO count.

---

# 9. Knowledge Object

| ID | Priority | Test | Acceptance |
|---|---|---|---|
| KO-001 | P0 | Create/read round-trip | Lossless |
| KO-002 | P0 | Restart retrieval | Same identity and semantics |
| KO-003 | P0 | Property types | String, numeric, bool, null, array, object, timestamp, identifier and artifact reference preserved |
| KO-004 | P0 | Property update | Only intended property changes |
| KO-005 | P0 | Relationship creation | Correct edge and reverse traversal where supported |
| KO-006 | P1 | Relationship metadata | Confidence/provenance/validity/status preserved |

---

# 10. Multi-Model Knowledge

AIKOQL should prove that one KO model can represent knowledge originating in different database paradigms.

| ID | Priority | Test |
|---|---|---|
| MM-001 | P1 | PostgreSQL row/schema → KO |
| MM-002 | P1 | MongoDB document → KO |
| MM-003 | P1 | Neo4j node/relationship → KO |
| MM-004 | P1 | PGVector/vector + metadata → KO |
| MM-005 | P0 | PostgreSQL customer `123`, Mongo `customerId=123`, Neo4j `Customer#123` resolve to one logical entity when configured |

Acceptance: source-specific representations are preserved as provenance; the canonical KO identity is not tied to one physical model.

---

# 11. Ontology

| ID | Priority | Test | Expected |
|---|---|---|---|
| ONT-001 | P0 | Explicit ontology | Entity types and predicates validate |
| ONT-002 | P1 | Auto-discovery across connectors | Candidate ontology with confidence and evidence |
| ONT-003 | P0 | Invalid relationship | Rejected or explicitly flagged according to enforcement mode |
| ONT-004 | P1 | Ontology evolution | Existing knowledge remains interpretable |

Auto-discovery must never silently convert low-confidence inference into authoritative truth where approval is required.

---

# 12. Provenance and Evidence

| ID | Priority | Test | Expected |
|---|---|---|---|
| PROV-001 | P0 | Derived fact evidence | KO → artifact → location → hash |
| PROV-002 | P0 | Restart | Provenance survives |
| PROV-003 | P0 | Conflicting sources | Both claims/evidence retained |
| PROV-004 | P1 | Authority ranking | Source authority rules are deterministic |

Every high-value derived fact should answer:

```text
What is it?
Why do we believe it?
Where did it come from?
When was it true?
Which extractor produced it?
What contradicts it?
```

---

# 13. Temporal Knowledge

| ID | Priority | Test | Expected |
|---|---|---|---|
| TEMP-001 | P0 | Validity window | Current state returns currently valid fact |
| TEMP-002 | P0 | Historical query | Query at prior time returns prior truth |
| TEMP-003 | P1 | Future fact | Not returned by current query unless requested |

Example:

```text
PostgreSQL valid_until = 2026-05-01
MongoDB valid_from     = 2026-05-01
```

Current query → MongoDB. Historical query before 2026-05-01 → PostgreSQL.

---

# 14. Contradictions

| ID | Priority | Test | Expected |
|---|---|---|---|
| CON-001 | P0 | Conflicting facts | Neither silently discarded |
| CON-002 | P1 | Explain conflict | Claims, evidence, time, authority and resolution shown |
| CON-003 | P1 | Resolve conflict | Current query follows authoritative resolution; historical claims remain available by policy |

---

# 15. Constraints

| ID | Priority | Test | Expected |
|---|---|---|---|
| CST-001 | P0 | Non-null/schema constraint | Invalid KO rejected |
| CST-002 | P0 | Cardinality constraint | Invalid relationship rejected/flagged |
| CST-003 | P0 | Program precondition | Execution blocked when false |
| CST-004 | P0 | Policy constraint | Unauthorized/unsafe operation blocked |

Constraints must be testable independently of an LLM.

---

# 16. AIKOQL Parser

The repository contains a compiler/parser/semantic/planner structure; parser tests must be independent of storage correctness. fileciteturn6file0L2-L10

| ID | Priority | Test |
|---|---|---|
| QL-001 | P0 | Simple entity query: `FIND Customer` |
| QL-002 | P0 | Property filter |
| QL-003 | P0 | One-hop relationship traversal |
| QL-004 | P0 | Multi-hop traversal |
| QL-005 | P1 | Temporal query |
| QL-006 | P1 | Provenance filter |
| QL-007 | P1 | Constraint-aware program query |
| QL-008 | P0 | Malformed query gives structured diagnostic and never executes |
| QL-009 | P0 | SQL/Cypher injection-like text remains data unless grammar explicitly treats it as syntax |

Parser acceptance:

```text
source
 ↓
lexer
 ↓
AST
 ↓
semantic validation
 ↓
logical plan
```

must be deterministic for the same input and language version.

---

# 17. Query Execution

| ID | Priority | Test | Expected |
|---|---|---|---|
| EXE-001 | P0 | Exact match | Exact oracle result |
| EXE-002 | P0 | Empty result | Empty set, not error |
| EXE-003 | P0 | One/multi-hop traversal | Exact graph oracle |
| EXE-004 | P0 | Predicate filtering | Correct cardinality |
| EXE-005 | P1 | Ordering | Deterministic |
| EXE-006 | P1 | Pagination | No duplicates/skips |

For every non-trivial query create a hand-authored expected-result oracle.

---

# 18. Agent Memory

## MEM-001 — Working Memory

Represent:

```text
current_task
current_plan
observations
pending_action
```

Expected: ephemeral by default; durable only after explicit consolidation.

## MEM-002 — Episodic Memory

Record:

```text
Agent
Task
Observation
Action
Outcome
Timestamp
```

Expected: persistent episode with links to affected KOs.

## MEM-003 — Semantic Memory

Convert validated experience into a durable fact with evidence.

## MEM-004 — Procedural Memory

Represent a procedure with:

- inputs
- preconditions
- steps
- constraints
- postconditions
- failure handling

## MEM-005 — Program-as-KO

Program must expose:

```text
inputs
outputs
permissions
preconditions
postconditions
side_effects
```

## MEM-006 — Memory Consolidation

Given three successful episodes, derive a candidate procedure. The procedure must cite source episodes and expose confidence/derivation.

## MEM-007 — Failed Experience

A failed episode remains queryable and can change procedure confidence.

## MEM-008 — Staleness

After relevant architecture/code changes, stale procedures/facts become detectable and are not silently treated as current.

---

# 19. Retrieval and Reranking

| ID | Priority | Test |
|---|---|---|
| RET-001 | P1 | Semantic retrieval |
| RET-002 | P0 | Exact structured retrieval |
| RET-003 | P1 | Hybrid keyword + semantic + graph + metadata retrieval |
| RET-004 | P1 | Reranking using labeled relevance set |
| RET-005 | P0 | Evidence/authority-aware ranking |

Measure:

```text
Precision@K
Recall@K
MRR
nDCG@K
```

Structured retrieval must remain deterministic even when semantic retrieval is unavailable.

---

# 20. Repository-Agent Scenarios

These are the most important end-to-end tests.

## AGENT-001 — Where should I implement feature X?

Compare baseline repository-only agent vs AIKOQL-enabled agent.

Covered: `agent_efficacy_bench.rs` (G10 §31 v1) — 5 "where" tasks, A/B/C/D treatments.

Score:

- correct component
- correct file
- correct dependency
- evidence quality

## AGENT-002 — Change impact

Covered: `agent_efficacy_bench.rs` (G10 §31 v1) — 9 "impact" tasks, A/B/C/D treatments.

Ask:

> What will be affected if module X changes?

Expected where available:

```text
code
→ dependencies
→ tests
→ docs
→ programs
→ constraints
→ requirements
```

## AGENT-003 — Architecture-aware implementation

Ask the agent to implement a feature while preserving architectural decisions and constraints.

Not yet measured — needs agent loops with edit/execute; deferred in G10 v1 (§31).

## AGENT-004 — Historical explanation

Ask why a component works in its current form. Answer must cite source/ADR/history evidence where available.

Covered: `agent_efficacy_bench.rs` (G10 §31 v1) — 6 "why" tasks, A/B/C/D treatments.

## AGENT-005 — Safe procedural execution

Not yet measured — needs agent loops with program execution; deferred in G10 v1 (§31).

```text
discover program
→ validate preconditions
→ permissions
→ constraints
→ execute
→ postconditions
→ outcome
```

A violation at any gate must stop execution.

---

# 21. Document/OCR

| ID | Priority | Test |
|---|---|---|
| DOC-001 | P1 | Text PDF extraction |
| DOC-002 | P1 | Scanned PDF OCR |
| DOC-003 | P1 | Table structure preservation |
| DOC-004 | P1 | Image artifact association |
| DOC-005 | P1 | Stable chunk/content identity |
| DOC-006 | P1 | Document v1/v2 versioning |
| DOC-007 | P1 | Retrieval/reranking after new document version |

A new document version must not erase historical evidence.

---

# 22. Connectors

For PostgreSQL, PGVector, Neo4j, MongoDB and document/repository ingestion, run the common connector contract:

```text
connect
→ discover
→ extract
→ normalize
→ create KO
→ create relationships
→ persist provenance
→ query
→ incremental update
→ disconnect
```

Every connector must have positive, negative, timeout, authentication, schema-change and incremental-update tests.

---

# 23. Derived Index Consistency

Where graph/vector/lexical indexes exist:

```text
canonical KO
    ├── graph index
    ├── vector index
    └── lexical index
```

| ID | Test | Expected |
|---|---|---|
| IDX-001 | Delete derived index and rebuild | Same logical results |
| IDX-002 | Update canonical KO | Index becomes updated/stale deterministically |
| IDX-003 | Orphan detection | No index references nonexistent KOs |

The canonical KO must remain the source of truth; indexes are rebuildable derived state.

---

# 24. Programs-as-KO

| ID | Priority | Test |
|---|---|---|
| PRG-001 | P0 | Register program |
| PRG-002 | P0 | Typed input validation |
| PRG-003 | P0 | Preconditions |
| PRG-004 | P0 | Constraints |
| PRG-005 | P0 | Permission checks |
| PRG-006 | P0 | Postcondition validation |
| PRG-007 | P1 | Idempotency/retry |
| PRG-008 | P1 | Failure creates episode/outcome knowledge |

---

# 25. Knowledge Evolution

| ID | Priority | Test | Expected |
|---|---|---|---|
| EVO-001 | P1 | New evidence | Confidence/knowledge state updates deterministically |
| EVO-002 | P0 | Source deletion | Derived knowledge becomes stale/invalid |
| EVO-003 | P0 | KO schema migration | Semantics preserved |
| EVO-004 | P1 | Ontology migration | Historical knowledge remains interpretable |
| EVO-005 | P1 | Compiler/extractor version | Extraction version recorded |

---

# 26. Security and Encryption

The current PR explicitly calls out **DEK-before-object persistence, fail-closed open on corrupt/missing DEKs, centralized `open_kernel()`, and no silent plaintext fallback**. These are mandatory regression tests. fileciteturn3file0L2-L2

| ID | Priority | Test | Expected |
|---|---|---|---|
| SEC-001 | P0 | Encryption at rest | Sensitive payload is encrypted per policy |
| SEC-002 | P0 | Correct passphrase/key | Store opens |
| SEC-003 | P0 | Wrong passphrase/key | Open fails; no plaintext fallback |
| SEC-004 | P0 | Missing/corrupt DEK | Open fails closed |
| SEC-005 | P0 | Crash during persistence | Store remains recoverable or fails safely |
| SEC-006 | P0 | Sensitive provenance/artifact | Encryption policy applies |
| SEC-007 | P0 | Agent authorization | Unauthorized knowledge is not returned |

---

# 27. Persistence and Recovery

| ID | Priority | Test |
|---|---|---|
| DB-001 | P0 | Restart after large write; verify all committed KOs |
| DB-002 | P0 | Kill process during write; verify recovery/fail-safe behavior |
| DB-003 | P0 | Concurrent writers; no lost/duplicate/corrupt data |
| DB-004 | P1 | Concurrent readers/writers; documented isolation semantics |

---

# 28. Negative / Adversarial Testing

Must cover:

- malformed KOs
- malformed relationships
- invalid ontology
- circular dependencies
- missing source
- missing provenance
- corrupt artifacts/indexes
- invalid UTF-8
- huge properties
- deeply nested objects
- oversized queries
- query timeout
- unavailable connector
- invalid credentials
- revoked credentials
- malformed documents
- OCR failure
- duplicate IDs
- conflicting versions
- corrupted encrypted store

**Hard rule:** no negative test may result in silent data corruption or silently accepted invalid knowledge.

---

# 29. Performance

The repository already has benchmark infrastructure for knowledge operations, scale, hybrid recall and load testing. Certification should extend that into product-level knowledge-quality and end-to-end measurements rather than relying only on microbenchmarks. fileciteturn5file0L2-L2

Measure:

```text
repository scan throughput
KO creation/sec
relationship creation/sec
query P50/P95/P99
memory usage
storage amplification
incremental scan speedup
retrieval latency
index rebuild time
startup time
recovery time
```

---

# 30. Knowledge Quality Metrics

## Extraction

```text
Entity Precision
Entity Recall
Relationship Precision
Relationship Recall
```

## Retrieval

```text
Precision@5
Recall@5
MRR
nDCG@10
```

## Provenance

```text
% derived facts with evidence
% relationships with evidence
```

## Freshness

```text
% stale facts detected
% stale facts correctly suppressed
```

## Consistency

```text
orphan relationships
duplicate entities
contradictory facts
invalid ontology relationships
```

---

# 31. Agent Efficacy Benchmark

This must become a flagship AIKOQL benchmark.

Create 50–100 realistic engineering tasks.

Run each task using:

### A — Baseline

Agent + repository only.

### B — Conventional memory/RAG

Agent + vector/document memory.

### C — Code graph

Agent + repository graph.

### D — AIKOQL

Agent + AIKOQL knowledge.

Measure:

```text
task success rate
first-attempt success
unit/integration tests passing
incorrect edits
unnecessary files changed
tokens consumed
time to completion
tool calls
retrieval calls
constraint violations
hallucinated facts
```

The most valuable claim is empirical:

> **Agents solve engineering tasks more reliably with AIKOQL knowledge while consuming less context/tool budget.**

### v1 measurement (2026-08-23) — `crates/ingestion/tests/agent_efficacy_bench.rs`

First empirical slice: 20 engineering tasks (AGENT-001 where-to-implement, AGENT-002 change-impact, AGENT-004 historical-explanation shapes), corpus = the real `docs/*.md` tree through the production Markdown pipeline (20 docs → 1550 chunks → merged graph 7505 entities / 4922 facts / 3719 relations), mechanical token-containment judge, live local model (llama3.1 on GPU-offloaded Ollama, temperature 0). Scores are printed, not enforced — a model's answers are not CI-pinnable; structural gates (budget, corpus integrity) are asserted.

| metric | A: repo-only | B: LLM + RAG | C: LLM + code graph | D: AIKOQL |
|---|---|---|---|---|
| Success rate | 0.200\* | 0.750 | 0.650 | 0.350 |
| Input tokens/query | 28 | 1338 | 1339 | 354 |
| LLM calls | 20 | 20 | 20 | **0** |
| Tool calls | 0 | 0 | 0 | 20 |
| Latency s/query | 23.0 | 26.7 | 16.8 | **8.1** |
| Cost USD/query | 0.0013 | 0.0052 | 0.0052 | **0.0011** |
| Failed generations | 2 | 0 | 0 | 0 |

\*All four A passes are token-overlap guess passes (e.g. "mcp-tool crate" for the `aikoql-mcp` golden) — under a strict judge A is 0/20; the repo-only agent hallucinates confidently ("British National Corpus").

Findings:

- **B wins accuracy, D wins budget** — the empirical claim splits: AIKOQL is the cheapest and the only deterministic path (0 LLM calls, 8.1 s/query, $0.0011) at 7/20; the RAG pack leads accuracy at 0.750.
- **The extraction blocker is closed**: v1 found D table-blind (`markdown.rs` had no table handling; 17/20 goldens live in docs tables — D's 3 hits were prose phrases). P1 landed 2026-08-23 (GFM pipe tables → Table AST nodes with payloads → the fragment leg's cell-cited facts with row-phrase anchors and `TableCell` evidence; +2144 facts, 4 new D hits — knowledge_bench.rs, e2e_answer_quality.rs, the measured-results row, the Track-B corpus cell). D 0.150 → 0.350 and C 0.500 → 0.650 (table facts enrich its packs) — the measurement now separates extraction from ranking.
- **D's remaining misses are ranking, not extraction**: pure-number cells (goldens "0.15", "6") and rows whose capitalized anchor phrases don't share question vocabulary — the entity gate excludes those cell facts. Candidates: section-heading anchors for cell facts, judge hardening for short goldens.
- **Graph expansion is corpus-dependent**: C still trails B on this corpus (0.650 < 0.750) — the opposite of the Track-B comparative where expansion won. Both measured, both pinned in their harnesses.
- **Epistemic behavior measured implicitly**: B/C answer with `[n]` citations and refuse with exactly "Not in sources" when evidence is insufficient — the §34–36 boundary behavior, live.
- Judge caveat: the ≤1-missing-token rule is lenient on 2-token goldens; hardening (longer goldens or strict match for short ones) is a follow-up.

Deferred (v2): AGENT-003 (implement a feature) and AGENT-005 (safe procedural execution) need agent loops with program execution; the 50–100 task scale is a corpus extension over the same harness.

---

# 32. Agent Memory Benchmark

| Memory | Required measurement |
|---|---|
| Working | Task-state continuity |
| Episodic | Previous-event retrieval |
| Semantic | Factual retrieval |
| Procedural | Procedure selection |
| Temporal | Historical truth |
| Constraint | Safe action selection |
| Provenance | Evidence attribution |
| Consolidation | Experience → durable knowledge |
| Contradiction | Conflicting-fact handling |
| Evolution | Knowledge update |

Compare AIKOQL against at least one conventional memory implementation.

---

# 33. Certification Acceptance Criteria

## Repository Knowledge Ready

- [ ] Supported repositories can be scanned.
- [ ] Artifacts are identified.
- [ ] Entities/symbols are extracted.
- [ ] Relationships are extracted.
- [ ] Provenance exists.
- [ ] Stable identity works.
- [ ] Rescan is idempotent.
- [ ] Incremental changes work.
- [ ] Deletes are handled.
- [ ] Git revisions are represented where supported.
- [ ] Documentation/tests are linked where supported.
- [ ] Query results are deterministic.
- [ ] Agent benchmark demonstrates utility.

## Knowledge Database Ready

- [ ] Relational-shaped knowledge.
- [ ] Document-shaped knowledge.
- [ ] Graph relationships.
- [ ] Vector/embedding metadata association.
- [ ] Cross-model identity resolution.
- [ ] Ontology.
- [ ] Provenance.
- [ ] Temporal state.
- [ ] Constraints.
- [ ] Canonical KO independent of derived indexes.

## Agent Memory Ready

- [ ] Working-memory handoff.
- [ ] Episodic persistence.
- [ ] Semantic facts.
- [ ] Procedural knowledge.
- [ ] Programs-as-KO.
- [ ] Provenance after restart.
- [ ] Temporal queries.
- [ ] Contradiction handling.
- [ ] Staleness detection.
- [ ] Evidence-backed consolidation.
- [ ] Constraint-aware retrieval.
- [ ] Access control.
- [ ] Encrypted memory.
- [ ] Crash recovery.
- [ ] Measurable agent improvement.

---

# 34. Release Certification Levels

## Level 1 — Knowledge Database

```text
KO
Storage
Query
Relationships
Provenance
Persistence
Security
```

## Level 2 — Repository Knowledge

```text
Repository compiler
Incremental compilation
Ontology
Cross-source knowledge
```

## Level 3 — Agent Memory

```text
Working
Episodic
Semantic
Procedural
Temporal
Consolidation
```

## Level 4 — Agent Knowledge OS

```text
Programs-as-KO
Constraints
Planning
Execution
Postconditions
Outcome learning
Knowledge evolution
```

Do not market Level 4 until Levels 1–3 are independently certified.

---

# 35. Recommended Automation Layout

```text
tests/
├── unit/
│   ├── ko/
│   ├── parser/
│   ├── ontology/
│   ├── provenance/
│   ├── constraints/
│   └── memory/
├── integration/
│   ├── repository_scan/
│   ├── storage/
│   ├── connectors/
│   ├── query/
│   └── encryption/
├── e2e/
│   ├── repository_to_knowledge/
│   ├── agent_memory/
│   ├── programs/
│   └── knowledge_evolution/
├── fixtures/
├── adversarial/
├── performance/
└── certification/
```

Execution strategy:

### Pull Request

```text
P0 unit
P0 integration
parser
storage
security regression
```

### Nightly

```text
full P0/P1
repository corpus
connector matrix
performance
fuzz
```

### Release

```text
full certification
agent benchmark
memory benchmark
recovery
cross-platform
Docker
security
```

The repository already has CI and nightly benchmark workflows, so the certification suite should integrate into those execution points. fileciteturn4file0L2-L2

---

# 36. Definition of Done

A capability is not complete when the implementation compiles.

```text
Implementation
     +
Unit Tests
     +
Integration Tests
     +
Negative Tests
     +
Persistence Tests
     +
Security Tests
     +
Performance Measurement
     +
Knowledge Quality Measurement
     +
Agent Utility Test
     =
CERTIFIED CAPABILITY
```

---

# 37. Final QA Position

The central QA question is not:

> **Can AIKOQL store a Knowledge Object?**

It is:

> **Can AIKOQL preserve the truth, context, provenance, temporal state, constraints and executable knowledge required by an agent to make a correct decision?**

The ultimate certification scenario is:

```text
Repository / Data Sources
          ↓
      Ingestion
          ↓
   Knowledge Objects
          ↓
       Ontology
          ↓
 Provenance + Evidence
          ↓
 Temporal + Versioned State
          ↓
      Constraints
          ↓
       AIKOQL Query
          ↓
    Agent Retrieval
          ↓
   Program-as-KO
          ↓
      Preconditions
          ↓
       Execution
          ↓
     Postconditions
          ↓
        Outcome
          ↓
   Episodic Knowledge
          ↓
 Knowledge Consolidation
          ↓
     Updated Memory
```

If AIKOQL passes this lifecycle on deterministic fixtures **and demonstrates measurable engineering-agent improvement against credible baselines**, then there is evidence for the larger product claim:

> **AIKOQL is a knowledge substrate for intelligent agents, not merely another database or repository graph.**
