# AIKOQL Wave 3.1 — Final QA Approval TDD Test Specification

**Document ID:** MVP-QA-003A  
**Role:** Senior QA Lead / Release Quality Owner  
**Purpose:** Close the remaining evidence gaps identified after Wave 3 execution and establish a defensible **Product QA Approved** decision.

## 1. Baseline

Wave 2 is already certified at **45/45 P0, 14/14 P1, Sev-1=0, Sev-2=0, GO**. fileciteturn9file12

Wave 3 is currently **GO**, with all W3-G01..G07 gates passing. It has nevertheless used a relatively small market corpus: **19 documents, 34 chunks and 13 questions**. fileciteturn9file10

Therefore Wave 3.1 is **not another feature-test wave**. It is the final product-evidence closure wave.

## 2. Product QA Approval Definition

AIKOQL is **Product QA Approved for the declared release scope** when:

```text
Technical correctness
        +
Market workload evidence
        +
Real-agent evidence
        +
Developer-value evidence
        +
Economic evidence
        +
Reproducibility
        +
Negative evidence
        =
PRODUCT QA APPROVED
```

Approval does **not** mean AIKOQL is universally better than RAG.

It means the release has sufficient evidence for scoped, reproducible product claims.

## 3. Already Closed — Do Not Duplicate

The existing test program already covers concurrency, knowledge consistency, derived state, fault recovery, schema evolution, retrieval, security, property/state-machine testing, continuity and performance. fileciteturn9file3

Wave 3 already covers market corpus integrity, workload classification, temporal reality, contradiction value, unknown handling, longitudinal value, debuggability, build-vs-buy and source expansion. fileciteturn9file11

These remain regression dependencies.

---

# 4. TDD Operating Rule

Every new test follows:

```text
Hypothesis
→ Golden dataset
→ Baseline
→ Failing test (RED)
→ Implementation/fix
→ GREEN
→ Full regression
→ Raw evidence
→ Comparative result
→ Negative-result classification
→ Claim approval
```

A test passes only when its predefined externally observable acceptance criteria pass.

---

# 5. P0 Test Cases

## W31-MKT-001 — Market Corpus Expansion

**Priority:** P0

Build a frozen market corpus with:

```text
≥100 independent tasks
≥10 tasks per workload class
≥12 workload classes
```

Classes:

```text
W1  Simple lookup
W2  Semantic lookup
W3  Multi-source synthesis
W4  Multi-hop reasoning
W5  Temporal reasoning
W6  Contradiction resolution
W7  Provenance/evidence
W8  Persistent memory
W9  Policy/constraint reasoning
W10 Agent planning
W11 Unknown/insufficient evidence
W12 Longitudinal evolution
```

Every task must define:

```text
expected answer
acceptable variants
expected evidence
expected relationships
expected temporal state
expected authority
expected ambiguity
```

Acceptance:

- [ ] ≥100 tasks.
- [ ] Versioned corpus.
- [ ] Manual/deterministic ground truth.
- [ ] ≥20% multi-source.
- [ ] ≥20% relationship-dependent.
- [ ] ≥10% temporal.
- [ ] ≥10% contradictory.
- [ ] ≥10% unknown/insufficient-evidence.
- [ ] No implementation-time leakage into holdout.

---

## W31-COMP-001 — RAG vs Graph-RAG vs AIKOQL

**Priority:** P0

Compare:

```text
A — Conventional RAG
B — Graph-RAG
C — AIKOQL
```

Hold constant:

```text
LLM
model version
prompt
corpus
task set
temperature
hardware
network
evaluation
```

Measure:

```text
task success
answer correctness
groundedness
evidence correctness
temporal correctness
multi-hop correctness
unknown handling
tool calls
LLM calls
tokens
p50/p95 latency
cost
```

Acceptance:

> AIKOQL demonstrates a predefined meaningful advantage in at least one important workload class without unacceptable correctness regression.

---

## W31-REAL-001 — Real LLM Agent Validation

**Priority:** P0

Run the full chain:

```text
User task
→ Agent
→ AIKOQL
→ Context
→ LLM
→ Final answer/action
```

Minimum:

```text
50 tasks × 5 repetitions
```

Measure:

```text
task success
correctness
groundedness
evidence correctness
unsupported claims
tool calls
retrieval retries
tokens
latency
cost
```

Acceptance:

- [ ] No Sev-1 behavior.
- [ ] No unauthorized action.
- [ ] Unknown tasks do not produce unsupported authoritative answers.
- [ ] At least one targeted workload class shows repeatable AIKOQL advantage.

---

## W31-DEC-001 — Evidence-to-Decision Correctness

**Priority:** P0

Scenario:

```text
Facts
→ relationships
→ conflicting evidence
→ authority
→ policy
→ agent decision
```

Expected:

- [ ] Current authoritative evidence selected.
- [ ] Historical evidence preserved where relevant.
- [ ] Material conflict disclosed.
- [ ] Unsafe instruction rejected.
- [ ] Decision supported by evidence.

This upgrades contradiction testing from **knowledge correctness** to **actual product outcome**.

---

## W31-TEMP-001 — Historical vs Current Agent Answer

**Priority:** P0

Timeline:

```text
v1 — January
v2 — March
v3 — June
```

Ask:

```text
What was true in February?
What is true now?
What changed?
Why?
```

Compare RAG / Graph-RAG / AIKOQL.

Measure separately:

```text
historical accuracy
current accuracy
change explanation
evidence accuracy
```

A current-answer pass must not compensate for historical failure.

---

## W31-UNK-001 — Four-State Epistemic Boundary

**Priority:** P0

Test:

```text
Known
Unknown
Conflicting
Historical-only
```

Expected:

```text
Known           → answer
Unknown         → insufficient evidence
Conflicting     → disclose conflict
Historical-only → do not present as current
```

Measure:

```text
false-confidence rate
incorrect-current rate
unsupported assertion rate
```

---

## W31-MEM-001 — Real Longitudinal Agent

**Priority:** P0

Run:

```text
Day 1
Day 7
Day 30
Day 60
Day 90
```

Introduce:

```text
new facts
superseded facts
corrections
contradictions
new relationships
deletions
```

Compare:

```text
stateless RAG
conversation-history memory
AIKOQL
```

Measure:

```text
task success
memory accuracy
stale-memory rate
important-fact retention
evidence retention
context tokens
LLM calls
developer intervention
```

The existing deterministic test already passes its 90-day checkpoints; this test validates whether that property survives a real agent. fileciteturn9file11

---

## W31-DEV-001 — Correct Build-vs-Buy Experiment

**Priority:** P0

The existing Wave 3 report compares **1,042 LOC retrieval baseline** against **9,410 LOC AIKOQL engine surface**. fileciteturn9file11

That does **not** prove developer productivity.

Build equivalent applications:

### Conventional

```text
Postgres
Vector DB
Graph/RAG
custom ingestion
custom provenance
custom temporal logic
custom conflict handling
custom context compilation
custom memory
```

### AIKOQL

```text
AIKOQL
```

Measure:

```text
application LOC
configuration
infrastructure components
custom retrieval
custom temporal code
custom provenance
custom conflict code
custom memory
developer hours
defects
time to add source
time to change knowledge rule
```

Acceptance:

> The moat claim must be based on reduced **application-owned complexity**, not AIKOQL's internal LOC.

---

## W31-COST-001 — Cost per Successful Task

**Priority:** P0

Do not claim AIKOQL is cheaper from token counts alone.

Calculate:

```text
Infrastructure
+ LLM
+ embedding
+ retrieval
+ agent/tool calls
--------------------------------
successful tasks
```

Compare:

```text
RAG
Graph-RAG
AIKOQL
```

Report:

```text
cost
successes
cost/success
failure rate
tokens/success
```

Acceptance:

> No universal "cheaper" claim unless AIKOQL wins across the explicitly declared workload scope.

---

## W31-REPRO-001 — Clean-Environment Reproduction

**Priority:** P0

Provide:

```text
dataset version
task set
baseline configuration
AIKOQL configuration
model
hardware
commands
evaluation code
raw output
summary output
```

An independent execution must reproduce the **direction and conclusion** of the headline result.

---

## W31-BIAS-001 — Benchmark Bias Audit

**Priority:** P0

Audit:

```text
question construction
corpus construction
baseline implementation
prompt wording
evaluation criteria
data leakage
AIKOQL-specific optimization
```

For every headline result ask:

> Could this test have been deliberately constructed to make AIKOQL win?

If yes:

- [ ] Create counter-task.
- [ ] Reclassify the result as exploratory.
- [ ] Do not use it for a primary public claim.

---

## W31-NEG-001 — Mandatory Falsification

**Priority:** P0

Maintain:

```text
wins.md
parity.md
losses.md
unknown.md
```

Required negative scenarios:

```text
simple exact lookup
simple document Q&A
small corpus
single-source query
```

AIKOQL must be allowed to lose.

This prevents benchmark gaming.

---

## W31-REG-001 — Full Regression

**Priority:** P0

Before every Wave 3.1 certification:

```text
MVP certification
→ Wave 2 certification
→ Wave 3 certification
→ Wave 3.1
```

No new work may introduce:

```text
knowledge-integrity regression
authorization bypass
retrieval regression
determinism regression
Wave-2 failure
```

---

# 6. P1 Test Cases

## W31-MEM-002 — Memory Compression

Compare:

```text
raw conversation history
summarized memory
AIKOQL structured memory
```

Measure:

```text
tokens
fact retention
relationship retention
conflict retention
provenance retention
task success
```

Primary metric:

> Correct task completion per retained token.

---

## W31-DEBUG-001 — End-to-End Debuggability

Inject:

```text
wrong source
stale source
wrong relationship
missing evidence
conflicting evidence
incorrect context
```

Developer diagnoses using normal AIKOQL observability.

Measure:

```text
root-cause identification
time to diagnosis
debugging operations
```

The existing deterministic test already diagnoses 5/5 injected failures; this test validates application-level diagnosis. fileciteturn9file11

---

## W31-IMPACT-001 — Knowledge Change Propagation

Change:

```text
Service A → depends_on → Service B
```

to:

```text
Service A → depends_on → Service C
```

Determine:

```text
affected KOs
affected relationships
affected contexts
affected answers
unaffected knowledge
```

Measure:

```text
impact precision
impact recall
false propagation
missed propagation
stale-answer rate
```

---

## W31-SCALE-001 — Knowledge Complexity Scaling

Run:

```text
1K KOs
10K KOs
100K KOs
1M KOs
```

Measure:

```text
task success
p50/p95
tokens
cost
context size
retrieval work
```

Question:

> Does AIKOQL's product advantage survive increasing knowledge complexity?

---

## W31-OSS-001 — OSS Time-to-Value

Fresh developer receives only:

```text
README
quickstart
examples
```

Tasks:

```text
install
start
ingest
query
add second source
create knowledge-backed agent
debug failure
```

Measure:

```text
time
completion rate
documentation failures
support intervention
```

Target must be established from baseline observations rather than invented.

---

# 7. Holdout Dataset

Use:

```text
development/
validation/
holdout/
```

The holdout dataset must not be exposed during implementation.

A result from a dataset repeatedly used during development is **not sufficient as primary market evidence**.

---

# 8. Product QA Gate

| Gate | Requirement | Priority |
|---|---|---|
| QA-P0-01 | Wave 2 remains GO | P0 |
| QA-P0-02 | ≥100 market tasks | P0 |
| QA-P0-03 | Frozen holdout | P0 |
| QA-P0-04 | RAG/Graph-RAG/AIKOQL comparison | P0 |
| QA-P0-05 | Real LLM agent validation | P0 |
| QA-P0-06 | Evidence-to-decision correctness | P0 |
| QA-P0-07 | Temporal agent correctness | P0 |
| QA-P0-08 | Epistemic boundary correctness | P0 |
| QA-P0-09 | Longitudinal agent validation | P0 |
| QA-P0-10 | Correct build-vs-buy methodology | P0 |
| QA-P0-11 | Cost per successful task | P0 |
| QA-P0-12 | Reproducibility | P0 |
| QA-P0-13 | Benchmark bias audit | P0 |
| QA-P0-14 | Negative evidence | P0 |
| QA-P0-15 | No critical regression | P0 |

---

# 9. Product QA Approval Rules

## APPROVED

Only if:

```text
ALL P0 PASS
AND
0 Sev-1
AND
0 Sev-2
AND
Wave 2 = GO
AND
≥1 meaningful workload class shows repeatable advantage
AND
build-vs-buy evidence is methodologically valid
AND
headline results are reproducible
AND
negative evidence is preserved
AND
public claims are scoped to evidence
```

## CONDITIONALLY APPROVED

Use when the software is stable but a particular market claim remains unproven.

Example:

```text
Product QA = PASS
Cost leadership claim = NOT PROVEN
```

## NO-GO

If any critical:

```text
correctness
security
reproducibility
knowledge integrity
```

gate fails.

---

# 10. Public Claim Approval

| Claim | Required evidence |
|---|---|
| Persistent knowledge | longitudinal benchmark |
| Temporal knowledge | temporal agent benchmark |
| Multi-hop reasoning | comparative multi-hop benchmark |
| Evidence-backed answers | provenance benchmark |
| Conflict-aware knowledge | decision benchmark |
| Unknown-aware AI | epistemic benchmark |
| Lower developer complexity | valid build-vs-buy |
| Easier multi-source integration | source-expansion study |
| Lower cost | cost/success benchmark |
| Faster | controlled latency comparison |
| Better than RAG | scoped workload comparison |
| Better for agents | real-agent benchmark |
| Production-ready | separate production-readiness assessment |

Never convert:

```text
benchmark result
```

into:

```text
universal product claim
```

---

# 11. Recommended Execution Sequence

## Phase A — Freeze

- [ ] Freeze Wave 2/Wave 3 baselines.
- [ ] Freeze validation corpus.
- [ ] Create holdout.
- [ ] Freeze evaluation methodology.

## Phase B — Market evidence

- [ ] ≥100 tasks.
- [ ] RAG.
- [ ] Graph-RAG.
- [ ] AIKOQL.
- [ ] Holdout evaluation.

## Phase C — Real agent

- [ ] Real LLM.
- [ ] Multi-hop.
- [ ] Temporal.
- [ ] Conflict.
- [ ] Unknown.
- [ ] Decision correctness.
- [ ] Longitudinal.

## Phase D — Developer evidence

- [ ] Build-vs-buy.
- [ ] Source expansion.
- [ ] Debugging.
- [ ] Knowledge impact.

## Phase E — Economics

- [ ] Cost/success.
- [ ] Scale.
- [ ] Tool efficiency.

## Phase F — Reproduction

- [ ] Clean environment.
- [ ] Independent execution.
- [ ] Raw artifacts.
- [ ] Final evidence review.

## Phase G — Release

- [ ] P0 review.
- [ ] Negative-evidence review.
- [ ] Marketing-claim audit.
- [ ] Product QA decision.

---

# 12. Required Evidence Artifacts

```text
docs/qa/
├── WAVE3-1-TDD-TEST-SPECIFICATION.md
├── qa-wave3-1-report.md
├── qa-wave3-1-results.json
└── qa-wave3-1-release-gate.md

docs/benchmarks/
├── wins.md
├── parity.md
├── losses.md
├── unknown.md
├── methodology.md
└── reproduction.md

docs/market/
├── corpus-version.md
├── task-taxonomy.md
├── build-vs-buy.md
└── public-claims.md
```

---

# 13. Senior QA Lead Final Verdict

The existing Wave 3 is already a legitimate **GO**, but it should not yet be interpreted as broad market validation. Its current market corpus is too small for that conclusion. fileciteturn9file10

The remaining QA work is therefore intentionally narrow.

The five questions that must be answered are:

```text
1. Does AIKOQL win on sufficiently diverse real workloads?

2. Does that advantage survive a real LLM/agent?

3. Does AIKOQL reduce application-owned complexity?

4. Does the advantage survive cost/economic measurement?

5. Can an independent developer reproduce the evidence?
```

If all five are answered positively, AIKOQL can move from:

```text
technically validated OSS project
        ↓
validated product
        ↓
defensible product thesis
```

The goal is **not** a perfect benchmark score.

The goal is evidence strong enough to survive an adversarial senior QA review.

## Final Definition of Done

```text
Wave 2
  ↓
Technical correctness certified

Wave 3
  ↓
Initial market signal

Wave 3.1
  ↓
Real workload diversity
+
Real agent validation
+
Developer-value validation
+
Economic validation
+
Independent reproducibility
+
Negative evidence
  ↓
PRODUCT QA APPROVED
```
