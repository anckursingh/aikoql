# AIKOQL — Conversational AI, Chatbot Memory & LLM Knowledge Certification Test Suite

**Document ID:** QA-AIKOQL-CHATBOT-001  
**Related suite:** QA-AIKOQL-AGENT-MEMORY-001  
**Role:** Senior QA / Quality Engineering  
**Status:** Proposed certification suite  
**Scope:** Chatbots, conversational agents, persistent memory, semantic/episodic/procedural memory, knowledge retrieval, LLM context reduction, provenance, temporal truth, contradiction handling, personalization, authorization, RAG comparison, and agentic execution.

---

# 1. Purpose

The previous AIKOQL certification suite tested the broader Agent Knowledge OS capabilities.

This companion suite focuses specifically on the **conversational AI / chatbot use case**.

The central hypothesis under test is:

> **AIKOQL can act as a durable, structured knowledge and memory layer for chatbots, reducing the need for the LLM to reconstruct knowledge from raw documents, conversation history, or retrieved chunks.**

This is deliberately stronger than testing whether AIKOQL can perform RAG.

The suite must establish whether AIKOQL can provide:

- persistent conversational memory
- semantic memory
- episodic memory
- procedural memory
- user/profile knowledge
- organizational knowledge
- temporal knowledge
- relationship-aware retrieval
- provenance-backed answers
- contradiction-aware answers
- constraint-aware responses
- personalized context
- memory consolidation
- knowledge evolution
- efficient LLM context
- lower dependency on raw-context reasoning
- safe agentic actions

---

# 2. Product Hypothesis

A conventional chatbot:

```text
User
  ↓
LLM
  ↓
Conversation history / RAG
  ↓
Retrieved chunks
  ↓
LLM reconstructs knowledge
  ↓
Answer
```

AIKOQL-backed chatbot:

```text
User
  ↓
Conversation / Intent Analysis
  ↓
AIKOQL
  ↓
Knowledge + Memory + Evidence + Constraints
  ↓
Context Compiler
  ↓
LLM
  ↓
Answer / Action
```

The test suite must determine whether the second architecture provides measurable improvements in:

```text
Accuracy
Consistency
Personalization
Grounding
Explainability
Temporal correctness
Safety
Latency
Token efficiency
Memory continuity
Task completion
```

---

# 3. Important QA Principle

AIKOQL must **not** be certified merely because it returns relevant records.

The test is:

> **Can the chatbot answer correctly because AIKOQL supplies structured, authoritative, contextual knowledge?**

And:

> **Can the chatbot preserve useful knowledge between conversations without treating every conversation transcript as durable truth?**

---

# 4. Certification Levels

| Level | Capability |
|---|---|
| C1 | Conversational Memory |
| C2 | Persistent Semantic Memory |
| C3 | Episodic Memory |
| C4 | Procedural Memory |
| C5 | Knowledge-Grounded Chatbot |
| C6 | Personalized Chatbot |
| C7 | Constraint-Aware Agent |
| C8 | Agentic Knowledge Chatbot |

A higher level requires all lower levels to pass.

---

# 5. Test Architecture

The test harness should contain:

```text
chatbot-tests/
├── fixtures/
│   ├── users/
│   ├── conversations/
│   ├── products/
│   ├── policies/
│   ├── procedures/
│   ├── episodes/
│   ├── contradictions/
│   └── temporal/
│
├── baseline/
│   ├── llm-only/
│   ├── rag/
│   └── graph-rag/
│
├── aikoql/
│   ├── semantic-memory/
│   ├── episodic-memory/
│   ├── procedural-memory/
│   └── context/
│
├── evaluation/
│   ├── accuracy/
│   ├── grounding/
│   ├── latency/
│   ├── tokens/
│   └── safety/
│
└── certification/
```

---

# 6. Baselines

Every major chatbot capability should be compared against at least:

### Baseline A — LLM only

```text
LLM + conversation context
```

### Baseline B — Conventional RAG

```text
LLM + vector DB + chunks
```

### Baseline C — Graph-RAG

```text
LLM + graph + retrieval
```

### Treatment D — AIKOQL

```text
LLM + AIKOQL
```

Where appropriate, also test:

```text
AIKOQL only
```

for deterministic knowledge queries that should not require an LLM.

---

# 7. Test Data Categories

Create deterministic datasets for:

```text
Customer support
Enterprise policy
Product support
Personal assistant
Developer assistant
HR assistant
Banking assistant
E-commerce assistant
Technical support
Agentic operations
```

Each dataset must contain:

- facts
- relationships
- documents
- conversation history
- conflicting information
- historical versions
- procedures
- user preferences
- permissions
- sensitive information

---

# 8. Conversational Memory

## CHAT-MEM-001 — Same-session memory

Conversation:

```text
User: My preferred language is English.
Assistant: Understood.
User: What language should you use?
```

### Expected

Assistant answers:

```text
English
```

without requiring the LLM to infer it from unrelated context.

---

## CHAT-MEM-002 — Cross-session memory

Session 1:

```text
User: I prefer concise answers.
```

Session 2:

```text
User: Explain AIKOQL.
```

### Expected

The preference is available through AIKOQL memory.

---

## CHAT-MEM-003 — Memory persistence

Restart chatbot and database.

Repeat query.

### Expected

Durable memory remains available.

---

## CHAT-MEM-004 — Explicit memory

User says:

> Remember that my preferred deployment environment is AWS.

### Expected

A durable memory candidate is created.

---

## CHAT-MEM-005 — Ephemeral statement

User says:

> I am currently testing this on AWS.

### Expected

The statement is not automatically converted into a permanent user preference unless the memory policy allows it.

---

# 9. Memory Classification

## CLASS-001 — Fact

```text
User's company = ACME
```

Expected:

```text
semantic memory
```

---

## CLASS-002 — Preference

```text
User prefers concise answers.
```

Expected:

```text
user preference KO
```

---

## CLASS-003 — Episode

```text
User contacted support on July 10.
```

Expected:

```text
episodic KO
```

---

## CLASS-004 — Procedure

```text
How to reset account.
```

Expected:

```text
procedural KO
```

---

## CLASS-005 — Program

```text
ResetAccount
```

Expected:

```text
Program-as-KO
```

---

# 10. Memory Consolidation

## CONS-001 — Conversation → Episode

Given a completed support interaction:

```text
problem
action
resolution
outcome
```

Expected:

```text
Episode KO
```

with provenance to the conversation.

---

## CONS-002 — Episode → Fact

Three independent successful conversations establish:

```text
Product X requires firmware version Y.
```

Expected:

A candidate semantic fact can be derived with:

```text
confidence
evidence_count
derived_from
```

---

## CONS-003 — Episode → Procedure

Repeated successful interactions show:

```text
Step A
→ Step B
→ Step C
```

Expected candidate procedure:

```text
Procedure KO
```

with references to episodes.

---

## CONS-004 — Failed experience

One procedure execution fails.

Expected:

- failed episode retained
- procedure confidence updated according to policy
- failure reason preserved
- previous successful evidence not deleted

---

# 11. Personalization

## PERS-001 — User preference

User preference:

```text
response_style = concise
```

Expected chatbot behavior changes.

---

## PERS-002 — Preference provenance

Query:

> Why do you answer me concisely?

Expected:

```text
source = user statement
timestamp
confidence
```

---

## PERS-003 — Preference conflict

Old:

```text
User prefers concise responses.
```

New:

```text
User now prefers detailed explanations.
```

Expected current preference:

```text
detailed
```

Historical preference remains available.

---

## PERS-004 — Preference scope

User preference must not automatically become an organization-wide preference.

---

# 12. Semantic Knowledge

## SEM-001 — Direct fact

Knowledge:

```text
Product A supports Windows.
```

Question:

> Does Product A support Windows?

Expected:

```text
Yes
```

---

## SEM-002 — Multi-hop knowledge

```text
Product A
 → compatible_with
Device B
 → supported_in
Region C
```

Question:

> Can I use Product A on Device B in Region C?

Expected correct multi-hop reasoning.

---

## SEM-003 — Structured answer without LLM reasoning

Where the answer is deterministic, AIKOQL should be able to resolve it before invoking the LLM.

Acceptance:

```text
LLM invocation = 0
```

for an explicitly configured deterministic query path.

---

# 13. Episodic Memory

## EP-001 — Previous interaction

User asks:

> What did we decide last time?

Expected retrieval of the relevant episode.

---

## EP-002 — Episode timeline

Retrieve all interactions concerning an account.

Expected chronological ordering.

---

## EP-003 — Episode relationship

Expected:

```text
User
 ↓
Conversation
 ↓
Issue
 ↓
Action
 ↓
Outcome
```

---

## EP-004 — Episode provenance

Every summarized episode must point back to its source conversation or evidence.

---

# 14. Temporal Memory

## TEMP-CHAT-001 — Current truth

Version 1:

```text
Product price = €100
valid_until = July 1
```

Version 2:

```text
Product price = €120
valid_from = July 1
```

Question today:

> What is the current price?

Expected:

```text
€120
```

---

## TEMP-CHAT-002 — Historical truth

Question:

> What was the price in June?

Expected:

```text
€100
```

---

## TEMP-CHAT-003 — Future truth

Future pricing must not be returned as current unless explicitly requested.

---

# 15. Contradiction Handling

## CONTR-CHAT-001 — Conflicting documents

Document A:

```text
Refund period = 30 days
```

Document B:

```text
Refund period = 60 days
```

Expected:

AIKOQL returns conflict rather than silently selecting one.

---

## CONTR-CHAT-002 — Authority

Policy document marked authoritative.

Expected authoritative answer.

---

## CONTR-CHAT-003 — Explain conflict

User:

> Why are there two refund periods?

Expected:

```text
Claim A
Claim B
sources
dates
authority
resolution
```

---

# 16. Provenance-Grounded Chatbot

## PROV-CHAT-001 — Source-backed answer

Question:

> What is the refund policy?

Expected answer includes source reference where configured.

---

## PROV-CHAT-002 — Unsupported claim

Ask something not present in the knowledge base.

Expected:

```text
I don't have sufficient information.
```

rather than hallucinating.

---

## PROV-CHAT-003 — Confidence threshold

Low-confidence inferred knowledge should not be presented as authoritative fact.

---

# 17. RAG vs AIKOQL

This is one of the most important test groups.

## COMP-001 — Simple factual question

Compare:

```text
LLM
RAG
Graph-RAG
AIKOQL
```

Measure:

```text
accuracy
latency
tokens
retrieval count
```

---

## COMP-002 — Multi-hop question

Example:

> Which customers are affected by the deprecation of service X?

Expected AIKOQL to resolve:

```text
Service
 ↓
Dependency
 ↓
Product
 ↓
Customer
```

Compare against RAG and Graph-RAG.

---

## COMP-003 — Temporal question

Example:

> Which version was active when customer X opened the ticket?

This should strongly test temporal knowledge.

---

## COMP-004 — Contradiction question

Test which architecture correctly detects conflicting knowledge.

---

## COMP-005 — Provenance question

Ask:

> Where did this answer come from?

Measure evidence completeness.

---

# 18. LLM Dependency Reduction

This is a critical AIKOQL product hypothesis.

## LLM-001 — Deterministic query path

Question:

> What is the current status of ticket #123?

Expected:

```text
AIKOQL resolves answer
LLM only verbalizes if needed
```

---

## LLM-002 — Structured context reduction

Measure:

```text
tokens sent to LLM
```

with:

```text
RAG
AIKOQL
```

Expected AIKOQL context should be materially smaller for equivalent knowledge.

Do not define an arbitrary improvement threshold initially. Establish a baseline across a representative corpus.

---

## LLM-003 — No repeated knowledge derivation

The LLM should not repeatedly infer:

```text
A → B
B → C
C → D
```

when AIKOQL already stores these relationships.

---

## LLM-004 — Answer without document dumping

Provide 100 documents containing the answer.

AIKOQL should return the relevant knowledge rather than injecting all relevant chunks into the LLM context.

---

# 19. Context Compilation

AIKOQL should have a context compilation layer.

Given:

```text
User question
User identity
Conversation state
Task
Permissions
```

compile:

```text
Relevant KOs
Relevant relationships
Relevant memories
Relevant procedures
Constraints
Evidence
```

## CTX-001

Same question for two users with different permissions.

Expected different contexts.

---

## CTX-002

Same question at different times.

Expected context changes according to temporal state.

---

## CTX-003

Same question after knowledge update.

Expected new context.

---

# 20. Context Minimization

## CTX-MIN-001

1000 KOs exist.

Only 20 are relevant.

Expected context compiler returns only relevant knowledge.

---

## CTX-MIN-002

Irrelevant conversation history must not be forwarded.

---

## CTX-MIN-003

Duplicate knowledge should be deduplicated.

---

# 21. Authorization-Aware Memory

## AUTH-CHAT-001

User A cannot retrieve User B's private memory.

---

## AUTH-CHAT-002

Support agent can access customer support history but not internal HR memory.

---

## AUTH-CHAT-003

Admin can retrieve broader organizational knowledge.

---

## AUTH-CHAT-004

An agent must not use unauthorized KOs to construct its context.

---

# 22. Sensitive Memory

Test:

```text
PII
financial information
credentials
security data
private conversations
```

Expected:

- access policy enforced
- storage policy enforced
- retrieval policy enforced
- auditability retained
- encryption policy applied

---

# 23. Memory Forgetting / Retention

## RET-CHAT-001

Create temporary memory with short retention.

Expected automatic expiry according to policy.

---

## RET-CHAT-002

Delete user memory.

Expected:

- current retrieval no longer returns it
- deletion semantics are deterministic
- retained audit metadata follows policy

---

## RET-CHAT-003

Historical immutable evidence must remain only when policy/legal requirements permit it.

---

# 24. Procedural Chatbot

## PROC-CHAT-001 — Known procedure

User:

> How do I reset my account?

Expected chatbot retrieves the authoritative procedure.

---

## PROC-CHAT-002 — Procedure version

Procedure v1 and v2 exist.

Expected current procedure returned.

---

## PROC-CHAT-003 — Procedure constraint

Procedure requires MFA.

User lacks MFA.

Expected chatbot does not instruct/exe­cute an invalid path.

---

## PROC-CHAT-004 — Procedure explanation

Ask:

> Why do I need MFA?

Expected explanation from procedure/policy knowledge.

---

# 25. Program-as-KO Chatbot

## PROG-CHAT-001 — Program discovery

User intent maps to:

```text
ResetAccount
```

Expected program discovery.

---

## PROG-CHAT-002 — Preconditions

Program requires authenticated identity.

Unauthenticated user attempts execution.

Expected execution denied.

---

## PROG-CHAT-003 — Approval

Sensitive program requires human approval.

Expected chatbot requests approval rather than executing.

---

## PROG-CHAT-004 — Postcondition

Program completes but postcondition fails.

Expected:

```text
execution = unsuccessful
episode = recorded
user = informed
```

---

# 26. Agentic Action Safety

## SAFE-CHAT-001

Question asks for an informational answer.

Expected no side-effecting program execution.

---

## SAFE-CHAT-002

Question implies action.

Expected chatbot distinguishes:

```text
explain
vs
execute
```

---

## SAFE-CHAT-003

Ambiguous action.

Expected clarification or safe default.

---

## SAFE-CHAT-004

Unauthorized action.

Expected denial.

---

# 27. Knowledge Evolution During Conversation

## EVO-CHAT-001

User corrects a known fact.

Expected:

```text
new evidence
→ knowledge update
→ old value historical
```

---

## EVO-CHAT-002

User provides incorrect information conflicting with authoritative source.

Expected policy determines whether it becomes:

```text
claim
```

rather than authoritative truth.

---

## EVO-CHAT-003

Authoritative source changes.

Expected chatbot reflects new knowledge without retraining the LLM.

This is a critical acceptance scenario.

---

# 28. "No Model Retraining" Test

Change:

```text
company policy
product price
support procedure
service endpoint
```

without changing the LLM.

Expected chatbot immediately uses the updated AIKOQL knowledge according to freshness/propagation guarantees.

This demonstrates separation between:

```text
model intelligence
```

and:

```text
application knowledge
```

---

# 29. Chatbot Restart / Continuity

## CONT-001

Conversation ends.

Restart chatbot.

Ask about durable information.

Expected continuity.

---

## CONT-002

Restart AIKOQL.

Expected durable memory survives.

---

## CONT-003

Upgrade AIKOQL schema.

Expected existing memory remains accessible or is migrated according to documented migration semantics.

---

# 30. Memory Isolation

Test:

```text
User A
User B
Tenant A
Tenant B
Agent A
Agent B
```

Expected no accidental cross-contamination.

---

# 31. Multi-Agent Shared Knowledge

Create:

```text
Support Agent
Sales Agent
Operations Agent
```

All share organization knowledge.

Expected:

```text
shared authoritative semantic knowledge
```

while:

```text
agent-private memory
```

remains isolated.

---

# 32. Agent Memory Ownership

Every durable memory should have an explicit scope:

```text
user
agent
session
conversation
project
tenant
organization
global
```

Test that scope cannot be accidentally widened.

---

# 33. Memory Explainability

For every important memory, chatbot should be able to answer:

```text
What do you remember?
Why do you remember it?
Where did you learn it?
When did you learn it?
Is it still valid?
Who can access it?
What evidence supports it?
```

---

# 34. Hallucination Test

Create a knowledge base containing only:

```text
Product A supports Windows.
```

Ask:

> Does Product A support Linux?

Expected:

```text
Unknown / insufficient evidence
```

not:

```text
Yes
```

---

# 35. Knowledge Boundary Test

Ask a question deliberately outside the knowledge base.

Expected:

AIKOQL returns:

```text
no authoritative knowledge
```

and the chatbot follows the configured fallback policy.

This must be differentiated from:

```text
knowledge exists but retrieval failed
```

---

# 36. Retrieval Failure Test

Temporarily disable one index.

Expected:

- failure is detectable
- fallback behavior follows policy
- system does not claim knowledge it failed to retrieve

---

# 37. Index Independence

If AIKOQL has:

```text
canonical KO
graph index
vector index
lexical index
```

delete/rebuild one derived index.

Expected canonical knowledge remains correct.

---

# 38. Conversation Summarization Test

Summarize a 100-message conversation.

Expected summary preserves:

```text
facts
decisions
actions
open issues
entities
constraints
outcomes
```

and does not invent facts.

---

# 39. Summary Provenance

Every important summarized fact must trace back to:

```text
conversation
message range
speaker
timestamp
```

where supported.

---

# 40. Memory Compression Test

Compare:

```text
100-message transcript
```

versus:

```text
AIKOQL structured memory
```

Measure:

```text
context tokens
answer accuracy
retrieval latency
```

The hypothesis is that structured memory can provide equivalent or better answer quality with substantially less context.

This is a measurement target, not an assumed result.

---

# 41. Chatbot Knowledge Freshness

## FRESH-001

Update source knowledge.

Expected AIKOQL freshness SLA is met.

Measure:

```text
source update
→ ingestion
→ KO update
→ query visibility
→ chatbot visibility
```

---

# 42. Cache Correctness

If chatbot/AIKOQL caches context:

Update underlying KO.

Expected stale cache cannot return incorrect authoritative knowledge beyond the documented consistency window.

---

# 43. Memory Race Conditions

Two concurrent conversations update the same user preference.

Example:

```text
Conversation A:
prefers concise

Conversation B:
prefers detailed
```

Expected deterministic conflict/version semantics.

---

# 44. Conversation Concurrency

Multiple messages from the same user arrive concurrently.

Expected:

- no lost memories
- no corrupted conversation state
- deterministic ordering semantics

---

# 45. Performance Tests

Measure:

```text
memory lookup P50/P95/P99
knowledge query P50/P95/P99
context compilation P50/P95/P99
cross-session retrieval
multi-hop query
hybrid retrieval
```

Compare:

```text
LLM + RAG
LLM + Graph-RAG
LLM + AIKOQL
```

---

# 46. Token Efficiency Benchmark

For identical questions, record:

```text
input tokens
output tokens
retrieved tokens
context tokens
total tokens
```

Calculate:

```text
Token Reduction =
(RAG tokens - AIKOQL tokens) / RAG tokens
```

Do not claim a target percentage until measured on a representative benchmark.

---

# 47. Latency Benchmark

Measure:

```text
T_total =
T_intent
+ T_memory
+ T_AI­KOQL
+ T_context
+ T_LLM
```

Compare against RAG:

```text
T_total_RAG
```

The goal is not necessarily that AIKOQL is always faster.

The test should establish **where AIKOQL saves latency and where it introduces overhead**.

---

# 48. Cost Benchmark

Measure:

```text
LLM calls
tokens
retrieval calls
reranker calls
embedding calls
database calls
```

Compare:

```text
LLM-only
RAG
Graph-RAG
AIKOQL
```

---

# 49. Agent Quality Benchmark

Build at least 50 realistic chatbot tasks.

Categories:

```text
fact lookup
personalization
historical question
multi-hop question
policy question
conflict question
procedure question
action request
authorization
unknown question
```

For every task measure:

```text
answer correctness
groundedness
citation/provenance correctness
memory correctness
policy compliance
action correctness
```

---

# 50. Golden Dataset

Create a manually verified dataset:

```text
Question
Expected answer
Expected KOs
Expected relationships
Expected evidence
Expected temporal state
Expected authorization
Expected action
```

All regression tests should execute against this golden dataset.

---

# 51. Critical End-to-End Scenario

This is the most important certification test.

### Initial conversation

```text
User:
I prefer concise responses.

User:
My account is ACME-123.

User:
Remember that I usually deploy on AWS.
```

AIKOQL should produce appropriate durable memories.

### Later conversation

```text
User:
What do you know about my deployment setup?
```

Expected:

```text
AWS
```

with correct provenance and scope.

### Later knowledge update

Authoritative organization source says:

```text
ACME-123 must now deploy on Azure.
```

User asks:

```text
Where should I deploy now?
```

Expected:

```text
Azure
```

with explanation that the organization policy supersedes the previous preference/knowledge where the policy requires it.

### Action request

```text
Deploy it.
```

Expected:

```text
resolve Program-as-KO
→ check identity
→ check permissions
→ check policy
→ check preconditions
→ request approval if required
→ execute
→ verify postconditions
→ record episode
```

This single scenario tests:

```text
memory
semantic knowledge
temporal knowledge
provenance
authority
constraints
programs
authorization
agentic execution
episodic learning
```

---

# 52. Ultimate Comparative Experiment

For the same chatbot application, run:

```text
Scenario A:
LLM only

Scenario B:
LLM + RAG

Scenario C:
LLM + Graph-RAG

Scenario D:
LLM + AIKOQL
```

Use the same:

```text
LLM
prompt policy
questions
knowledge corpus
hardware
network conditions
```

Measure:

| Metric | LLM | RAG | Graph-RAG | AIKOQL |
|---|---:|---:|---:|---:|
| Accuracy | 0.000 | 0.625 | 0.812 | 0.938 |
| Groundedness | 0.000 | 0.000 | 0.000 | 1.000 |
| Hallucination rate | 0.0 | 0.0 | 0.0 | 0.0 |
| Provenance accuracy | 0.000 | 0.500 | 0.500 | 1.000 |
| Temporal accuracy | 0.000 | 0.000 | 0.000 | 0.000 |
| Multi-hop accuracy | 0.000 | 0.375 | 0.750 | 0.875 |
| Memory continuity | — | — | — | — |
| Input tokens | 10.1 | 77.1 | 82.1 | 179.9 |
| LLM calls | 1 | 1 | 1 | 0 |
| Latency | 41 µs | 570 µs | 284 µs | 3 905 µs |
| Cost | $0.000062 | $0.000072 | $0.000072 | $0.000087 |
| Action safety | — | — | — | — |

This table contains **measured results only**.

Measured 2026-08-22 by `crates/ingestion/tests/comparative_chatbot_bench.rs`
(G11, mechanical run — CI-reproducible, no live model): the four treatments
receive the same corpus (15-doc Track-B corpus + one COMP-005 provenance
question, 8 questions × 2 evidence units), the same 300-token budget, and
the same token-containment judge, over the payload the LLM would receive.
The mechanical slice measures the retrieval/context tier; the rows that
need a generated answer are honest about their proxy:

- **Accuracy / Multi-hop accuracy / Provenance accuracy** — evidence units
  delivered by the payload; AIKOQL's only miss is the depth-2 leaf fact
  (single-round relation boost ceiling). Graph-RAG = lexical rank seed +
  transitive entity-mention chunk expansion.
- **Groundedness** — delivered units whose payload cites its source
  document (AIKOQL renders [`doc`] per entity; raw chunks carry no doc id).
- **Hallucination rate** — 0.0 for all four *by construction*: every
  treatment copies corpus text verbatim, there is no generative step. The
  real-model pass for this row is the `e2e_answer_quality` harness
  (answer_gen seam).
- **Temporal accuracy** — current claim present AND stale claim absent:
  0.0 for every treatment; none suppresses the stale claim (the open
  temporal-policy item).
- **LLM calls** — the SEM-003 proxy: A/B/C need one LLM turn to answer;
  AIKOQL resolves via the deterministic compile path, no call.
- **Memory continuity / Action safety** ("—") — need the live chatbot
  stack; measured by the §51 MCP scenarios (`mcp_real_world.rs`, TP-3b)
  instead of this harness.
- **Input tokens / Latency / Cost** — measured per treatment; cost uses
  the G12 reference rates ($0.15/1M input, $0.60/1M output × 100 assumed
  answer tokens) so the column stays comparable across runs.

---

# 53. Certification Acceptance Criteria

AIKOQL may claim:

## "Chatbot Memory Ready"

only when:

- [ ] Cross-session memory works.
- [ ] Memory survives restart.
- [ ] Memory classification works.
- [ ] Explicit memory works.
- [ ] Ephemeral information is not automatically durable.
- [ ] User preferences work.
- [ ] Memory scopes work.
- [ ] Memory deletion/retention works.
- [ ] Provenance works.

---

AIKOQL may claim:

## "Knowledge-Grounded Chatbot Ready"

only when:

- [ ] Structured factual retrieval works.
- [ ] Multi-hop retrieval works.
- [ ] Temporal retrieval works.
- [ ] Contradictions are detectable.
- [ ] Authoritative sources are respected.
- [ ] Unknown knowledge is handled safely.
- [ ] Evidence can be exposed.
- [ ] Knowledge changes without LLM retraining.

---

AIKOQL may claim:

## "Agentic Chatbot Ready"

only when:

- [ ] Procedures are represented.
- [ ] Programs-as-KO work.
- [ ] Preconditions are enforced.
- [ ] Constraints are enforced.
- [ ] Authorization is enforced.
- [ ] Postconditions are verified.
- [ ] Outcomes become episodic memory.
- [ ] Failed actions are recorded.
- [ ] Knowledge evolves from validated experience.

---

# 54. Product-Level Success Criteria

The strongest evidence for AIKOQL is not:

```text
"AIKOQL has a graph."
"AIKOQL has vectors."
"AIKOQL stores KOs."
```

The strongest evidence is:

```text
AIKOQL-backed chatbot
        ↓
uses less raw context
        ↓
requires fewer unnecessary LLM reasoning steps
        ↓
maintains persistent memory
        ↓
answers temporal questions correctly
        ↓
handles contradictions
        ↓
provides provenance
        ↓
respects constraints
        ↓
executes known procedures safely
        ↓
learns from validated outcomes
```

---

# 55. Final QA Position

The core hypothesis to certify is:

> **AIKOQL does not merely retrieve information for an LLM. It provides a persistent, structured, governed representation of what the chatbot knows, remembers, can do, and is allowed to do.**

That creates a clean separation:

```text
                    CHATBOT
                       │
             ┌─────────┴─────────┐
             ↓                   ↓
          AIKOQL                 LLM
             │                   │
      What is known?       How should it be said?
      What happened?       How should it reason?
      What is current?     How should it explain?
      What is allowed?     How should it converse?
      What can be done?
      What evidence exists?
             │                   │
             └─────────┬─────────┘
                       ↓
                 Final Response
                    / Action
```

The certification goal is therefore not to prove that AIKOQL can replace an LLM.

It is to prove that:

> **The LLM no longer needs to be the system of record for knowledge or memory.**

That is the chatbot-specific capability that should be demonstrated experimentally.
