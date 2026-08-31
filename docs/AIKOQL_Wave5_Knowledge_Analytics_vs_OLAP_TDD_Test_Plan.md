# AIKOQL Wave 5 — Knowledge Analytics vs OLAP
## ClickHouse / StarRocks Comparative TDD & Implementation Test Plan

**Status:** Proposed  
**Methodology:** RED → GREEN → REFACTOR → REGRESSION → EVIDENCE

## 1. Objective

Wave 5 must **not** attempt to prove that AIKOQL is a faster ClickHouse or StarRocks.

ClickHouse is optimized for column-oriented analytical SQL, vectorized execution, large scans/aggregations and high-scale analytics. citeturn1search1turn1search14

StarRocks targets real-time OLAP with MPP, vectorized execution, cost-based optimization and materialized views. citeturn0search0turn0search6

Instead test three hypotheses:

```text
P1 — OLAP boundary
Can AIKOQL perform a useful conventional analytical subset?

P2 — Knowledge analytics
Does AIKOQL provide measurable advantage when queries require
entities + relationships + temporal state + provenance + conflicts?

P3 — Composability
Can AIKOQL use ClickHouse / StarRocks as analytical backends
rather than replacing them?
```

Target architecture:

```text
             AI APPLICATION / AGENT
                      │
                      ▼
             ┌──────────────────┐
             │      AIKOQL      │
             │ Knowledge Layer  │
             └────────┬─────────┘
                      │
        ┌─────────────┼─────────────┐
        ▼             ▼             ▼
   ClickHouse     StarRocks     PostgreSQL
   events/OLAP    warehouse     operational
```

---

# 2. Existing Evidence

Wave 3 already reports:

```text
Multi-hop:   AIKOQL 7/8 vs RAG 3/8
Provenance:  AIKOQL 2/2 vs RAG 1/2
```

and classifies those workload classes as Strong Fit. fileciteturn12file7

The existing Wave 3 plan also calls for scale-to-value, application-complexity and economic experiments. fileciteturn12file1

Therefore Wave 5 must extend this evidence rather than repeat basic retrieval tests.

---

# 3. Benchmark Dataset

Build a market-realistic dataset containing:

```text
10M+ events
1M+ entities
5M+ relationships
10+ years temporal history
multiple sources
conflicting assertions
source authority
provenance
corrections
supersession
deletions
```

Suggested domain:

```text
Customers
Accounts
Devices
Applications
Services
Transactions
Incidents
Policies
Locations
Events
```

---

# 4. P0 — Conventional OLAP Baseline

These tests establish the boundary where ClickHouse/StarRocks should remain the better tool.

## W5-OLAP-001 — Large Aggregation

```sql
SELECT customer_id, sum(amount)
FROM transactions
GROUP BY customer_id;
```

Run at:

```text
10M
100M
1B rows
```

Measure:

```text
p50 / p95 / p99
CPU
memory
throughput
storage
correctness
```

Do not claim AIKOQL superiority if conventional OLAP wins.

## W5-OLAP-002 — Time-Series Analytics

Examples:

```text
events/minute
error rate/service
p95 latency/service
traffic/region
```

## W5-OLAP-003 — High-Cardinality GROUP BY

Dimensions:

```text
service
device
customer
region
error_code
day
```

## W5-OLAP-004 — Large Multi-Table Join

Join:

```text
customers
transactions
devices
events
```

Measure latency, memory, CPU and correctness.

StarRocks explicitly targets large joins and high-dimensional analytics through MPP/vectorized execution. citeturn0search6

---

# 5. P0 — Knowledge Analytics

## W5-KA-001 — Multi-Hop Dependency Analysis

Question:

> Which customers are affected by a service dependency change, and why?

Path:

```text
Customer
→ Account
→ Application
→ Service
→ Dependency
→ Incident
```

Compare:

```text
ClickHouse
StarRocks
AIKOQL
```

Measure:

```text
answer correctness
path correctness
evidence completeness
missed hops
false hops
latency
application LOC
developer hours
```

Primary metric:

```text
Correct answer / application complexity
```

---

## W5-KA-002 — Temporal Knowledge

Timeline:

```text
Architecture v1 → January
Architecture v2 → March
Architecture v3 → June
Incident → February
```

Questions:

```text
What was active during the incident?
What is active now?
What changed?
```

Measure separately:

```text
historical accuracy
current accuracy
change explanation
evidence accuracy
```

---

## W5-KA-003 — Provenance-Aware Analytics

Question:

> Why is Customer X high risk?

Expected:

```text
current classification
supporting evidence
source
timestamp
authority
superseded evidence
```

Measure:

```text
correctness
evidence completeness
stale-evidence rate
application LOC
auxiliary tables
application-side rules
```

---

## W5-KA-004 — Conflicting Evidence

Sources:

```text
CRM      → ACTIVE
Fraud    → BLOCKED
Policy   → BLOCKED overrides ACTIVE
```

Expected:

```text
effective state = BLOCKED
conflict disclosed
authoritative source identified
```

Measure:

```text
effective-state correctness
conflict detection
authority selection
evidence completeness
application complexity
```

---

## W5-KA-005 — Unknown / Insufficient Evidence

If no authoritative relationship exists:

```text
Expected = UNKNOWN / INSUFFICIENT EVIDENCE
```

Not:

```text
false
probably yes
```

Measure:

```text
false-positive rate
unsupported-assertion rate
evidence correctness
```

---

## W5-KA-006 — Evidence-Backed Aggregate

Question:

> How many high-risk customers are currently supported by at least two independent authoritative sources?

This combines:

```text
aggregation
entity state
source independence
authority
temporal validity
```

Measure:

```text
result correctness
source-independence correctness
temporal correctness
latency
application complexity
```

---

## W5-KA-007 — Change Impact Analysis

Change:

```text
Service A → depends_on → B
```

to:

```text
Service A → depends_on → C
```

Expected:

```text
affected customers
affected applications
affected incidents
affected evidence
affected contexts
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

## W5-KA-008 — Historical Reconstruction

Question:

> Reconstruct what the organization knew about Customer X on 2025-06-01.

Expected:

```text
facts valid then
sources available then
relationships valid then
superseded/current facts correctly separated
```

Measure:

```text
historical correctness
provenance correctness
reconstruction completeness
```

---

# 6. P0 — Agent Knowledge Analytics

## W5-AGENT-001 — Minimal Evidence Context

Compare:

```text
ClickHouse-derived context
StarRocks-derived context
RAG
AIKOQL
```

Measure:

```text
task success
context tokens
irrelevant facts
missing evidence
stale facts
LLM calls
tool calls
cost/successful task
```

Primary metric:

```text
Successful investigation / context tokens
```

---

# 7. P0 — Build-vs-Buy

This is the most important commercial experiment.

Do **not** compare AIKOQL LOC with ClickHouse LOC.

Build equivalent application capabilities.

### Conventional stack

```text
ClickHouse or StarRocks
+
relationship tables
+
temporal logic
+
provenance
+
conflict rules
+
entity resolution
+
context compiler
+
agent integration
+
synchronization jobs
```

### AIKOQL

```text
AIKOQL
+
application
```

Measure:

```text
application LOC
developer hours
services
tables
indexes
custom rules
synchronization jobs
retry logic
test count
maintenance surface
defects
```

Acceptance:

> The moat claim is based on reduced **application-owned complexity**, not AIKOQL's internal LOC.

---

# 8. P0 — Schema and Query Complexity

For the same business capability measure:

```text
tables
columns
indexes
foreign keys
materialized views
ETL jobs
application data structures
```

and:

```text
SQL statements
JOIN count
CTEs
subqueries
application post-processing
round trips
```

Compare against the AIKOQL implementation.

Acceptance:

```text
same correctness
+
less application-owned complexity
```

Fewer lines alone are not sufficient evidence.

---

# 9. P1 — Materialized Knowledge

StarRocks supports materialized views and automatic query rewrite. citeturn0search2turn0search11

Test whether AIKOQL can maintain derived knowledge without application-owned duplicated logic.

Example:

```text
current_customer_risk
```

derived from:

```text
transactions
devices
incidents
policies
sources
temporal validity
```

Change one source and verify:

```text
knowledge update
→ derived state update
```

Measure:

```text
freshness
correctness
update cost
maintenance complexity
query latency
```

---

# 10. P1 — Incremental Knowledge Update

Change one source event.

Expected:

```text
only affected knowledge is recomputed
```

Measure:

```text
affected KOs
recomputed KOs
unaffected KOs
CPU
latency
stale window
```

Metric:

```text
Correct affected-state propagation / recomputation work
```

---

# 11. P1 — Scale by Knowledge Complexity

Do not measure only row count.

Test:

```text
10K KOs
100K KOs
1M KOs
10M KOs
```

and:

```text
1 relationship/entity
5 relationships/entity
20 relationships/entity
100 relationships/entity
```

Measure:

```text
multi-hop latency
temporal latency
provenance traversal
context compilation
memory
storage
```

---

# 12. P1 — Agent Concurrency

Run:

```text
100 agents
1,000 agents
10,000 agents
```

mixed with:

```text
OLAP-style queries
knowledge queries
context compilation
updates
```

Measure:

```text
p50
p95
p99
error rate
resource utilization
```

Question:

> Does knowledge reasoning remain predictable under concurrent agent workloads?

---

# 13. P1 — Federation / Pushdown

This is strategically critical.

Example:

```text
ClickHouse
   │
   │ filter/aggregate
   ▼
AIKOQL
   │
   │ knowledge traversal
   ▼
Agent
```

Example task:

```text
ClickHouse:
Find devices with >10,000 failures.

AIKOQL:
Which customers own those devices,
which incidents affected them,
what policy applies,
and what evidence supports the conclusion?
```

Measure:

```text
data transferred
source queries
AIKOQL work
latency
final correctness
```

Goal:

> Prove AIKOQL can complement OLAP rather than replace it.

---

# 14. P1 — Pushdown Correctness

For every delegated operation:

```text
filter
projection
aggregation
time-range selection
```

verify:

```text
pushed result == canonical result
```

Test:

```text
nulls
duplicates
time boundaries
filters
joins
aggregates
```

This becomes a federation invariant.

---

# 15. P1 — Cross-System Provenance

Example:

```text
ClickHouse query
→ aggregate
→ AIKOQL assertion
→ provenance
→ source query/record
→ timestamp
```

Agent question:

> Where did this conclusion come from?

Expected:

```text
AIKOQL evidence
+
analytical source
+
query/record reference
+
time
```

---

# 16. P1 — Cost per Successful Investigation

Include:

```text
database compute
storage
network
LLM
embedding
agent/tool calls
application infrastructure
```

Divide by:

```text
successful investigations
```

Compare:

```text
ClickHouse stack
StarRocks stack
RAG
AIKOQL
AIKOQL + ClickHouse
```

No universal "cheaper" claim is permitted.

---

# 17. P1 — Operational Complexity

Count:

```text
services
databases
queues
workers
ETL jobs
scheduled jobs
materialized views
sync processes
application libraries
custom retry logic
custom conflict logic
custom temporal logic
```

Compare:

```text
conventional stack
AIKOQL
AIKOQL + OLAP
```

This is a high-value moat experiment.

---

# 18. P1 — Failure Semantics

Inject:

```text
ClickHouse unavailable
StarRocks unavailable
AIKOQL unavailable
stale source
partial update
connector failure
network timeout
```

Determine:

```text
what remains queryable
what becomes stale
what becomes UNKNOWN
what must fail closed
```

Never turn unavailable evidence into fabricated certainty.

---

# 19. P2 — Mandatory OLAP Losses

Explicitly record workloads where AIKOQL loses:

```text
massive SUM
massive GROUP BY
wide scans
dashboard workloads
high-cardinality aggregation
large fact-table joins
```

Expected classifications may be:

```text
ClickHouse wins
StarRocks wins
AIKOQL not recommended
```

This negative evidence is mandatory.

---

# 20. P2 — Knowledge Crossover Curve

Increase workload complexity:

```text
L1 aggregation
L2 filtering + joins
L3 multi-source
L4 relationships
L5 temporal
L6 provenance
L7 conflicts
L8 historical reconstruction
L9 agent context
```

Measure:

```text
correctness
latency
cost
query complexity
application complexity
```

The benchmark should discover whether a crossover exists:

```text
OLAP advantage
      │
      │
      └───────────────┐
                      │
                      ▼
              knowledge complexity
                      │
                      ▼
               AIKOQL advantage
```

Do not assume the curve before testing.

---

# 21. P0 Release Gates

| Gate | Requirement |
|---|---|
| W5-G01 | Conventional OLAP baseline reproducible |
| W5-G02 | ClickHouse/StarRocks configurations documented |
| W5-G03 | AIKOQL correctness has no regression |
| W5-G04 | ≥3 knowledge workload classes evaluated |
| W5-G05 | ≥1 repeatable AIKOQL Strong-Fit workload |
| W5-G06 | Build-vs-buy uses application-owned complexity |
| W5-G07 | Cost measured per successful task/investigation |
| W5-G08 | Negative OLAP evidence preserved |
| W5-G09 | Federation result reproducible |
| W5-G10 | No unsupported "AIKOQL replaces ClickHouse" claim |
| W5-G11 | Independent reproduction completed |
| W5-G12 | Wave 2 + Wave 3 + Wave 3.1 regression remains GO |

---

# 22. Benchmark Artifacts

```text
docs/benchmarks/knowledge-analytics/
├── README.md
├── dataset.md
├── schema/
├── tasks/
├── ground-truth/
├── clickhouse/
├── starrocks/
├── aikoql/
├── federation/
├── results/
├── wins.md
├── parity.md
├── losses.md
└── unknown.md
```

Machine-readable:

```text
qa-wave5-results.json
```

Human-readable:

```text
qa-wave5-report.md
qa-wave5-release-gate.md
qa-wave5-failures.md
```

---

# 23. TDD Implementation Order

## Phase A — RED

Write failing tests first:

```text
W5-OLAP-001
W5-OLAP-002
W5-KA-001
W5-KA-002
W5-KA-003
W5-KA-004
W5-KA-006
W5-KA-007
W5-KA-008
```

Missing capabilities must fail honestly.

## Phase B — Minimum Knowledge Execution

Candidate primitives:

```text
ENTITY
TRAVERSE
TEMPORAL_FILTER
EVIDENCE_FILTER
AUTHORITY
CONFLICT_RESOLVE
PROVENANCE_TRACE
IMPACT
CONTEXT_COMPILE
```

Correctness before optimization.

## Phase C — Comparative Harness

Implement adapters:

```text
ClickHouse
StarRocks
AIKOQL
```

All consume the same dataset, tasks and ground truth.

## Phase D — GREEN

For each test:

```text
RED
→ implementation
→ GREEN
→ full regression
→ evidence
```

## Phase E — Optimization

Only after correctness:

```text
indexing
caching
materialization
pushdown
parallel traversal
vectorization
batch execution
```

Then rerun the entire benchmark.

---

# 24. What AIKOQL Should Optimize

Do not blindly copy an OLAP execution model.

Traditional analytical operators:

```text
SCAN
FILTER
PROJECT
JOIN
GROUP BY
SORT
AGGREGATE
```

Potential knowledge-native operators:

```text
ENTITY
RESOLVE
TRAVERSE
TEMPORAL_FILTER
EVIDENCE_FILTER
AUTHORITY
CONFLICT_RESOLVE
PROVENANCE_TRACE
IMPACT
CONTEXT_COMPILE
```

This is a hypothesis.

Wave 5 determines whether these deserve first-class execution primitives.

---

# 25. Highest-Value Benchmark

If engineering capacity is limited, build this first:

## Knowledge Investigation Benchmark

Dataset:

```text
10M events
1M entities
5M relationships
10 years history
multiple sources
conflicts
provenance
```

Tasks:

```text
10 simple analytics
10 multi-source
10 multi-hop
10 temporal
10 provenance
10 conflict
10 historical
10 unknown
10 impact analysis
10 agent investigations
```

Treatments:

```text
ClickHouse
StarRocks
RAG
AIKOQL
AIKOQL + ClickHouse
```

Measure:

```text
correctness
evidence correctness
temporal correctness
application complexity
query complexity
latency
tokens
LLM calls
tool calls
cost
```

This can become the foundation of AIKOQL's analytical positioning.

---

# 26. Four Possible Outcomes

### A — AIKOQL loses everywhere

Conclusion:

> AIKOQL should remain a knowledge layer, not an analytical execution engine.

Valid result.

### B — AIKOQL wins only on knowledge semantics

Conclusion:

> Specialize in knowledge analytics and delegate conventional OLAP.

Likely the most attractive outcome.

### C — AIKOQL wins on semantics + application complexity

Conclusion:

> Evidence supports investment in a knowledge-native analytical execution engine.

### D — AIKOQL wins on both

Conclusion:

> AIKOQL may have a path toward broader analytical workloads.

Do not assume D.

---

# 27. Website / Moat Evidence

If Wave 5 succeeds, do **not** market:

> "AIKOQL is faster than ClickHouse."

A stronger positioning is:

> **Use ClickHouse for analytics. Use AIKOQL when analytics become knowledge.**

Demonstration:

```text
ClickHouse
10M events
      ↓
AIKOQL
entities + relationships + temporal state + provenance
      ↓
Agent
"Why is this customer at risk?"
      ↓
evidence-backed answer
```

Potential moat claim:

> **AIKOQL turns analytical data into durable, queryable knowledge without forcing every application to rebuild entity, relationship, temporal, provenance and conflict logic.**

This claim becomes public only after the build-vs-buy and knowledge-analytics experiments support it.

---

# 28. Senior QA Position

Wave 5 is a **boundary-discovery benchmark**, not a ClickHouse benchmark.

The objective is to discover:

```text
Where OLAP is the right abstraction
            ↓
Where knowledge analytics begins
            ↓
Where AIKOQL becomes valuable
            ↓
Whether AIKOQL can use OLAP engines underneath it
```

The key engineering rule is:

> **Do not build ClickHouse inside AIKOQL until the benchmark proves that knowledge-native execution requires it.**

Start with semantic correctness.

If:

```text
multi-hop
+
temporal
+
provenance
+
conflict
+
impact analysis
+
agent context
```

produce a repeatable workload class where conventional OLAP stacks require significantly more application-owned machinery, then invest in specialized AIKOQL execution.

That turns the ClickHouse/StarRocks comparison from a competitive distraction into a potential **AIKOQL architectural moat**.
