# MRFC-0070 — Agent Knowledge Interface & Engineering Knowledge Compiler

**Status:** Proposed  
**Category:** Architecture / Knowledge Infrastructure / Agentic AI  
**Priority:** Strategic / Tier-1  
**Target:** Mnemosyne + AIKOQL  
**Version:** 1.0  
**Depends on:** Knowledge Kernel, Ontology Engine, Provenance, Constraint Engine, Document Intelligence, AIKOQL, Connectors, Program-as-KO  
**Primary consumers:** Coding agents, software-engineering agents, SRE agents, DevOps agents, security agents, architecture agents, data-engineering agents, autonomous engineering agents

---

# 1. Executive Summary

This MRFC defines the **Agent Knowledge Interface (AKI)** and **Engineering Knowledge Compiler (EKC)** for Mnemosyne.

The objective is to provide a universal, agent-independent semantic knowledge layer beneath modern engineering agents.

The system must not model Codex, Claude Code, Cline, Cursor, or any other agent as a special case. Instead, it implements the universal engineering knowledge model defined by:

```text
Entity
Artifact
Relationship
Claim
Rule
Requirement
Decision
Task
Evidence
Event
```

with cross-cutting metadata:

```text
Scope
Authority
Confidence
Provenance
Temporal Validity
Version
Status
```

Mnemosyne becomes capable of transforming engineering artifacts into structured Knowledge Objects (KOs), resolving their relationships, tracking provenance and temporal state, detecting conflicts and stale knowledge, and compiling task-specific knowledge into an optimized context package for any engineering agent.

The resulting architecture is:

```text
Engineering System
       |
       +-- Code
       +-- Markdown
       +-- ADR / RFC
       +-- Tests
       +-- Configuration
       +-- Issues
       +-- PRs
       +-- Commits
       +-- CI/CD
       +-- Schemas
       +-- Telemetry
       |
       v
Engineering Knowledge Compiler
       |
       v
Knowledge IR
       |
       v
Typed Knowledge Objects
       |
       +-- Ontology
       +-- Provenance
       +-- Authority
       +-- Temporal Model
       +-- Constraints
       |
       v
Engineering Knowledge Graph
       |
       v
AIKOQL / Agent Knowledge Interface
       |
       v
Context Compiler
       |
       v
Any Engineering Agent
       |
       v
Action / Change
       |
       v
Validation
       |
       v
Knowledge Reconciliation
```

The strategic objective is to make Mnemosyne a **knowledge infrastructure layer for autonomous engineering**, rather than another coding agent.

---

# 2. Motivation

Modern coding and engineering agents have converged on several forms of repository context:

```text
AGENTS.md
CLAUDE.md
.clinerules/
skills
MCP servers
repository documentation
architecture documents
ADRs
task memory
tool outputs
```

These mechanisms are useful but primarily represent knowledge as documents, instructions, retrieved text, or tool responses.

This creates recurring problems:

1. Knowledge is fragmented.
2. Knowledge is duplicated.
3. Instructions and facts are mixed.
4. Current and historical knowledge are difficult to distinguish.
5. Documentation can become stale relative to code.
6. Conflicting sources are difficult to resolve.
7. Agents receive too much irrelevant context.
8. Context selection is mostly retrieval-oriented rather than knowledge-oriented.
9. Agent-generated knowledge is difficult to validate and persist.
10. There is no universal semantic contract between an engineering knowledge system and arbitrary agents.

The proposed system addresses these problems without requiring Mnemosyne to become an agent runtime.

---

# 3. Goals

## 3.1 Primary Goals

The system SHALL:

1. Implement the universal engineering knowledge model.
2. Ingest engineering artifacts from heterogeneous sources.
3. Convert artifacts into typed Knowledge Objects.
4. Preserve provenance for derived knowledge.
5. Resolve entities across artifacts and connectors.
6. Model relationships between engineering entities.
7. Support temporal and versioned knowledge.
8. Track authority and confidence independently.
9. Support scoped knowledge.
10. Detect knowledge conflicts.
11. Detect stale documentation and stale claims.
12. Compile task-specific context for agents.
13. Optimize context under token and latency budgets.
14. Expose knowledge through AIKOQL.
15. Expose knowledge through an agent integration interface.
16. Support agent-generated knowledge proposals.
17. Validate proposed knowledge before promotion.
18. Reconcile knowledge after engineering changes.
19. Support explainability and evidence tracing.
20. Remain independent of any specific agent, LLM, IDE, or tool protocol.

---

# 4. Non-Goals

This MRFC does NOT define:

- a replacement for Codex, Claude Code, Cline, Cursor, or other agents
- a new LLM
- a new foundation model
- a mandatory agent orchestration framework
- a mandatory IDE
- a replacement for MCP
- a replacement for Git
- a replacement for CI/CD
- a replacement for source control
- a requirement that Markdown disappear
- a requirement that every artifact become a KO
- a universal authorization system for every external agent

Mnemosyne provides the knowledge layer.

---

# 5. Architectural Principles

## 5.1 Agent Independence

The Knowledge Model SHALL NOT depend on a specific agent.

```text
Universal Knowledge Model
        |
        +-- Codex
        +-- Claude Code
        +-- Cline
        +-- Cursor
        +-- Custom Agent
        +-- SRE Agent
        +-- Security Agent
```

## 5.2 Knowledge Before Retrieval

Vector retrieval is a retrieval mechanism, not the knowledge architecture.

The system SHALL support:

```text
lexical retrieval
vector retrieval
graph traversal
ontology retrieval
structural retrieval
code-symbol retrieval
dependency retrieval
temporal retrieval
authority filtering
provenance filtering
```

## 5.3 Evidence-Backed Knowledge

Important claims SHALL be traceable to evidence.

## 5.4 Current Truth and Historical Truth

The system SHALL preserve history while exposing the current valid state.

## 5.5 Knowledge Does Not Imply Authorization

Knowing how to perform an operation does not grant permission to perform it.

## 5.6 Human-Readable and Machine-Readable

Knowledge SHALL support:

```text
Markdown
AIKOQL
API
SDK
Agent Context
Studio UI
```

## 5.7 Progressive Context

Agents SHALL receive the minimum sufficient context and be able to expand it when necessary.

## 5.8 Safe Agent-Generated Knowledge

Agent-generated knowledge SHALL be distinguishable from authoritative knowledge.

---

# 6. Universal Conceptual Model

The canonical primitives are:

```text
ENTITY
ARTIFACT
RELATIONSHIP
CLAIM
RULE
REQUIREMENT
DECISION
TASK
EVIDENCE
EVENT
```

Specialized types include:

```text
CONSTRAINT
INSTRUCTION
PROPOSAL
OBSERVATION
INCIDENT
TEST
DEPENDENCY
POLICY
MEMORY
CONFLICT
```

These are represented as typed Knowledge Objects.

---

# 7. Knowledge Object Model

## 7.1 Canonical Envelope

```yaml
KnowledgeObject:
  id:
  type:

  subject:
  predicate:
  object:

  properties:

  scope:

  authority:
  confidence:

  provenance:

  temporal:
    valid_from:
    valid_to:
    observed_at:

  status:

  relationships:

  version:

  created_at:
  updated_at:
```

## 7.2 Required Fields

Every KO SHALL have:

```text
id
type
status
version
created_at
updated_at
```

Claims and derived KOs SHALL additionally have provenance.

## 7.3 Optional Fields

Depending on type:

```text
subject
predicate
object
scope
authority
confidence
temporal
relationships
properties
```

---

# 8. Knowledge Types

## 8.1 Entity Types

Examples:

```text
Project
Repository
Service
Component
Module
Class
Function
API
Database
Schema
Table
Agent
Environment
Infrastructure
Dependency
Package
Team
```

## 8.2 Artifact Types

```text
SourceFile
Markdown
RFC
ADR
PRD
Test
Configuration
Dockerfile
Terraform
OpenAPI
Migration
Commit
PullRequest
Release
Log
Trace
CIWorkflow
```

## 8.3 Semantic Types

```text
Fact
Rule
Requirement
Decision
Task
Constraint
Instruction
Proposal
Observation
Event
Evidence
Conflict
```

---

# 9. Knowledge Lifecycle

```text
DISCOVERED
    |
    v
EXTRACTED
    |
    v
PROPOSED
    |
    v
VALIDATED
    |
    v
ACCEPTED
    |
    v
ACTIVE
    |
    v
UPDATED
    |
    v
SUPERSEDED
    |
    v
ARCHIVED
```

Agent-generated knowledge SHOULD enter at `PROPOSED`.

Automatically accepting agent-generated knowledge is permitted only through explicit policy.

---

# 10. Authority Model

Authority and confidence are independent.

Possible authority levels:

```text
HUMAN_APPROVED
ORGANIZATION_POLICY
ARCHITECTURE_DECISION
SOURCE_CODE
TEST_VERIFIED
CI_VERIFIED
DEPLOYMENT_OBSERVED
DOCUMENTATION
AGENT_DERIVED
LLM_INFERRED
UNTRUSTED_EXTERNAL
```

The exact precedence SHALL be policy-configurable.

---

# 11. Provenance Model

Every derived KO SHALL retain:

```text
source artifact
source location
source version
extraction method
extractor version
model/provider where applicable
timestamp
```

Example:

```yaml
provenance:
  source:
    artifact: docs/architecture.md
    location: "Vector Engine"
    revision: abc123

  extraction:
    method: semantic-analysis
    extractor_version: "1.0"
    model: provider/model-version
```

---

# 12. Temporal Model

The system SHALL support:

```text
valid_from
valid_to
observed_at
created_at
updated_at
superseded_at
```

A query SHALL be able to request:

```text
CURRENT
AS_OF
BETWEEN
HISTORICAL
```

Example:

```text
"What vector index is currently used?"
```

versus:

```text
"What vector index was used before migration?"
```

---

# 13. Scope Model

Supported scopes:

```text
GLOBAL
ORGANIZATION
PROJECT
REPOSITORY
BRANCH
DIRECTORY
COMPONENT
TASK
SESSION
AGENT
USER
ENVIRONMENT
```

Scope resolution SHALL be deterministic.

Nested scope may override broader scope according to explicit policy.

---

# 14. Knowledge Conflict Model

A conflict exists when two or more active claims cannot simultaneously be true under the same scope and temporal context.

Example:

```text
Code:
HNSW

README:
FAISS

ADR:
HNSW
```

The system SHALL create or expose a conflict representation:

```text
ConflictKO
    |
    +-- Claim A
    +-- Claim B
    +-- Evidence
    +-- Authority
    +-- Temporal context
    +-- Resolution state
```

Resolution states:

```text
UNRESOLVED
CURRENT_TRUTH
STALE
SUPERSEDED
CONFLICTED
```

The system SHALL NOT silently discard conflicting evidence.

---

# 15. Stale Knowledge Detection

Mnemosyne SHALL be able to identify likely stale documentation.

Example:

```text
Source code:
HnswIndex

Architecture document:
FAISS
```

Result:

```text
STALE_KNOWLEDGE

Current evidence:
source code

Stale artifact:
architecture.md
```

Staleness detection may use:

```text
version
commit history
timestamps
symbol references
dependency relationships
tests
explicit supersession
authority
```

---

# 16. Engineering Knowledge Compiler — HLD

## 16.1 Overview

The compiler converts heterogeneous engineering artifacts into structured knowledge.

```text
                   ARTIFACTS
                       |
       +---------------+----------------+
       |               |                |
      CODE          DOCUMENTS       OPERATIONS
       |               |                |
       +---------------+----------------+
                       |
                 Ingestion Layer
                       |
                  Parser Layer
                       |
                Knowledge IR
                       |
              Semantic Extraction
                       |
                Entity Resolution
                       |
               Relationship Builder
                       |
              Provenance Attachment
                       |
              Validation / Constraints
                       |
                Knowledge Objects
                       |
                 Ontology Engine
                       |
              Engineering KG
```

---

# 17. EKC Components

```text
ekc/
├── ingestion/
├── parsers/
├── ast/
├── semantic/
├── extraction/
├── entity-resolution/
├── relationship/
├── provenance/
├── temporal/
├── authority/
├── validation/
├── reconciliation/
├── compiler/
└── adapters/
```

---

# 18. Ingestion Layer

Supported initial inputs:

```text
Markdown
Text
Rust
Java
Python
TypeScript
JavaScript
SQL
YAML
JSON
TOML
Dockerfile
OpenAPI
protobuf
Git
CI configuration
```

Future:

```text
issues
PRs
telemetry
tickets
cloud infrastructure
observability platforms
```

The ingestion layer SHALL normalize source metadata.

---

# 19. Parser Layer

Each source type SHALL have a parser.

Example:

```text
Markdown Parser
    ↓
Document AST

Rust Parser
    ↓
Code AST

OpenAPI Parser
    ↓
API AST

Git Adapter
    ↓
Repository Event Model
```

Parsers SHALL preserve source location information.

---

# 20. Knowledge IR

Knowledge IR is the intermediate representation between parsing and KO generation.

Example:

```yaml
entity:
  type: Component
  name: ConstraintEngine

claims:
  - predicate: IMPLEMENTED_IN
    object: crates/kernel/constraints/

relationships:
  - type: DEPENDS_ON
    target: TransactionEngine
```

Knowledge IR SHALL be deterministic where extraction is deterministic.

---

# 21. Semantic Extraction

Semantic extraction identifies:

```text
entities
claims
rules
requirements
decisions
tasks
constraints
relationships
events
```

Methods may include:

```text
AST analysis
pattern matching
ontology mapping
LLM-assisted extraction
schema inference
static analysis
Git analysis
test analysis
```

LLM-derived results SHALL retain extraction provenance.

---

# 22. Entity Resolution

The same entity may appear in:

```text
README
source code
ADR
RFC
test
Git commit
issue
```

The Entity Resolution subsystem SHALL unify references.

Example:

```text
"Constraint Engine"
"constraint-engine"
"ConstraintEngine"
"crates/kernel/constraints"
```

may resolve to:

```text
component:constraint-engine
```

Resolution confidence SHALL be retained.

---

# 23. Relationship Builder

Relationships SHALL be extracted from:

```text
imports
calls
documentation references
Git history
explicit ontology
configuration
dependency manifests
tests
architecture documents
```

Examples:

```text
Component DEPENDS_ON Component
Requirement IMPLEMENTED_BY Component
Component TESTED_BY Test
Decision GOVERNS Component
Artifact DESCRIBES Entity
Commit MODIFIES Artifact
```

---

# 24. Provenance Attachment

Every extracted relationship or claim SHALL retain provenance.

Example:

```text
ConstraintEngine
    DEPENDS_ON
TransactionEngine

Evidence:
engine.rs
line range
commit
extractor
```

---

# 25. Ontology Integration

The compiler SHALL use the Ontology Engine to map extracted concepts into the universal model.

Example:

```text
"database constraint"
        ↓
Constraint
        ↓
type = DATABASE_CONSTRAINT
```

Ontology discovery from connectors remains supported.

---

# 26. Constraint Integration

Knowledge validation SHALL use the Constraint Engine.

Examples:

```text
KO identity uniqueness
required properties
relationship validity
scope validity
temporal consistency
referential integrity
type correctness
```

Knowledge Objects SHALL NOT bypass the Knowledge Kernel's transactional guarantees.

---

# 27. Knowledge Storage HLD

```text
                Agent Knowledge Interface
                         |
                       AIKOQL
                         |
                  Query / Context API
                         |
                Knowledge Query Layer
                         |
       +-----------------+------------------+
       |                 |                  |
   Graph Index       Vector Index       Lexical Index
       |                 |                  |
       +-----------------+------------------+
                         |
                  Knowledge Kernel
                         |
       +-----------------+------------------+
       |                 |                  |
   Canonical KO      Provenance         Versioning
      Store             Store              |
       |                 |                  |
       +-----------------+------------------+
                         |
                  Storage Engine
```

The canonical source of truth remains the Knowledge Kernel.

Indexes are derived structures.

---

# 28. Agent Knowledge Interface — HLD

The Agent Knowledge Interface provides an agent-neutral access layer.

```text
                External Agent
                       |
             +---------+---------+
             |                   |
           AIKOQL               API
             |                   |
             +---------+---------+
                       |
                 Agent Gateway
                       |
        +--------------+--------------+
        |              |              |
      Auth         Policy         Session
        |              |              |
        +--------------+--------------+
                       |
              Context Compiler
                       |
              Knowledge Query Layer
                       |
                  Mnemosyne
```

MCP may be implemented as one transport/adapter.

MCP is not the semantic model.

---

# 29. Agent Gateway

Responsibilities:

```text
authentication
agent identity
authorization
request validation
rate limiting
tenant isolation
session handling
audit
protocol adaptation
```

It SHALL NOT contain agent-specific knowledge semantics.

---

# 30. Agent Identity

Requests SHOULD contain:

```yaml
agent:
  id:
  type:
  version:
  session_id:
  task_id:
```

Example:

```yaml
agent:
  id: agent:codex
  type: coding-agent
  version: x.y
```

The system SHALL not assume that an agent ID identifies a particular vendor.

---

# 31. Context Compiler — HLD

The Context Compiler is the central agent-facing intelligence layer.

```text
Task
 |
 v
Intent Understanding
 |
 v
Scope Resolution
 |
 v
Knowledge Candidate Retrieval
 |
 +-- Graph
 +-- Vector
 +-- Lexical
 +-- Structural
 +-- Temporal
 +-- Ontology
 |
 v
Authority Filtering
 |
 v
Conflict Detection
 |
 v
Dependency Expansion
 |
 v
Reranking
 |
 v
Context Compression
 |
 v
Token Budget Enforcement
 |
 v
Context Package
```

---

# 32. Context Compiler Objectives

It SHALL optimize for:

```text
relevance
authority
freshness
evidence quality
scope match
temporal match
dependency relevance
task utility
```

subject to:

```text
token budget
latency budget
security policy
```

---

# 33. Context Package

A context package SHOULD contain:

```yaml
context:
  task:
  summary:

  knowledge:
    - ko:
      relevance:
      authority:
      confidence:

  relationships:
  constraints:
  decisions:
  requirements:
  instructions:

  evidence:
  conflicts:
  warnings:

  source_references:
```

---

# 34. Progressive Context

The initial response SHALL be compact.

Agents can request:

```text
expand KO
expand evidence
expand source
expand relationship
expand history
```

This prevents unnecessary context consumption.

---

# 35. AIKOQL Integration

AIKOQL SHALL expose universal knowledge operations.

Examples:

```aikoql
EXPLAIN COMPONENT "ConstraintEngine"
```

```aikoql
FIND REQUIREMENTS
WHERE status = "UNIMPLEMENTED"
```

```aikoql
TRACE REQUIREMENT "REQ-042"
TO CODE
```

```aikoql
FIND CONFLICTS
WHERE component = "VectorEngine"
```

```aikoql
FIND STALE DOCUMENTATION
```

```aikoql
GET CONTEXT FOR TASK
"Implement deferred referential integrity"
```

The syntax is illustrative; the parser specification remains governed by the AIKOQL language design.

---

# 36. Universal Agent Operations

The interface SHALL support these semantic operations:

```text
READ
QUERY
TRAVERSE
EXPLAIN
COMPARE
VALIDATE
PROPOSE
CREATE
UPDATE
SUPERSEDE
LINK
UNLINK
ARCHIVE
```

Not every agent receives every operation.

Authorization and policy determine access.

---

# 37. Agent Knowledge Creation

An agent MAY propose new knowledge.

Example:

```yaml
proposal:
  type: Decision
  subject: VectorEngine

  claim:
    predicate: USES_INDEX
    object: HNSW

  reason:
    "Observed in implementation."

  evidence:
    - source_code
```

Default lifecycle:

```text
AGENT
 ↓
PROPOSE
 ↓
VALIDATE
 ↓
ACCEPT / REJECT
```

---

# 38. Automatic Knowledge Promotion

Automatic promotion MAY occur if policy permits.

Example:

```text
source = source code
authority = CODE
confidence >= threshold
constraint validation = PASS
```

Policy SHALL define promotion rules.

---

# 39. Post-Change Reconciliation

After an engineering change:

```text
Git Diff
   ↓
Affected Artifacts
   ↓
Affected Entities
   ↓
Affected Relationships
   ↓
Affected Claims
   ↓
Affected Requirements
   ↓
Affected Constraints
   ↓
Affected Documentation
```

The system SHALL identify knowledge potentially requiring reconciliation.

---

# 40. Change Impact Analysis

Example:

```text
Agent modifies:
ConstraintEngine

Impact:
  TransactionEngine
  ConstraintEngine tests
  MRFC-0060
  architecture.md
  API documentation
```

The agent SHALL be able to request:

```text
"what knowledge does this change affect?"
```

---

# 41. Explainability

Every significant response SHOULD support:

```text
WHY?
SOURCE?
WHEN?
WHO?
CONFIDENCE?
WHAT ELSE SUPPORTS IT?
IS THERE CONFLICT?
```

Example:

```text
Why does ConstraintEngine use MVCC?

Evidence:
1. source code
2. ADR-012
3. integration test

Authority:
HIGH

Current:
YES
```

---

# 42. Security Model

The Agent Knowledge Interface SHALL enforce:

```text
authentication
authorization
tenant isolation
source access control
secret filtering
PII filtering
document ACLs
environment boundaries
audit logging
rate limiting
```

Knowledge retrieval SHALL respect source-level access permissions.

---

# 43. Prompt Injection Defense

External documents are not inherently trusted.

The system SHALL distinguish:

```text
FACT
INSTRUCTION
CONSTRAINT
UNTRUSTED_CONTENT
```

A document containing:

```text
"Ignore all previous instructions..."
```

must not automatically become an authoritative agent instruction.

External content SHALL carry provenance and trust classification.

---

# 44. Secret Handling

Secrets SHALL NOT become normal Knowledge Objects.

The extraction layer SHALL detect and redact or block:

```text
API keys
private keys
passwords
tokens
credentials
session secrets
```

unless explicit policy permits secure secret references.

---

# 45. PII Handling

PII policy SHALL apply before knowledge is exposed to an agent.

Knowledge may contain:

```text
redacted
masked
tokenized
access-controlled
```

representations.

---

# 46. Multi-Tenant Isolation

Knowledge SHALL be tenant-scoped where applicable.

A query from tenant A must never return tenant B knowledge.

This applies to:

```text
KOs
relationships
embeddings
indexes
provenance
context caches
audit logs
```

---

# 47. Context Cache

The Context Compiler MAY cache:

```text
task context
component summaries
ontology mappings
frequently requested relationships
```

Cache keys SHALL include security-relevant scope.

Cached context must not cross authorization boundaries.

---

# 48. Failure Handling

The system SHALL distinguish:

```text
NO_KNOWLEDGE
INSUFFICIENT_KNOWLEDGE
CONFLICTING_KNOWLEDGE
STALE_KNOWLEDGE
UNAUTHORIZED_KNOWLEDGE
CONTEXT_BUDGET_EXCEEDED
QUERY_INVALID
KNOWLEDGE_VALIDATION_FAILED
```

It SHALL not fabricate missing knowledge.

---

# 49. Low-Level Design

## 49.1 Rust Crate Structure

Proposed:

```text
crates/
├── kernel/
│   ├── knowledge/
│   ├── storage/
│   ├── transaction/
│   ├── security/
│   └── lifecycle/
│
├── engines/
│   ├── query/
│   ├── planner/
│   ├── optimizer/
│   ├── graph/
│   ├── vector/
│   ├── semantic/
│   ├── reasoning/
│   ├── scheduler/
│   ├── indexing/
│   └── agent_knowledge/
│
├── runtime/
├── cluster/
├── services/
│   ├── api/
│   ├── auth/
│   ├── telemetry/
│   └── agent_gateway/
│
├── sdk/
│   ├── rust/
│   ├── python/
│   ├── java/
│   ├── go/
│   └── typescript/
│
├── plugins/
├── benchmarks/
├── integration-tests/
└── fuzz/
```

---

# 50. Agent Knowledge Engine Modules

```text
agent_knowledge/
├── model/
├── ingestion/
├── compiler/
├── context/
├── retrieval/
├── ranking/
├── conflict/
├── reconciliation/
├── provenance/
├── policy/
├── authorization/
├── projection/
├── protocol/
└── cache/
```

---

# 51. Core Rust Domain Types

Illustrative:

```rust
pub struct KnowledgeObjectId(pub String);

pub struct KnowledgeObject {
    pub id: KnowledgeObjectId,
    pub kind: KnowledgeType,
    pub subject: Option<EntityRef>,
    pub predicate: Option<Predicate>,
    pub object: Option<Value>,
    pub properties: PropertyMap,
    pub scope: Scope,
    pub authority: Authority,
    pub confidence: Confidence,
    pub provenance: Provenance,
    pub temporal: TemporalValidity,
    pub status: KnowledgeStatus,
    pub version: Version,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
```

---

# 52. Knowledge Types

```rust
pub enum KnowledgeType {
    Entity,
    Artifact,
    Claim,
    Rule,
    Requirement,
    Decision,
    Task,
    Evidence,
    Event,
    Constraint,
    Instruction,
    Proposal,
    Observation,
    Test,
    Conflict,
}
```

The implementation may extend this enumeration as the ontology evolves.

---

# 53. Authority

```rust
pub enum Authority {
    HumanApproved,
    OrganizationPolicy,
    ArchitectureDecision,
    SourceCode,
    TestVerified,
    CiVerified,
    DeploymentObserved,
    Documentation,
    AgentDerived,
    LlmInferred,
    UntrustedExternal,
}
```

Authority ranking must be policy-driven rather than hard-coded into business logic.

---

# 54. Confidence

```rust
pub struct Confidence {
    pub score: f32,
    pub method: ConfidenceMethod,
}
```

Where:

```rust
pub enum ConfidenceMethod {
    Explicit,
    DeterministicExtraction,
    StaticAnalysis,
    TestEvidence,
    ModelInference,
    Aggregated,
}
```

---

# 55. Provenance

```rust
pub struct Provenance {
    pub source: SourceRef,
    pub location: Option<SourceLocation>,
    pub revision: Option<String>,
    pub extraction_method: ExtractionMethod,
    pub extractor_version: String,
    pub observed_at: Timestamp,
}
```

---

# 56. Scope

```rust
pub enum Scope {
    Global,
    Organization(EntityId),
    Project(EntityId),
    Repository(EntityId),
    Branch(String),
    Directory(String),
    Component(EntityId),
    Task(EntityId),
    Session(EntityId),
    Agent(EntityId),
    User(EntityId),
    Environment(EntityId),
}
```

---

# 57. Context Request

```rust
pub struct ContextRequest {
    pub task: TaskRef,
    pub required_types: Vec<KnowledgeType>,
    pub optional_types: Vec<KnowledgeType>,
    pub excluded_scopes: Vec<Scope>,
    pub token_budget: u32,
    pub latency_budget_ms: u32,
    pub depth: u32,
}
```

---

# 58. Context Package

```rust
pub struct ContextPackage {
    pub task: TaskRef,
    pub knowledge: Vec<ContextItem>,
    pub relationships: Vec<Relationship>,
    pub evidence: Vec<EvidenceRef>,
    pub conflicts: Vec<ConflictRef>,
    pub warnings: Vec<ContextWarning>,
    pub token_count: u32,
    pub generated_at: Timestamp,
}
```

---

# 59. Retrieval Pipeline

```text
ContextRequest
      |
      v
Query Planner
      |
      +--> Lexical Search
      +--> Vector Search
      +--> Graph Search
      +--> Ontology Search
      +--> Symbol Search
      +--> Temporal Search
      |
      v
Candidate Fusion
      |
      v
Authorization
      |
      v
Authority Filtering
      |
      v
Conflict Detection
      |
      v
Relationship Expansion
      |
      v
Reranker
      |
      v
Context Compressor
      |
      v
Budget Enforcement
      |
      v
Context Package
```

---

# 60. Candidate Scoring

A candidate may be scored conceptually as:

```text
score =
    relevance
  * authority_weight
  * freshness_weight
  * evidence_weight
  * scope_weight
  * temporal_weight
  * task_utility
```

The implementation SHALL allow these factors to be configured and benchmarked.

---

# 61. Context Compression

Compression levels:

```text
SUMMARY
STRUCTURED_FACT
RELATIONSHIP
EVIDENCE
SOURCE_FRAGMENT
FULL_ARTIFACT
```

The compiler SHALL prefer the lowest-cost representation that satisfies the context requirement.

---

# 62. AIKOQL Execution

AIKOQL requests SHALL be compiled:

```text
AIKOQL
  ↓
Lexer
  ↓
Parser
  ↓
AST
  ↓
Semantic Analyzer
  ↓
Knowledge Query Plan
  ↓
Optimizer
  ↓
Knowledge Engine
  ↓
Indexes / Graph / Storage
```

The existing AIKOQL architecture remains authoritative for language parsing.

This MRFC defines the semantic operations exposed to agents.

---

# 63. Agent Gateway API

Illustrative REST API:

```text
POST /v1/agent/context
POST /v1/agent/query
POST /v1/agent/explain
POST /v1/agent/trace
POST /v1/agent/validate
POST /v1/agent/proposals
POST /v1/agent/reconcile
GET  /v1/agent/knowledge/{id}
```

Transport may also be exposed through:

```text
MCP
gRPC
SDK
AIKOQL
```

---

# 64. Example: Coding Agent Context Request

Task:

```text
Implement deferred referential integrity.
```

The compiler should identify:

```text
Constraint Engine
Transaction Engine
Knowledge Kernel
Referential Integrity Constraint
Deferred Constraint Requirement
Relevant ADRs
MRFC-0060
Affected tests
Affected connectors
Recent changes
Known conflicts
```

The output should be a concise context package, not an indiscriminate document dump.

---

# 65. Example: Explain

Request:

```text
EXPLAIN COMPONENT "ConstraintEngine"
```

Expected conceptual output:

```text
Purpose
Architecture
Dependencies
Constraints
Requirements
Decisions
Implementation
Tests
Recent changes
Known issues
Documentation
Evidence
Conflicts
```

---

# 66. Example: Trace

Request:

```text
TRACE REQUIREMENT "REQ-042" TO CODE
```

Expected graph:

```text
Requirement
   |
   +-- Decision
   |
   +-- Component
          |
          +-- Module
                |
                +-- Function
                       |
                       +-- Test
```

Every edge should be evidence-backed where possible.

---

# 67. Example: Stale Documentation

Input:

```text
Code:
HnswIndex

ADR:
HNSW selected

README:
FAISS
```

Output:

```text
Conflict detected.

Current likely truth:
HNSW

Stale candidate:
README

Evidence:
source code
ADR

Recommended action:
review README
```

The system SHALL not silently rewrite the document without authorization.

---

# 68. Example: Agent-Generated Knowledge

Agent discovers:

```text
ConstraintEngine requires transaction snapshot.
```

It submits:

```text
ProposalKO
```

Mnemosyne checks:

```text
source code
tests
existing constraints
architecture decisions
```

If validated:

```text
Proposal
   ↓
Validated
   ↓
Accepted Claim
```

---

# 69. Example: Post-Change Reconciliation

Agent changes:

```text
crates/kernel/constraints/
```

Mnemosyne computes:

```text
Affected:
ConstraintEngine
TransactionEngine
MRFC-0060
architecture.md
constraint tests
```

The system returns:

```text
Knowledge Impact Report
```

containing:

```text
updated
potentially stale
conflicting
missing documentation
affected constraints
affected tests
```

---

# 70. Markdown Projection

The system SHALL support generating human-readable projections from KOs.

Example:

```text
Knowledge Object
      ↓
Markdown Renderer
      ↓
docs/components/constraint-engine.md
```

The generated Markdown SHALL retain source references and version metadata where configured.

---

# 71. Markdown Ingestion

Markdown SHALL be treated as:

```text
Artifact
```

and semantic content extracted into:

```text
Claim
Rule
Requirement
Decision
Constraint
Instruction
Proposal
```

Markdown content SHALL NOT automatically become agent instructions.

---

# 72. Agent Memory

Agent memory can be modeled using the same universal model.

Scopes may include:

```text
SESSION
TASK
PROJECT
REPOSITORY
AGENT
USER
```

Examples:

```text
Task memory:
"Neo4j connector currently lacks deferred constraints."

Project knowledge:
"All connectors expose a normalized schema model."

Repository instruction:
"Run cargo test --workspace before merge."
```

No separate memory ontology is required.

---

# 73. Agent Skills

A skill may be represented as:

```text
AgentSkillKO
```

with:

```text
name
capabilities
required_knowledge
required_tools
instructions
validation
scope
```

Programs-as-KO can provide executable implementations of skills.

This connects the existing Programs-as-KO design with the Agent Knowledge Interface.

---

# 74. Programs-as-KO Integration

A Program-as-KO may describe:

```text
inputs
outputs
preconditions
postconditions
dependencies
permissions
implementation
tests
```

The Context Compiler can retrieve Programs-as-KO when a task requires executable behavior.

Example:

```text
Task:
"Validate schema migration."

Context:
Constraint rules
Migration Program KO
Schema KO
Relevant tests
```

---

# 75. Ontology Integration

Ontology provides semantic interpretation.

Example:

```text
"database constraint"
"schema invariant"
"integrity rule"
```

may resolve to:

```text
Constraint
```

The ontology also supports:

```text
synonym resolution
entity classification
relationship typing
semantic expansion
connector mapping
```

---

# 76. Connector Integration

Existing connectors may contribute knowledge:

```text
PostgreSQL
PGVector
Neo4j
MongoDB
Document sources
```

Example:

```text
PostgreSQL table
    ↓
Schema KO
    ↓
Column KO
    ↓
Constraint KO
```

Neo4j:

```text
Node label
    ↓
Entity KO

Relationship type
    ↓
Relationship KO
```

MongoDB:

```text
Collection
    ↓
Entity KO

Observed field
    ↓
Property Claim
```

All connector-derived knowledge SHALL preserve connector provenance.

---

# 77. Document Intelligence Integration

Document Intelligence can produce:

```text
Document
Page
Section
Table
Image
OCR text
Chunk
Entity
Claim
Relationship
```

These become evidence and knowledge inputs.

Document chunks remain useful retrieval units but are not themselves the complete semantic model.

---

# 78. Chunk Integration

Chunks SHALL remain available for:

```text
source retrieval
vector search
precise evidence
context expansion
citation
```

But:

```text
Chunk ≠ Knowledge Object
```

A KO may reference one or more chunks as evidence.

---

# 79. Versioning

Knowledge SHALL support immutable versions.

Example:

```text
KO:v1
KO:v2
KO:v3
```

Updates SHOULD create new versions rather than mutating historical state invisibly.

---

# 80. Transactions

Knowledge mutations SHALL use Knowledge Kernel transactions.

A multi-KO update SHOULD be atomic.

Example:

```text
Create Component
Create Dependency
Create Requirement relationship
Create Provenance
```

Either all are committed or none are.

---

# 81. Consistency

The following invariants SHALL be enforced:

```text
KO identity uniqueness
relationship referential integrity
scope validity
version monotonicity
provenance validity
authorized mutation
tenant isolation
temporal consistency
```

The Constraint Engine governs these invariants.

---

# 82. Concurrency

Concurrent agents may update the same knowledge.

The system SHALL support:

```text
optimistic concurrency
version checks
conflict detection
merge/retry
```

Example:

```text
Agent A updates ComponentKO v5 → v6

Agent B attempts update based on v5

Result:
VERSION_CONFLICT
```

The agent can retrieve current state and retry.

---

# 83. Idempotency

Agent operations SHOULD support idempotency keys.

Example:

```text
proposal_id
task_id
agent_id
operation_id
```

Repeated submissions must not create duplicate semantic objects.

---

# 84. Observability

The system SHALL expose metrics for:

```text
ingestion latency
KO extraction latency
entity-resolution accuracy
relationship extraction accuracy
query latency
context compilation latency
context token count
cache hit rate
conflict rate
stale-knowledge rate
proposal acceptance rate
knowledge validation failures
agent query success rate
```

---

# 85. Audit

The system SHALL audit:

```text
who read
who created
who modified
who proposed
who accepted
who rejected
which source was used
which context was generated
which policy was applied
```

Agent interactions should be traceable without storing sensitive content unnecessarily.

---

# 86. Performance Targets

Initial targets for MVP:

```text
Simple KO lookup:
p95 < 50 ms

Structured knowledge query:
p95 < 500 ms

Context compilation:
p95 < 2 s

Proposal validation:
p95 < 2 s for normal repository scale

Incremental reconciliation:
p95 < 5 s for a normal commit-sized change set
```

These are initial engineering targets and SHALL be benchmarked against representative datasets.

---

# 87. Scalability

The architecture SHALL support:

```text
10^6 KOs
10^7 KOs
10^8+ relationships
large repositories
multi-repository projects
multi-tenant deployments
```

The system SHALL avoid requiring full graph scans for ordinary agent requests.

---

# 88. Reliability

MVP target:

```text
99.9% availability
```

for the Agent Knowledge Interface in a production deployment.

Knowledge durability SHALL follow Knowledge Kernel storage guarantees.

---

# 89. Acceptance Criteria

The following acceptance criteria are mandatory for certification.

## AKI-001 — Universal KO Creation

The system SHALL create typed KOs from supported artifacts.

**Pass:** 95%+ of benchmark fixtures produce structurally valid KOs.

---

## AKI-002 — Source Provenance

Every derived KO SHALL contain provenance.

**Pass:** 100% of derived KOs in certification tests contain resolvable source references.

---

## AKI-003 — Entity Resolution

Equivalent entities across source types SHALL resolve to the same canonical entity where confidence exceeds the configured threshold.

**Pass:** ≥95% precision on the entity-resolution benchmark.

---

## AKI-004 — Relationship Extraction

The system SHALL extract supported relationships from code and documentation.

**Pass:** ≥90% precision and ≥85% recall on the certification fixture.

---

## AKI-005 — Scope Resolution

Nested scopes SHALL resolve deterministically.

**Pass:** 100% of scope precedence tests pass.

---

## AKI-006 — Authority Preservation

Authority SHALL remain separate from confidence.

**Pass:** tests demonstrate that high-confidence low-authority inference does not override higher-authority evidence.

---

## AKI-007 — Temporal Queries

The system SHALL answer current and historical knowledge queries.

**Pass:** 100% of temporal certification cases return the correct version.

---

## AKI-008 — Conflict Detection

Contradictory claims SHALL be detected.

**Pass:** ≥95% detection on deterministic conflict fixtures.

---

## AKI-009 — Stale Documentation Detection

Code/documentation divergence SHALL be detectable.

**Pass:** all deterministic stale-documentation fixtures are detected.

---

## AKI-010 — Knowledge Lifecycle

KOs SHALL transition through the defined lifecycle.

**Pass:** invalid state transitions are rejected.

---

## AKI-011 — Agent Proposal

An agent SHALL be able to create a proposal without automatically creating authoritative knowledge.

**Pass:** 100% of unauthorized promotion attempts are rejected.

---

## AKI-012 — Proposal Validation

A proposal SHALL be validated against configured evidence and constraints.

**Pass:** known valid proposals pass; known invalid proposals fail.

---

## AKI-013 — Context Retrieval

The system SHALL produce task-specific context.

**Pass:** ≥90% recall of manually labeled critical knowledge items.

---

## AKI-014 — Context Precision

The context compiler SHALL avoid unnecessary unrelated knowledge.

**Pass:** ≥80% precision on labeled context sets for MVP.

---

## AKI-015 — Context Budget

The context compiler SHALL respect token limits.

**Pass:** 100% of certification requests remain within the configured budget.

---

## AKI-016 — Progressive Expansion

Agents SHALL be able to expand a KO into evidence and source fragments.

**Pass:** 100% of valid expansion requests resolve correctly.

---

## AKI-017 — Evidence Trace

Every material claim returned to an agent SHALL be traceable.

**Pass:** 100% of certified claim responses contain evidence references.

---

## AKI-018 — Explainability

`EXPLAIN` requests SHALL return semantic and evidence context.

**Pass:** all certification scenarios contain purpose, relationships, evidence, and relevant warnings.

---

## AKI-019 — Requirement-to-Code Traceability

The system SHALL trace requirements to implementation and tests where relationships exist.

**Pass:** ≥90% recall on labeled traceability fixtures.

---

## AKI-020 — Change Impact

A source change SHALL identify affected knowledge.

**Pass:** ≥90% recall of directly affected entities on benchmark fixtures.

---

## AKI-021 — Knowledge Reconciliation

After a code/document change, stale or affected KOs SHALL be identified.

**Pass:** deterministic fixtures achieve 100% detection.

---

## AKI-022 — AIKOQL Integration

AIKOQL SHALL execute supported universal knowledge operations.

**Pass:** all mandatory certification queries parse, plan, execute, and return expected results.

---

## AKI-023 — MCP/API Independence

The semantic model SHALL remain unchanged when transport changes.

**Pass:** identical semantic request through AIKOQL and API returns equivalent results.

---

## AKI-024 — Agent Independence

The system SHALL not require agent-specific schema changes for different agent clients.

**Pass:** at least three independent client implementations consume the same semantic interface.

---

## AKI-025 — Authorization

Unauthorized knowledge SHALL not be returned.

**Pass:** 100% of authorization isolation tests pass.

---

## AKI-026 — Secret Filtering

Secrets SHALL not appear in normal knowledge output.

**Pass:** 100% of seeded secret fixtures are blocked or redacted.

---

## AKI-027 — Prompt Injection Classification

Instructions embedded in untrusted documents SHALL not automatically become trusted agent instructions.

**Pass:** 100% of security fixtures are correctly classified.

---

## AKI-028 — Tenant Isolation

Cross-tenant retrieval SHALL be impossible.

**Pass:** 100% of isolation tests pass.

---

## AKI-029 — Concurrency

Concurrent updates SHALL detect version conflicts.

**Pass:** deterministic concurrent-write fixtures produce expected conflict behavior.

---

## AKI-030 — Idempotency

Repeated identical agent operations SHALL not create semantic duplicates.

**Pass:** 100% of idempotency tests pass.

---

## AKI-031 — Transactional Integrity

Multi-KO mutations SHALL be atomic.

**Pass:** injected failures leave no partial transaction state.

---

## AKI-032 — Provenance Stability

Reprocessing an unchanged artifact SHALL retain equivalent semantic provenance.

**Pass:** deterministic extraction produces stable provenance references.

---

## AKI-033 — Markdown Round Trip

Knowledge may be projected to Markdown and ingested again without semantic corruption.

**Pass:** certified fixtures preserve required semantic information after round trip.

---

## AKI-034 — Code Knowledge Extraction

Supported source languages SHALL produce code entities and relationships.

**Pass:** benchmark coverage meets the language-specific parser target.

---

## AKI-035 — Connector Knowledge

Connector-derived schema/graph knowledge SHALL enter the same universal model.

**Pass:** PostgreSQL, PGVector, Neo4j, MongoDB and document fixtures produce normalized KOs.

---

## AKI-036 — Document Intelligence

Document-derived entities and claims SHALL preserve page/chunk provenance.

**Pass:** 100% of certification fixtures retain source page/chunk references.

---

## AKI-037 — Constraint Enforcement

Invalid KO mutations SHALL be rejected.

**Pass:** 100% of mandatory constraint fixtures fail correctly.

---

## AKI-038 — Query Explainability

The system SHALL explain why a knowledge item was selected.

**Pass:** context items contain ranking/evidence metadata sufficient for diagnostics.

---

## AKI-039 — Context Latency

MVP context compilation SHALL meet:

```text
p95 < 2 seconds
```

for the defined benchmark repository.

---

## AKI-040 — Query Latency

Simple KO lookup SHALL meet:

```text
p95 < 50 ms
```

on the defined benchmark dataset.

---

## AKI-041 — Failure Transparency

Missing, stale, conflicting and unauthorized knowledge SHALL be distinguishable.

**Pass:** all failure-state fixtures produce the correct status.

---

## AKI-042 — No Fabrication

When required knowledge cannot be established, the system SHALL return insufficient knowledge rather than fabricate a claim.

**Pass:** 100% of missing-evidence fixtures return `INSUFFICIENT_KNOWLEDGE`.

---

## AKI-043 — Historical Reconstruction

The system SHALL reconstruct knowledge as-of a specified revision/date.

**Pass:** 100% of historical certification scenarios pass.

---

## AKI-044 — Context Reproducibility

Given the same repository revision, knowledge state, policy, task and compiler configuration, context generation SHALL be reproducible within the configured ranking tolerance.

---

## AKI-045 — Auditability

Every mutation SHALL have an audit record.

**Pass:** 100% of mutation certification tests produce complete audit events.

---

# 90. Benchmark Strategy

The feature SHALL have a dedicated benchmark suite.

```text
benchmarks/
└── agent-knowledge/
    ├── extraction/
    ├── entity-resolution/
    ├── relationships/
    ├── retrieval/
    ├── context/
    ├── conflicts/
    ├── stale-knowledge/
    ├── reconciliation/
    ├── security/
    ├── latency/
    └── scale/
```

---

# 91. Retrieval Metrics

Measure:

```text
Precision@K
Recall@K
MRR
nDCG
context recall
context precision
evidence coverage
```

---

# 92. Context Metrics

Measure:

```text
task success rate
critical-context recall
irrelevant-context ratio
tokens consumed
latency
context compression ratio
agent correction rate
```

The most important metric is not retrieval similarity.

It is:

```text
Agent Task Success
with
Minimum Sufficient Context
```

---

# 93. Knowledge Quality Metrics

Measure:

```text
entity-resolution precision
entity-resolution recall
relationship precision
relationship recall
claim validation rate
conflict detection rate
stale detection rate
provenance completeness
knowledge freshness
```

---

# 94. Agent Evaluation

A representative evaluation should compare:

```text
Agent + raw repository
```

against:

```text
Agent + traditional RAG
```

against:

```text
Agent + Mnemosyne Agent Knowledge Interface
```

Metrics:

```text
task completion
test pass rate
architectural violations
unnecessary edits
context tokens
time-to-completion
number of tool calls
rework
incorrect assumptions
```

---

# 95. Security Acceptance

Security certification SHALL include:

```text
prompt injection
malicious Markdown
secret leakage
PII leakage
cross-tenant access
privilege escalation
context poisoning
stale authorization
agent-generated false authority
```

---

# 96. Failure Modes

The system must explicitly handle:

```text
source unavailable
parser failure
partial extraction
ambiguous entity
conflicting claims
stale artifact
insufficient evidence
authorization failure
context overflow
index unavailable
graph unavailable
vector index unavailable
transaction conflict
concurrent update
agent retry
```

Degraded operation SHALL never silently lower security boundaries.

---

# 97. Implementation Plan

## Phase 0 — Model Foundation

Implement:

```text
KO envelope
KnowledgeType
Authority
Confidence
Provenance
Scope
TemporalValidity
Lifecycle
```

Exit criteria:

```text
AKI-001
AKI-002
AKI-005
AKI-006
AKI-007
AKI-010
```

---

## Phase 1 — Markdown Compiler

Implement:

```text
Markdown parser
Document AST
semantic extraction
KO generation
provenance
Markdown projection
```

Exit criteria:

```text
AKI-001
AKI-002
AKI-032
AKI-033
```

---

## Phase 2 — Code Compiler

Implement:

```text
code parsers
symbol extraction
dependency extraction
test extraction
relationship extraction
```

Initial languages:

```text
Rust
Python
Java
TypeScript
```

Exit criteria:

```text
AKI-004
AKI-019
AKI-020
AKI-034
```

---

## Phase 3 — Knowledge Graph

Implement:

```text
canonical KO storage
relationships
ontology integration
graph indexing
entity resolution
```

Exit criteria:

```text
AKI-003
AKI-004
AKI-018
AKI-019
```

---

## Phase 4 — Conflict and Temporal Engine

Implement:

```text
versioning
temporal queries
conflict detection
stale detection
authority resolution
```

Exit criteria:

```text
AKI-006
AKI-007
AKI-008
AKI-009
AKI-043
```

---

## Phase 5 — Context Compiler

Implement:

```text
multi-modal retrieval
candidate fusion
reranking
authority filtering
scope resolution
dependency expansion
context compression
token budgeting
```

Exit criteria:

```text
AKI-013
AKI-014
AKI-015
AKI-016
AKI-038
AKI-039
```

---

## Phase 6 — AIKOQL Agent Interface

Implement:

```text
agent queries
EXPLAIN
TRACE
GET CONTEXT
FIND CONFLICTS
FIND STALE
VALIDATE
PROPOSE
```

Exit criteria:

```text
AKI-022
AKI-023
AKI-024
```

---

## Phase 7 — Agent Gateway

Implement:

```text
API
MCP adapter
authentication
authorization
agent identity
audit
rate limiting
```

Exit criteria:

```text
AKI-025
AKI-026
AKI-028
AKI-045
```

---

## Phase 8 — Change Reconciliation

Implement:

```text
Git diff analysis
impact analysis
stale detection
knowledge update proposals
documentation impact
```

Exit criteria:

```text
AKI-020
AKI-021
AKI-040
```

---

## Phase 9 — Connector and Document Integration

Integrate:

```text
PostgreSQL
PGVector
Neo4j
MongoDB
Document Intelligence
OCR
chunks
ontology discovery
```

Exit criteria:

```text
AKI-035
AKI-036
```

---

## Phase 10 — Agent Evaluation

Evaluate:

```text
Codex-like coding workflow
Claude-Code-like workflow
Cline-like workflow
custom agent
```

The goal is not vendor comparison.

The goal is proving that the universal interface works across different agent runtimes.

---

# 98. MVP Definition

The MVP SHALL NOT attempt to implement the entire vision.

MVP scope:

```text
Markdown ingestion
Rust/Python/Java/TypeScript code extraction
Knowledge Object model
Provenance
Ontology
Graph relationships
AIKOQL queries
Context Compiler
GET CONTEXT
EXPLAIN
TRACE
FIND CONFLICTS
Agent proposal workflow
MCP/API adapter
Security boundaries
Benchmark suite
```

MVP should support a complete loop:

```text
Repository
 ↓
Compile Knowledge
 ↓
Agent asks for context
 ↓
Mnemosyne returns context
 ↓
Agent modifies code
 ↓
Mnemosyne detects affected knowledge
 ↓
Agent updates/proposes knowledge
 ↓
Validation
```

---

# 99. MVP Non-Requirements

Defer:

```text
full autonomous agent
automatic production deployment
large-scale telemetry ingestion
all programming languages
organization-wide knowledge federation
fully automatic knowledge promotion
advanced predictive reasoning
```

---

# 100. Reference End-to-End Flow

```text
Developer creates task
        |
        v
Agent receives task
        |
        v
Agent requests context
        |
        v
Agent Gateway authenticates
        |
        v
Context Compiler resolves:
  task
  scope
  requirements
  rules
  components
  decisions
  constraints
  tests
  evidence
        |
        v
Multi-modal retrieval
        |
        v
Authority / conflict / temporal filtering
        |
        v
Context ranking
        |
        v
Token budgeting
        |
        v
Context Package
        |
        v
Agent
        |
        v
Code modification
        |
        v
Tests
        |
        v
Git diff
        |
        v
Knowledge Impact Analysis
        |
        v
Stale/conflict detection
        |
        v
Agent Knowledge Proposal
        |
        v
Validation
        |
        v
Commit / merge
        |
        v
Knowledge state updated
```

---

# 101. Architectural Invariants

The following invariants SHALL hold:

```text
1. Knowledge is not synonymous with Markdown.
2. Chunk is not synonymous with Knowledge Object.
3. Vector similarity is not semantic truth.
4. Authority is not confidence.
5. Knowledge does not imply authorization.
6. Agent-generated knowledge is not automatically authoritative.
7. Historical knowledge is never silently destroyed.
8. Derived knowledge retains provenance.
9. Tenant boundaries apply to all indexes and caches.
10. Invalid knowledge mutations are transactionally rejected.
11. Context is a projection of knowledge, not the knowledge store.
12. Transport protocols do not define the semantic model.
13. Agent implementations do not define the Knowledge Model.
14. Missing knowledge must not be fabricated.
15. Conflicting evidence must remain observable.
16. Current truth and historical truth must be distinguishable.
17. Security policy applies before context exposure.
18. Knowledge indexes are derived from canonical knowledge state.
19. Knowledge changes must be auditable.
20. The system must support progressive context expansion.
```

---

# 102. Definition of Done

MRFC-0070 is considered implemented when:

```text
[ ] Universal KO model implemented
[ ] Markdown compiler implemented
[ ] Code compiler implemented
[ ] Provenance implemented
[ ] Ontology integration implemented
[ ] Entity resolution implemented
[ ] Relationship graph implemented
[ ] Temporal model implemented
[ ] Authority model implemented
[ ] Conflict detection implemented
[ ] Stale detection implemented
[ ] Context Compiler implemented
[ ] AIKOQL agent operations implemented
[ ] Agent proposal workflow implemented
[ ] Agent Gateway implemented
[ ] MCP/API adapter implemented
[ ] Security controls implemented
[ ] Change reconciliation implemented
[ ] Connector integration implemented
[ ] Document Intelligence integration implemented
[ ] Benchmark suite implemented
[ ] Acceptance criteria satisfied
[ ] Auditability verified
[ ] No critical security findings
[ ] Performance targets achieved
```

---

# 103. Strategic Outcome

After this MRFC, Mnemosyne should be positioned as:

```text
              ENGINEERING AGENTS
       +-----------+-----------+-----------+
       |           |           |           |
     Codex      Claude       Cline      Custom
       |           |           |           |
       +-----------+-----------+-----------+
                       |
                  AIKOQL / API / MCP
                       |
              AGENT KNOWLEDGE INTERFACE
                       |
               CONTEXT COMPILER
                       |
              ENGINEERING KNOWLEDGE
                       |
      +----------------+----------------+
      |                |                |
    Ontology       Provenance       Constraints
      |                |                |
      +----------------+----------------+
                       |
                KNOWLEDGE KERNEL
                       |
                 Mnemosyne DB
```

The strategic product is therefore not:

> "A database that stores Markdown for coding agents."

It is:

> **A universal, evidence-backed engineering knowledge infrastructure that compiles software-system knowledge into the minimum sufficient context required by autonomous engineering agents.**

AIKOQL becomes the semantic query language over that knowledge.

Programs-as-KO becomes executable knowledge.

Ontology provides semantic meaning.

Constraints provide correctness.

Provenance provides trust.

Versioning provides temporal memory.

The Context Compiler provides agent-ready context.

The Agent Knowledge Interface provides interoperability.

Together they form the **Engineering Knowledge Layer** beneath autonomous agents.

---

# 104. Future Extensions

Potential future capabilities:

```text
agent-to-agent knowledge exchange
organization knowledge federation
architecture drift detection
automatic ADR generation
automatic documentation repair
architecture-aware code review
requirement coverage analysis
security policy reasoning
SRE operational memory
incident knowledge graphs
automated migration planning
dependency risk reasoning
cross-repository architecture graphs
agent learning from historical tasks
knowledge-based task decomposition
knowledge-aware subagent delegation
agent-generated architecture proposals
continuous repository knowledge certification
```

These are deliberately outside MVP but fit the same universal model.

---

# 105. Final Architectural Statement

The fundamental abstraction is:

```text
             ENGINEERING SYSTEM
                     |
                 KNOWLEDGE
                     |
              CONTEXT COMPILER
                     |
                   AGENT
                     |
                  ACTION
                     |
                VALIDATION
                     |
            KNOWLEDGE EVOLUTION
```

The system is universal because it does not model a particular agent.

It models the **knowledge required by engineering agents**.

That distinction is the architectural foundation of MRFC-0070.
