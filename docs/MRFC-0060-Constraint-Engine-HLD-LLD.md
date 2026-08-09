# MRFC-0060 — Schema, Constraint & Integrity Engine

**Status:** Proposed  
**Target:** Mnemosyne MVP → Production  
**Type:** Architecture / HLD / LLD / NFR / Acceptance Criteria  
**Primary owners:** Knowledge Kernel, Schema/Ontology Layer, Transaction Engine  
**Related architecture:** AIKOQL, Knowledge Objects, Ontology Discovery, Connectors, Document Knowledge Compiler, Programs-as-KO, Transactions, Security, Provenance

---

# 1. Executive Summary

A production database cannot rely only on an ontology or on the capabilities of an underlying connector.

Mnemosyne needs its own **canonical schema and constraint contract** so that PostgreSQL, Neo4j, MongoDB, documents, vector indexes, and future storage engines participate in a consistent logical data model.

The Constraint Engine is responsible for determining whether a proposed state transition is legal according to:

- schema
- property types
- nullability
- required fields
- uniqueness
- primary identity
- relationships
- cardinality
- referential integrity
- value domains
- checks
- temporal rules
- security constraints
- ontology rules
- business rules
- cross-object invariants
- tenant boundaries
- version compatibility

The central architectural principle is:

> **Ontology defines meaning. Schema defines structure. Constraints define legal state. Transactions define atomic state transition.**

The canonical flow is:

```text
AIKOQL / API / Agent / Connector
              |
              v
        Authorization
              |
              v
       Schema Resolution
              |
              v
      Constraint Compilation
              |
              v
       Constraint Evaluation
              |
       +------+------+
       |             |
     Valid          Invalid
       |             |
       v             v
  Transaction      Reject
       |
       v
 Knowledge Kernel
       |
       +-------------------------+
       |            |            |
      Graph       Vector       Storage
```

---

# 2. Why Mnemosyne Needs Its Own Constraint Layer

Underlying databases already have constraints, but their semantics differ.

```text
PostgreSQL       relational constraints
Neo4j            graph constraints
MongoDB          document validation/index semantics
Vector stores    index-level constraints
Documents        no native schema
```

If Mnemosyne simply delegates constraint enforcement to each backend, the logical database becomes inconsistent.

Example:

```text
PostgreSQL:
Customer.email UNIQUE

MongoDB:
Customer.email not unique

Document:
Customer email appears multiple times
```

Mnemosyne must define the canonical rule:

```text
Customer.email = UNIQUE
```

and then decide:

```text
Can backend enforce it?
Can Kernel enforce it?
Can it only be validated?
Is it advisory?
```

The logical constraint belongs to Mnemosyne.

---

# 3. Architectural Separation

Three concepts must remain separate.

```text
                    ONTOLOGY
                       |
             "What does this mean?"
                       |
                       v
                    SCHEMA
                       |
             "What does it contain?"
                       |
                       v
                  CONSTRAINT
                       |
             "What states are legal?"
                       |
                       v
                 TRANSACTION
                       |
             "How does state change?"
```

## Ontology

Defines:

```text
Customer
Account
Contract
OWNS
SIGNED
```

## Schema

Defines:

```text
Customer.email : String
Customer.status : Enum
Customer.created_at : Timestamp
```

## Constraint

Defines:

```text
Customer.email IS UNIQUE
Customer.status IN {ACTIVE,SUSPENDED,CLOSED}
Contract.end_date >= Contract.start_date
Account MUST belong to Customer
```

## Transaction

Defines how multiple state changes become one atomic state transition.

---

# 4. Design Goals

The Constraint Engine MUST:

1. provide a canonical logical constraint model
2. validate writes before commit
3. support transaction-aware constraints
4. support graph and relational semantics
5. support property/document validation
6. support cross-object constraints
7. support referential integrity
8. support temporal constraints
9. support schema evolution
10. support inferred constraints
11. support advisory constraints
12. support connector capability mapping
13. expose explainable violations
14. be deterministic for deterministic constraints
15. support Programs-as-KO for programmable rules
16. integrate with authorization
17. integrate with provenance
18. support multi-tenancy
19. avoid unnecessary backend round trips
20. preserve historical validity when schemas evolve

---

# 5. Non-Goals

The Constraint Engine should not become:

- a general-purpose workflow engine
- a full programming language runtime
- an arbitrary LLM evaluator
- a replacement for the transaction engine
- a replacement for ontology management
- a replacement for storage-specific optimization

Programs-as-KO can provide programmable constraint logic, but execution must remain bounded, deterministic where required, observable, and policy-controlled.

---

# 6. Constraint Taxonomy

Mnemosyne should support the following constraint classes.

```text
Schema Constraints
Property Constraints
Identity Constraints
Uniqueness Constraints
Relationship Constraints
Cardinality Constraints
Referential Integrity
Domain Constraints
Check Constraints
Temporal Constraints
Cross-Object Constraints
Cross-Type Constraints
Security Constraints
Tenant Constraints
Version Constraints
Provenance Constraints
Programmable Constraints
```

---

# 7. Schema-Level Constraints

Examples:

```text
schema exists
schema name unique
type name unique within schema
property name unique within type
relationship name unique within namespace
schema version valid
```

Example:

```yaml
schema: commerce
version: 4

constraints:
  type_name_unique: true
  property_name_unique: true
  relationship_name_unique: true
```

---

# 8. Type / Entity Constraints

Example:

```yaml
type: Customer

identity:
  property: customer_id

properties:
  customer_id:
    type: UUID
    required: true
    immutable: true

  email:
    type: STRING
    required: true
    unique: true

  status:
    type: ENUM
    values:
      - ACTIVE
      - SUSPENDED
      - CLOSED
```

A Knowledge Object cannot violate an `ENFORCED` type constraint.

---

# 9. Property Type System

Minimum supported types:

```text
Boolean
Int32
Int64
Float32
Float64
Decimal
String
Text
UUID
Binary
Date
Time
Timestamp
Duration
Enum
Array
Map
Object
Reference
Vector
JSON
```

Future:

```text
Geospatial
Money
PhoneNumber
Email
URL
IPAddress
ULID
Custom Scalar
```

---

# 10. Nullability and Requiredness

Distinguish:

```text
ABSENT
NULL
VALUE
```

This is important for document and schemaless sources.

Example:

```text
Customer.middle_name
```

could be:

```text
ABSENT
```

while:

```text
Customer.middle_name = NULL
```

is explicit null.

A constraint may specify:

```yaml
required: true
nullable: false
```

or:

```yaml
required: false
nullable: true
```

---

# 11. Default Values

Defaults must be explicit.

```yaml
status:
  type: ENUM
  default: ACTIVE
```

Defaults must specify whether they are:

```text
CONSTANT
EXPRESSION
PROGRAM
SERVER_GENERATED
```

Examples:

```text
created_at → server timestamp
id → UUID generator
status → ACTIVE
```

Default evaluation occurs before constraint validation where necessary.

---

# 12. Identity Constraints

Every canonical Knowledge Object should have a stable identity.

Support:

```text
single-property identity
composite identity
generated identity
external identity
canonical identity
```

Example:

```yaml
identity:
  kind: COMPOSITE
  properties:
    - tenant_id
    - account_number
```

Identity must be stable across connectors.

---

# 13. Primary Identity vs External Identity

Separate:

```text
canonical_id
external_id
source_id
```

Example:

```text
canonical:
Customer:C123

PostgreSQL:
customer_id=8472

Neo4j:
node_id=991

Mongo:
customerId="A-77"
```

All may map to the same canonical KO.

---

# 14. Uniqueness

Support:

```text
property UNIQUE
composite UNIQUE
conditional UNIQUE
tenant-scoped UNIQUE
global UNIQUE
```

Example:

```yaml
constraint:
  type: UNIQUE
  target:
    - email
  scope: TENANT
```

Conditional example:

```text
Customer.email UNIQUE
WHERE Customer.status != CLOSED
```

The implementation may require an index-backed strategy.

---

# 15. Relationship Constraints

Example:

```yaml
relationship: OWNS

from: Customer
to: Account

cardinality:
  source: ONE
  target: MANY

required: false

inverse:
  name: OWNED_BY
```

Relationships can have:

```text
allowed source type
allowed target type
cardinality
requiredness
inverse
uniqueness
symmetry
transitivity
acyclicity
```

---

# 16. Cardinality

Support:

```text
ONE_TO_ONE
ONE_TO_MANY
MANY_TO_ONE
MANY_TO_MANY
```

and bounded cardinality:

```text
0..1
1..1
0..N
1..N
2..5
```

Example:

```text
Account MUST belong to exactly one Customer.
Customer MAY own zero or more Accounts.
```

---

# 17. Referential Integrity

Reference policies:

```text
RESTRICT
CASCADE
SET_NULL
DETACH
SOFT_DELETE
ARCHIVE
```

Default for canonical KOs:

```text
RESTRICT
```

unless explicitly configured.

Example:

```text
Customer C123
    |
    +-- OWNS --> Account A456
```

Deleting C123 must evaluate the relationship policy before commit.

---

# 18. Domain Constraints

Values can be constrained by:

```text
min
max
exclusive_min
exclusive_max
precision
scale
length
min_length
max_length
pattern
enum
format
```

Example:

```yaml
age:
  type: INT32
  min: 0
  max: 150

email:
  type: STRING
  format: EMAIL

currency:
  type: STRING
  enum:
    - EUR
    - USD
    - GBP
```

---

# 19. Check Constraints

Example:

```text
Contract.end_date >= Contract.start_date
```

or:

```text
Transaction.amount > 0
```

A check should be declarative where possible.

Example DSL:

```text
CHECK Contract.end_date >= Contract.start_date
```

The constraint compiler converts this to an executable predicate.

---

# 20. Cross-Property Constraints

Example:

```text
Account.closed_at IS NULL
OR Account.status = CLOSED
```

Another:

```text
Customer.type = BUSINESS
→ Customer.company_registration_number IS NOT NULL
```

These require the Constraint Engine to evaluate object state rather than individual fields independently.

---

# 21. Cross-Object Constraints

Example:

```text
Account.balance >= 0
```

may require:

```text
Account
+
Transactions
```

Example:

```text
Account.balance =
SUM(Transaction.amount)
```

These constraints require transaction-scoped reads.

They must execute against a consistent snapshot.

---

# 22. Cross-Type Constraints

Example:

```text
Contract.customer.status != CLOSED
```

or:

```text
Payment.account.status = ACTIVE
```

The engine must understand dependency graphs.

```text
Payment
  ↓
Account
  ↓
Customer
```

Constraint evaluation must detect cycles and avoid unbounded traversal.

---

# 23. Graph Constraints

Mnemosyne requires constraints particularly suited to graph data.

Examples:

### Acyclic

```text
Employee
  └── REPORTS_TO → Employee
```

Constraint:

```text
REPORTS_TO must be acyclic
```

### Symmetric

```text
Person ──KNOWS──> Person
```

may require:

```text
A KNOWS B
⇔
B KNOWS A
```

### Transitive

```text
Organization
  └── PARENT_OF → Organization
```

### Relationship uniqueness

```text
Customer C123
  └── OWNS → Account A456
```

should not create duplicate logical edges if uniqueness is configured.

---

# 24. Temporal Constraints

Support:

```text
valid_from <= valid_to
non-overlapping intervals
effective-date uniqueness
temporal referential integrity
```

Example:

```text
Contract.v1:
2025-01-01 → 2026-05-01

Contract.v2:
2026-05-01 → ∞
```

Invalid:

```text
v1:
2025 → 2027

v2:
2026 → 2028
```

if the ontology requires non-overlapping versions.

---

# 25. Bitemporal Constraints

Future production support should allow:

```text
valid time
transaction time
```

Example:

```text
valid_from
valid_to

recorded_at
superseded_at
```

This is useful when a document says something became true in the past but Mnemosyne learned it later.

---

# 26. Security Constraints

Security should be treated as constraints on state transitions and visibility.

Examples:

```text
tenant_id must match execution context
classification = SECRET → restricted principals
encrypted field cannot be returned without permission
```

The Constraint Engine must integrate with authorization rather than bypass it.

---

# 27. Tenant Constraints

Every canonical object should be tenant-scoped unless explicitly declared global.

```text
tenant_id
schema_id
type_id
object_id
```

A reference across tenants must be rejected unless an explicit cross-tenant policy exists.

---

# 28. Provenance Constraints

Certain Knowledge Objects may require evidence.

Example:

```text
Contract.expiry_date
```

may require:

```text
source_document
page
evidence_region
extraction_method
confidence
```

Constraint:

```text
expiry_date MUST HAVE PROVENANCE
```

This is particularly important for AI-generated or document-derived knowledge.

---

# 29. Confidence Constraints

AI-derived assertions can have minimum confidence policies.

Example:

```yaml
property: customer_id
source: DOCUMENT_EXTRACTION

minimum_confidence: 0.90
```

Policy:

```text
confidence >= 0.90
    → publish

0.70–0.90
    → proposed

< 0.70
    → unresolved
```

These values are policy defaults, not universal constants.

---

# 30. Constraint Enforcement Modes

Every constraint has an enforcement mode:

```text
ENFORCED
VALIDATED
ADVISORY
INFERRED
DISABLED
```

### ENFORCED

Transaction fails on violation.

### VALIDATED

Constraint is evaluated before publication/commit.

### ADVISORY

Write succeeds but violation is recorded.

### INFERRED

Constraint was discovered from evidence but is not authoritative.

### DISABLED

Constraint is retained but not evaluated.

---

# 31. Constraint Severity

Support:

```text
ERROR
WARNING
INFO
```

Example:

```text
ERROR:
Customer.email uniqueness violated

WARNING:
Inferred Customer.email uniqueness violated

INFO:
Document assertion conflicts with low-authority source
```

---

# 32. Constraint Provenance

Every constraint should have provenance.

```yaml
constraint:
  name: CustomerEmailUnique
  source:
    kind: USER_DEFINED
    principal: admin
  created_at: ...
  version: 2
```

For inferred constraints:

```yaml
source:
  kind: INFERRED
  evidence_count: 18472
  confidence: 0.998
```

---

# 33. Constraints as Knowledge Objects

Constraints may themselves be represented as KOs.

```text
Constraint
 ├── target
 ├── kind
 ├── expression
 ├── enforcement
 ├── severity
 ├── version
 ├── provenance
 └── status
```

This allows:

```text
Ontology
 └── Customer
      ├── Property
      ├── Relationship
      └── Constraint
```

It also makes constraints queryable through AIKOQL.

---

# 34. Programs-as-KO Integration

Programmable constraints can be represented using Programs-as-KO.

Example:

```text
Constraint:
AccountBalanceInvariant
```

Program:

```text
balance == SUM(posted_transactions)
```

The program must declare:

```text
inputs
permissions
resource limits
determinism
side effects
timeout
version
```

A constraint program must not perform arbitrary mutations.

It should preferably be:

```text
pure
deterministic
read-only
bounded
```

---

# 35. Constraint Compilation

Do not evaluate every constraint by interpreting a generic object at runtime.

Compile constraints into executable plans.

```text
Constraint Definition
        ↓
Parser
        ↓
Type Checker
        ↓
Dependency Analysis
        ↓
Execution Plan
        ↓
Compiled Constraint
        ↓
Constraint Cache
```

Example:

```text
CHECK Contract.end_date >= Contract.start_date
```

becomes a direct predicate.

---

# 36. Constraint Dependency Graph

Constraints may depend on other objects.

Example:

```text
Payment
  ↓
Account
  ↓
Customer
```

The engine should build:

```text
Constraint Dependency Graph
```

and evaluate only affected constraints.

If:

```text
Customer.email
```

changes, don't reevaluate unrelated:

```text
Contract.expiry_date
```

constraints.

---

# 37. Incremental Constraint Evaluation

A transaction produces a write set:

```text
WriteSet
 ├── Customer:C123.email
 ├── Account:A456.status
 └── Contract:C900.expiry_date
```

The engine maps the write set to affected constraints:

```text
WriteSet
   ↓
Dependency Index
   ↓
Affected Constraints
   ↓
Evaluate
```

This is essential for production performance.

---

# 38. Constraint Fast Path

Most writes should not require a full graph scan.

Use:

```text
local property checks
      ↓
indexed uniqueness
      ↓
direct references
      ↓
bounded graph checks
      ↓
cross-object checks
      ↓
programmable checks
```

Stop early when a definitive violation occurs.

---

# 39. Deferred Constraints

Some constraints cannot be evaluated after every individual operation.

Support:

```text
IMMEDIATE
DEFERRED
```

Example transaction:

```text
INSERT Customer
INSERT Account referencing Customer
COMMIT
```

The temporary intermediate state may violate a constraint, while the final transaction state is valid.

Therefore:

```text
statement-time validation
```

and:

```text
commit-time validation
```

must be distinct.

---

# 40. Transaction Integration

The Constraint Engine must never independently commit data.

Correct:

```text
Transaction
    ↓
Read Snapshot
    ↓
Apply Candidate Writes
    ↓
Constraint Evaluation
    ↓
Conflict Detection
    ↓
Commit
```

The Kernel owns commit.

The Constraint Engine returns:

```rust
ConstraintResult {
    valid: bool,
    violations: Vec<ConstraintViolation>,
    warnings: Vec<ConstraintWarning>,
}
```

---

# 41. Constraint Isolation

Constraint reads must obey the transaction's isolation level.

At minimum:

```text
READ_COMMITTED
SNAPSHOT
SERIALIZABLE
```

A uniqueness constraint cannot be evaluated against a stale snapshot and then blindly committed.

The transaction system must coordinate:

```text
constraint validation
+
write conflicts
+
commit
```

---

# 42. Concurrency

Example:

```text
Transaction A:
email = a@example.com

Transaction B:
email = a@example.com
```

Both cannot independently pass a uniqueness check and commit.

The final enforcement requires:

```text
unique index / lock / serialization protocol
```

depending on the constraint implementation.

The logical constraint engine chooses the required enforcement strategy; the storage/transaction layer provides atomicity.

---

# 43. Connector Capability Mapping

Each connector declares capabilities:

```yaml
postgresql:
  unique: true
  foreign_key: true
  check: true
  not_null: true
  transaction: true

neo4j:
  unique: true
  relationship_cardinality: kernel
  foreign_key: kernel

mongodb:
  unique: partial
  validation: true
  foreign_key: kernel
```

The actual capability matrix must be discovered/configured per connector version.

---

# 44. Enforcement Placement

A constraint can be enforced at:

```text
CONNECTOR
KERNEL
TRANSACTION
INDEX
APPLICATION
```

Preferred strategy:

```text
Underlying DB capability
        ↓
use native enforcement where semantically equivalent
        ↓
Kernel enforcement for logical constraints
        ↓
Transaction enforcement for cross-object invariants
```

Never assume backend enforcement is equivalent without capability verification.

---

# 45. Constraint Pushdown

Where safe, push constraints to connectors.

Example:

```text
Customer.email = 'a@example.com'
```

can become a PostgreSQL predicate.

But semantic constraints that the backend cannot guarantee remain in Mnemosyne.

Pushdown is an optimization, not a semantic authority.

---

# 46. Schema Evolution

Constraint changes require schema versions.

Supported operations:

```text
ADD_CONSTRAINT
DROP_CONSTRAINT
ENABLE_CONSTRAINT
DISABLE_CONSTRAINT
ALTER_CONSTRAINT
RENAME_CONSTRAINT
CHANGE_ENFORCEMENT
CHANGE_SEVERITY
```

Example:

```text
Schema v3
  Customer.email nullable

Schema v4
  Customer.email required
```

Before enabling the constraint, run:

```text
validation scan
```

and report existing violations.

---

# 47. Constraint Migration States

```text
PROPOSED
    ↓
VALIDATING
    ↓
VALIDATED
    ↓
ENABLED
    ↓
DISABLED / SUPERSEDED
```

An unsafe constraint must not be enabled blindly.

---

# 48. Constraint Discovery

Mnemosyne should eventually infer constraints from:

```text
database metadata
data statistics
documents
ontology
existing KOs
query patterns
application behavior
historical writes
```

Examples:

```text
email appears unique in 20M rows
```

→ inferred uniqueness candidate.

```text
end_date >= start_date in 99.99% of records
```

→ inferred temporal check candidate.

Inferred constraints remain:

```text
INFERRED
```

until explicitly promoted.

---

# 49. Constraint Validation Report

Before enabling a constraint:

```text
Constraint: Customer.email UNIQUE
Status: FAILED

Records scanned: 10,000,000
Violations: 127

Top duplicates:
a@example.com → 4
b@example.com → 3
...
```

For inferred constraints:

```text
observations
violations
confidence
coverage
false-positive estimate
```

should be available.

---

# 50. Violation Model

```rust
pub struct ConstraintViolation {
    pub constraint_id: ConstraintId,
    pub constraint_version: u64,
    pub object_ids: Vec<KnowledgeObjectId>,
    pub property_paths: Vec<PropertyPath>,
    pub expected: Option<Value>,
    pub actual: Option<Value>,
    pub severity: Severity,
    pub message: String,
    pub provenance: Option<ProvenanceRef>,
}
```

A violation should be machine-readable, not only a string.

---

# 51. Explainability

AIKOQL/Studio should be able to answer:

```text
WHY WAS THIS WRITE REJECTED?
```

Example:

```text
Constraint:
CustomerEmailUnique

Violation:
Customer:C123.email

Existing object:
Customer:C456

Value:
a@example.com

Policy:
UNIQUE WITHIN TENANT

Transaction:
txn-889
```

This should be exposed through:

```text
explain constraint
```

---

# 52. AIKOQL Constraint Queries

Examples:

```aikoql
SHOW CONSTRAINTS
```

```aikoql
SHOW CONSTRAINTS ON Customer
```

```aikoql
SHOW VIOLATIONS FOR Customer
```

```aikoql
EXPLAIN CONSTRAINT CustomerEmailUnique
```

```aikoql
FIND INFERRED CONSTRAINTS
```

```aikoql
VALIDATE CONSTRAINT CustomerEmailUnique
```

Actual syntax should follow the AIKOQL grammar and parser architecture.

---

# 53. Studio Constraint Management

Add:

```text
Studio
 └── Schema
      ├── Types
      ├── Properties
      ├── Relationships
      ├── Constraints
      ├── Violations
      ├── Inferred Constraints
      └── Schema Versions
```

Constraint detail:

```text
CustomerEmailUnique

Target:
Customer.email

Type:
UNIQUE

Scope:
TENANT

Mode:
ENFORCED

Status:
ENABLED

Version:
3

Evidence:
...
```

---

# 54. NFR — Correctness

For deterministic constraints:

> A committed transaction MUST NOT violate an enabled `ENFORCED` constraint.

Exceptions require explicit transactional semantics such as deferred constraints.

---

# 55. NFR — Atomicity

Constraint validation and commit must form one transactional protocol.

A transaction must not:

```text
pass validation
+
fail to enforce the same condition during commit
```

due to race conditions.

---

# 56. NFR — Performance

Target architecture:

```text
property validation       O(1)
indexed uniqueness        O(log N)
direct reference          O(log N)
bounded relationship      O(k)
cross-object              bounded by declared dependency
program constraint        bounded by resource policy
```

No unconstrained graph traversal should occur during a normal write.

---

# 57. NFR — Determinism

Declarative constraints must be deterministic.

Programmable constraints should be deterministic unless explicitly classified otherwise.

External network calls should not be permitted from transactional constraint programs.

---

# 58. NFR — Availability

A temporary analytics/index failure should not necessarily prevent a write if the constraint has an independent authoritative enforcement mechanism.

Conversely:

> An `ENFORCED` constraint must fail closed if Mnemosyne cannot safely determine whether the write is valid.

---

# 59. NFR — Explainability

100% of rejected writes must expose:

```text
constraint
version
target
actual value
expected condition
affected objects
transaction
```

where available.

---

# 60. NFR — Multi-Tenancy

Constraint evaluation must always respect tenant scope.

Cross-tenant reads require explicit authorization.

---

# 61. NFR — Schema Evolution

Constraint versions must be immutable.

Changing:

```text
expression
scope
enforcement
severity
target
```

creates a new constraint version.

---

# 62. NFR — Observability

Metrics:

```text
constraint_evaluations_total
constraint_failures_total
constraint_violations_total
constraint_evaluation_duration
constraint_pushdown_total
constraint_pushdown_failures
constraint_deferred_total
constraint_inferred_total
constraint_validation_scan_duration
```

Tracing:

```text
transaction
 ├── schema resolution
 ├── constraint compilation
 ├── constraint evaluation
 │    ├── property
 │    ├── uniqueness
 │    ├── relationship
 │    └── programmable
 └── commit
```

---

# 63. LLD Package Structure

Recommended:

```text
crates/
├── kernel/
│   ├── knowledge/
│   ├── storage/
│   ├── transaction/
│   ├── security/
│   ├── lifecycle/
│   └── constraints/
│       ├── mod.rs
│       ├── engine.rs
│       ├── compiler.rs
│       ├── evaluator.rs
│       ├── planner.rs
│       ├── dependency.rs
│       ├── cache.rs
│       ├── violation.rs
│       ├── context.rs
│       ├── policy.rs
│       ├── deferred.rs
│       ├── inference.rs
│       ├── reconciliation.rs
│       ├── pushdown.rs
│       └── capabilities.rs
│
├── engines/
│   ├── query/
│   ├── planner/
│   ├── optimizer/
│   ├── graph/
│   ├── vector/
│   ├── semantic/
│   └── indexing/
│
├── services/
│   ├── api/
│   ├── auth/
│   ├── telemetry/
│   └── migration/
│
└── integration-tests/
    └── constraints/
```

---

# 64. Rust Core Model

```rust
pub struct ConstraintDefinition {
    pub id: ConstraintId,
    pub name: String,
    pub version: u64,
    pub target: ConstraintTarget,
    pub kind: ConstraintKind,
    pub expression: ConstraintExpression,
    pub enforcement: EnforcementMode,
    pub severity: Severity,
    pub scope: ConstraintScope,
    pub provenance: Option<ProvenanceRef>,
}
```

```rust
pub enum ConstraintKind {
    Required,
    Type,
    Unique,
    Check,
    Range,
    Pattern,
    Enum,
    Reference,
    Cardinality,
    Relationship,
    Acyclic,
    Temporal,
    CrossObject,
    CrossType,
    Security,
    Tenant,
    Provenance,
    Programmable,
}
```

---

# 65. Constraint Context

```rust
pub struct ConstraintContext {
    pub tenant_id: TenantId,
    pub transaction_id: TransactionId,
    pub snapshot: SnapshotId,
    pub schema_version: SchemaVersion,
    pub security_context: SecurityContext,
    pub write_set: WriteSet,
}
```

Constraints must evaluate against this context.

---

# 66. Constraint Engine Interface

```rust
#[async_trait]
pub trait ConstraintEngine: Send + Sync {
    async fn validate(
        &self,
        ctx: ConstraintContext
    ) -> Result<ConstraintResult>;

    async fn validate_constraint(
        &self,
        constraint_id: ConstraintId,
        ctx: ConstraintContext
    ) -> Result<ConstraintResult>;

    async fn explain(
        &self,
        constraint_id: ConstraintId
    ) -> Result<ConstraintExplanation>;
}
```

---

# 67. Compiler

```rust
pub trait ConstraintCompiler: Send + Sync {
    fn compile(
        &self,
        definition: &ConstraintDefinition
    ) -> Result<CompiledConstraint>;
}
```

Compilation steps:

```text
parse
 ↓
type check
 ↓
normalize
 ↓
dependency extraction
 ↓
capability analysis
 ↓
pushdown analysis
 ↓
execution plan
 ↓
compiled representation
```

---

# 68. Dependency Index

```rust
pub struct ConstraintDependencyIndex {
    property_to_constraints: Map<PropertyPath, Vec<ConstraintId>>,
    type_to_constraints: Map<TypeId, Vec<ConstraintId>>,
    relationship_to_constraints: Map<RelationshipId, Vec<ConstraintId>>,
}
```

Write-set driven evaluation:

```text
WriteSet
   ↓
Dependency Index
   ↓
Affected Constraints
   ↓
Constraint Plan
```

---

# 69. Unique Constraint Implementation

Recommended strategy:

```text
logical UNIQUE constraint
        ↓
canonical uniqueness key
        ↓
index
        ↓
atomic reservation/check
        ↓
commit
```

Canonical key:

```text
tenant_id
+
type_id
+
property values
```

for tenant-scoped uniqueness.

---

# 70. Referential Integrity Implementation

For:

```text
Customer OWNS Account
```

the engine verifies:

```text
source exists
target exists
types valid
tenant compatible
cardinality valid
relationship uniqueness valid
delete/update policy valid
```

---

# 71. Deferred Constraint Implementation

Maintain:

```text
TransactionConstraintState
```

with:

```text
pending constraints
pending references
pending uniqueness keys
```

At commit:

```text
statement validation
       ↓
deferred validation
       ↓
conflict detection
       ↓
commit
```

---

# 72. Constraint Caching

Cache:

```text
compiled constraint
dependency plan
connector capability
pushdown plan
```

Do not cache transaction-specific validation results unless their snapshot dependencies are explicitly represented.

Cache key:

```text
constraint_id
+
constraint_version
+
schema_version
+
compiler_version
```

---

# 73. Connector Capability Interface

```rust
pub trait ConstraintCapabilityProvider {
    fn capabilities(&self) -> ConstraintCapabilities;
}
```

Example:

```rust
pub struct ConstraintCapabilities {
    pub unique: bool,
    pub foreign_key: bool,
    pub check: bool,
    pub not_null: bool,
    pub transactions: bool,
    pub conditional_unique: bool,
}
```

Capabilities must be version-aware.

---

# 74. Pushdown Safety

A constraint may be pushed down only if:

```text
semantic equivalence is proven
AND
transaction semantics are compatible
AND
null semantics are compatible
AND
collation/type semantics are compatible
AND
tenant scope is preserved
```

Otherwise evaluate in Mnemosyne.

---

# 75. Inference Engine

Future inference pipeline:

```text
Existing Data
   ↓
Statistics
   ↓
Pattern Mining
   ↓
Candidate Constraint
   ↓
Historical Validation
   ↓
Confidence
   ↓
Proposal
```

Example:

```text
Candidate:
Customer.email UNIQUE

Rows:
20,000,000

Duplicate violations:
0

Confidence:
0.999+

Status:
INFERRED
```

Inference must never silently promote itself to `ENFORCED`.

---

# 76. Migration Strategy

When enabling:

```text
UNIQUE Customer.email
```

run:

```text
Discovery
 ↓
Validation Scan
 ↓
Violation Report
 ↓
Repair / Migration
 ↓
Enable
```

When dropping a constraint:

```text
schema version increment
 ↓
constraint superseded
 ↓
indexes/caches updated
 ↓
new transactions use new schema
```

Historical transactions remain interpretable using their original schema version.

---

# 77. Failure Semantics

Constraint evaluation can fail because of:

```text
timeout
storage unavailable
index unavailable
program limit
connector unavailable
schema mismatch
unknown capability
```

Policy:

### ENFORCED

Fail closed.

### VALIDATED

Fail closed unless policy explicitly allows fallback.

### ADVISORY

Record failure and continue according to policy.

### INFERRED

Record evaluation failure; never block writes.

---

# 78. Security of Programmable Constraints

Programs-as-KO constraints require:

```text
sandbox
CPU limit
memory limit
execution timeout
read-set restriction
no arbitrary network
no arbitrary filesystem
no uncontrolled mutation
version pinning
audit
```

A constraint program should be pure or effectively pure.

---

# 79. Testing Strategy

## Unit

Test:

```text
type
required
nullable
default
unique
range
pattern
enum
reference
cardinality
check
temporal
acyclic
cross-object
programmable
```

## Transaction tests

Test:

```text
single write
multi-write
rollback
commit
deferred constraint
concurrent writes
snapshot isolation
serializable conflicts
```

## Connector tests

Test semantic equivalence against:

```text
PostgreSQL
Neo4j
MongoDB
```

## Property-based tests

Generate random:

```text
schemas
KOs
relationships
transactions
constraint combinations
```

and verify invariants.

## Fuzzing

Fuzz:

```text
constraint parser
expression compiler
schema migration
constraint serialization
AIKOQL constraint syntax
```

---

# 80. Acceptance Criteria

### AC-01

A required property missing from an object causes an `ENFORCED` write to fail.

### AC-02

A type mismatch causes a write failure.

### AC-03

A unique constraint prevents duplicate values within its configured scope.

### AC-04

Composite uniqueness works.

### AC-05

Tenant-scoped uniqueness does not incorrectly reject identical values in different tenants.

### AC-06

Relationship source/target types are validated.

### AC-07

Configured cardinality is enforced.

### AC-08

Referential integrity prevents invalid dangling references.

### AC-09

Configured delete policy is respected.

### AC-10

Check constraints are evaluated atomically with the transaction.

### AC-11

Cross-property constraints work.

### AC-12

Cross-object constraints evaluate against a consistent transaction snapshot.

### AC-13

Deferred constraints are validated at commit.

### AC-14

Concurrent transactions cannot both commit a conflicting enforced uniqueness constraint.

### AC-15

Temporal overlap constraints work.

### AC-16

Tenant boundaries are enforced.

### AC-17

Provenance-required properties reject unsupported AI/document assertions.

### AC-18

Inferred constraints never become enforced without explicit promotion.

### AC-19

Constraint violations expose machine-readable details.

### AC-20

AIKOQL can inspect constraints and violations.

### AC-21

Constraint versions remain immutable.

### AC-22

Schema migration detects existing violations before enabling a new enforced constraint.

### AC-23

Equivalent connector capabilities can be used for pushdown without changing logical semantics.

### AC-24

Unsupported backend constraints are enforced by the Kernel when possible.

### AC-25

Constraint compilation is cached by schema/constraint/compiler version.

### AC-26

Only constraints affected by a transaction write set are evaluated where dependency analysis permits.

### AC-27

Programmable constraints execute inside resource limits.

### AC-28

Constraint failure never leaves a partially committed transaction.

### AC-29

Constraint evaluation respects authorization and tenant context.

### AC-30

Constraint explainability identifies the violated rule and affected objects.

---

# 81. Example End-to-End Transaction

Input:

```aikoql
CREATE Customer {
    customer_id: "C123",
    email: "a@example.com",
    status: "ACTIVE"
}
```

Pipeline:

```text
AIKOQL
  ↓
Parser
  ↓
Planner
  ↓
Schema Resolution
  ↓
Type Validation
  ↓
Required Validation
  ↓
Default Evaluation
  ↓
Unique Constraint
  ↓
Tenant Constraint
  ↓
Authorization
  ↓
Transaction
  ↓
Commit
```

If the email already exists:

```text
Transaction rejected

Constraint:
CustomerEmailUnique

Existing:
Customer:C456

Requested:
Customer:C123

Scope:
TENANT

Reason:
UNIQUE violation
```

---

# 82. Example Graph Transaction

```text
CREATE Customer C123
CREATE Account A456
CREATE OWNS(C123, A456)
```

Constraints:

```text
Customer exists
Account exists
OWNS source type = Customer
OWNS target type = Account
Customer cardinality valid
Account has exactly one owner
tenant IDs match
```

All must be satisfied before commit.

---

# 83. Example Document-Derived Knowledge

Document extraction produces:

```text
Contract C900
expiry_date = 2026-12-31
confidence = 0.97
evidence = document/page/region
```

Constraint:

```text
Contract.expiry_date MUST HAVE provenance
confidence >= 0.90
```

Result:

```text
VALID
```

If confidence is:

```text
0.71
```

the configured policy may produce:

```text
PROPOSED
```

rather than publish the assertion as authoritative.

---

# 84. Example Inferred Constraint

From millions of records:

```text
Transaction.amount > 0
```

Observed:

```text
20,000,000 records
0 violations
```

Mnemosyne proposes:

```text
Constraint:
TransactionAmountPositive

Mode:
INFERRED

Confidence:
high

Evidence:
20M observations
```

An administrator may promote it:

```text
INFERRED
   ↓
VALIDATED
   ↓
ENFORCED
```

---

# 85. Production Invariants

The following invariants are mandatory:

```text
I-01
An enabled enforced constraint cannot be bypassed through an alternate API.

I-02
Connector writes must pass through the same logical constraint policy.

I-03
Constraint validation and commit must share transactional semantics.

I-04
Historical schema versions remain interpretable.

I-05
A failed transaction produces no partial state.

I-06
Cross-tenant references are forbidden unless explicitly authorized.

I-07
Inferred constraints cannot silently become enforced.

I-08
Constraint versions are immutable.

I-09
Programmable constraints cannot perform uncontrolled external side effects.

I-10
Constraint evaluation is auditable.

I-11
Constraint violations are explainable.

I-12
Pushdown is an optimization and never changes logical semantics.
```

---

# 86. Recommended Implementation Order

## Phase C1 — Core schema validation

```text
type
required
nullable
default
enum
range
pattern
```

## Phase C2 — Identity and uniqueness

```text
primary identity
composite identity
unique
tenant-scoped unique
```

## Phase C3 — Relationships

```text
reference
cardinality
relationship type
inverse
referential integrity
```

## Phase C4 — Transaction integration

```text
write set
snapshot
immediate/deferred
commit validation
concurrency
```

## Phase C5 — Declarative checks

```text
check expressions
cross-property
cross-object
temporal
```

## Phase C6 — Connector pushdown

```text
capability discovery
equivalence checks
pushdown
fallback
```

## Phase C7 — Schema evolution

```text
constraint versions
migration
validation scans
rollout
rollback
```

## Phase C8 — Intelligence

```text
constraint inference
confidence
human approval
constraint proposals
```

## Phase C9 — Programs-as-KO

```text
sandbox
resource limits
deterministic execution
programmable constraints
```

---

# 87. Final Architecture

The database correctness layer should ultimately be:

```text
                         AIKOQL
                            |
                     Query / Mutation
                            |
                    Authorization
                            |
                    Schema Resolution
                            |
                  Constraint Compilation
                            |
                 Constraint Dependency Graph
                            |
                  Constraint Evaluation
                            |
                  Transaction / MVCC
                            |
                  Conflict Detection
                            |
                         Commit
                            |
                    Knowledge Kernel
                            |
          +-----------------+----------------+
          |                 |                |
        Graph             Vector          Storage
          |                 |                |
     PostgreSQL           HNSW            MongoDB
     Neo4j                BM25            Other
          |
          +------ Connectors / Federation
```

And the semantic hierarchy becomes:

```text
Ontology
   ↓
Schema
   ↓
Constraints
   ↓
Transaction
   ↓
Knowledge State
```

while the intelligent data path is:

```text
Documents / Databases / APIs
          ↓
      Discovery
          ↓
       Ontology
          ↓
     Knowledge IR
          ↓
   Constraint Validation
          ↓
   Knowledge Objects
          ↓
Graph + Vector + Evidence
          ↓
        AIKOQL
```

---

# 88. Final Architectural Position

Mnemosyne should not merely ask:

> "What does this data mean?"

It must also answer:

> "What structure does this data conform to?"

and:

> "Is this state legal?"

and:

> "Which rules caused this state transition to be accepted or rejected?"

That requires a first-class **Schema + Constraint + Transaction correctness layer**.

The resulting division of responsibility is:

```text
Ontology
    = semantics

Schema
    = structure

Constraint Engine
    = validity

Transaction Engine
    = atomic state transition

Knowledge Kernel
    = canonical knowledge state

Storage Engines
    = physical persistence

Provenance
    = evidence

AIKOQL
    = query/mutation language

Agents
    = intelligent consumers
```

This should be treated as a foundational database subsystem rather than a feature attached to the ontology layer.
