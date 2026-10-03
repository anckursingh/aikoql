# AIKOQL Goal 2 --- Database Ecosystem & Compatibility Implementation Plan

**Status:** Future enhancement\
**Target:** Post AIKOQL Database 1.0\
**Repository:** `anckursingh/aikoql`\
**Baseline:** `main` branch as reviewed on 2026-09-24\
**Primary principle:** One database engine, multiple protocols and
client surfaces.

------------------------------------------------------------------------

## 1. Objective

Goal 2 is to evolve AIKOQL from a capable database with MCP/REST/SDK
access into a database that can participate naturally in the broader
database ecosystem.

The target is **not** to turn AIKOQL into a PostgreSQL, Neo4j, or Qdrant
clone.

The target architecture is:

``` text
                         AIKOQL
                           |
             +-------------+-------------+
             |                           |
          DATABASE                    KNOWLEDGE
             |                           |
      +------+------+              +-----+------+
      |      |      |              |     |      |
     SQL    KOQL  Native          Graph Vector Semantic
      |      |      |              |     |      |
      +------+------+-+------------+-----+------+
                      |
                 LogicalPlan
                      |
                 PhysicalPlan
                      |
                   Runtime
                      |
                 Kernel
                      |
                 Storage V2
```

External protocols become translation layers:

``` text
MCP       ─┐
SQL       ─┤
Cypher    ─┤
Native    ─┼──> AIKOQL LogicalPlan ─> PhysicalPlan ─> Runtime
REST      ─┤
SDK       ─┘
```

### Core architectural rule

> **Protocols translate into AIKOQL plans; protocols must never
> implement database semantics.**

This prevents AIKOQL from developing separate execution engines for MCP,
SQL, Cypher, or other protocols.

------------------------------------------------------------------------

# 2. Current Repository Baseline

The current `main` branch already contains substantial infrastructure
required by Goal 2:

-   Storage V2
-   MVCC/OCC transaction semantics
-   durable storage and recovery
-   query compiler
-   KOQL
-   logical/physical plan separation
-   streaming execution
-   CBO/index work
-   graph engine
-   vector engine
-   semantic engine
-   temporal/provenance capabilities
-   RBAC/authentication
-   audit
-   MCP server
-   REST surface
-   TCP transport
-   session handling
-   request timeout/cancellation
-   Python SDK
-   benchmark/certification harness

Important existing areas:

``` text
crates/kernel/
crates/compiler/
crates/runtime/
crates/storage/aikoql-v2/
crates/engines/graph/
crates/engines/vector/
crates/engines/semantic/
crates/services/api/mcp/
crates/sdk/python/
crates/providers/
```

The current MCP service is still the application host/orchestrator. Goal
1 should first extract the database/server boundary so Goal 2 can build
on a stable service contract.

------------------------------------------------------------------------

# 3. Goals and Non-Goals

## 3.1 Goals

Goal 2 should eventually provide:

1.  Stable language-neutral database API
2.  Native AIKOQL wire protocol
3.  Connection/session/transaction model
4.  Prepared statements
5.  Result-set abstraction
6.  Durable catalog and introspection
7.  First-party Rust/Python/TypeScript clients
8.  Java and Go clients
9.  SQL compatibility layer
10. PostgreSQL wire compatibility
11. JDBC/ODBC ecosystem access
12. Useful Cypher compatibility
13. Migration tooling
14. Backup/restore tooling
15. Operational observability
16. Connection pooling support
17. Stable compatibility/versioning contracts

## 3.2 Non-goals

Do not introduce these merely for ecosystem compatibility:

-   Full PostgreSQL reimplementation
-   Full Neo4j implementation
-   Full Cypher implementation
-   Qdrant API clone
-   Separate SQL execution engine
-   Separate graph storage engine
-   Separate vector database
-   Distributed consensus/Raft
-   Sharding
-   Multi-region replication
-   GPU query execution
-   MySQL protocol unless a concrete adoption requirement appears

These can be reopened based on measured workload demand.

------------------------------------------------------------------------

# 4. Target Architecture

## 4.1 Database Core

``` text
aikoql-core
    |
    +-- DatabaseInstance
    |
    +-- Connection
    |
    +-- Session
    |
    +-- Transaction
    |
    +-- Statement
    |
    +-- PreparedStatement
    |
    +-- ResultSet
    |
    +-- Catalog
    |
    +-- ExplainPlan
```

The database core owns semantics.

Protocol adapters do not.

## 4.2 Protocol Layer

``` text
protocol/
    native/
    postgres/
    mcp/
    rest/
    sql/
    cypher/
```

Each adapter performs:

``` text
wire request
    ↓
protocol parser
    ↓
AIKOQL request model
    ↓
Database API
    ↓
LogicalPlan
    ↓
PhysicalPlan
    ↓
Runtime
    ↓
response encoder
```

## 4.3 Client Layer

``` text
clients/
    rust/
    python/
    typescript/
    java/
    go/
```

Client libraries should consume the stable native contract rather than
directly coupling to internal kernel types.

------------------------------------------------------------------------

# 5. Implementation Phases

## ECO-1 --- Stable Database API

### Objective

Extract a language-neutral API from the current MCP/application-host
architecture.

### Proposed API

``` rust
pub trait Database {
    fn connect(&self, options: ConnectionOptions)
        -> Result<Connection>;

    fn catalog(&self) -> &Catalog;
}

pub trait Connection {
    fn prepare(&mut self, query: &str)
        -> Result<PreparedStatement>;

    fn execute(&mut self, statement: Statement)
        -> Result<ResultSet>;

    fn begin(&mut self)
        -> Result<Transaction>;
}

pub trait Transaction {
    fn execute(&mut self, statement: Statement)
        -> Result<ResultSet>;

    fn commit(self) -> Result<()>;

    fn rollback(self) -> Result<()>;
}
```

The exact Rust API should be determined during TDD design; the above is
the architectural target, not a mandatory signature.

### Work

-   Extract `DatabaseInstance`
-   Extract connection state
-   Extract transaction lifecycle
-   Move session semantics below MCP
-   Define stable request/response types
-   Remove direct protocol dependency from database operations
-   Make MCP call the Database API
-   Make REST call the same Database API

### Acceptance

``` text
MCP ─────┐
REST ────┼──> Database API ─> Runtime
Native ──┘
```

No protocol adapter directly accesses storage internals.

------------------------------------------------------------------------

# 6. ECO-2 --- Native AIKOQL Wire Protocol

### Objective

Create a protocol optimized for AIKOQL rather than adopting PostgreSQL
wire semantics prematurely.

### Initial operations

``` text
CONNECT
AUTH
OPEN_DATABASE
CLOSE

PREPARE
EXECUTE
EXECUTE_STREAM
DESCRIBE
EXPLAIN

BEGIN
COMMIT
ROLLBACK

CATALOG

PING
HEALTH
METRICS
```

### Protocol requirements

-   explicit framing
-   request IDs
-   protocol version
-   capability negotiation
-   authentication
-   session ID
-   transaction ID
-   prepared statement ID
-   bounded frame size
-   streaming result support
-   server error codes
-   cancellation
-   graceful shutdown
-   backward compatibility

### Recommended initial transport

TCP with a framed protocol.

Do not use MCP JSON-RPC as the permanent database wire protocol.

### Acceptance

A minimal native client can:

``` text
connect
authenticate
prepare
execute
stream
begin
commit
rollback
disconnect
```

with deterministic protocol tests.

------------------------------------------------------------------------

# 7. ECO-3 --- Connection, Session and Transaction Semantics

### Objective

Separate:

``` text
Connection
Session
Transaction
Statement
```

These must not be conflated.

### Target lifecycle

``` text
Connection
    |
    +-- Session
          |
          +-- Prepared Statements
          |
          +-- Transaction
                |
                +-- Statements
```

### Requirements

-   transaction ownership
-   transaction timeout
-   transaction cancellation
-   snapshot identity
-   session identity
-   tenant
-   roles
-   authorization context
-   connection limits
-   idle timeout
-   server-side cleanup
-   connection shutdown semantics

### Acceptance

Concurrent clients must not leak:

-   transactions
-   prepared statements
-   sessions
-   locks
-   snapshots
-   cancellation tokens

------------------------------------------------------------------------

# 8. ECO-4 --- Prepared Statements

### Objective

Avoid parsing and planning repeatedly for recurring queries.

### Pipeline

``` text
PREPARE
   |
   +-- Parse
   +-- Semantic analysis
   +-- LogicalPlan
   +-- PhysicalPlan
   +-- Parameter metadata
   |
   v
PreparedPlan
   |
   +-- EXECUTE(parameters)
```

### Example

``` sql
SELECT *
FROM person
WHERE age > $1;
```

Prepare once:

``` text
PreparedStatement {
    id
    parameter_types
    result_schema
    logical_plan
    physical_plan
}
```

Execute repeatedly:

``` text
execute(id, [35])
execute(id, [42])
execute(id, [50])
```

### Requirements

-   parameter type validation
-   result schema
-   plan invalidation
-   catalog invalidation
-   index invalidation
-   transaction/snapshot compatibility
-   authorization revalidation where required

------------------------------------------------------------------------

# 9. ECO-5 --- Durable Catalog

### Objective

Turn the existing catalog/statistics work into a complete database
metadata system.

### Target metadata

``` text
databases
schemas
types
properties
relationships
indexes
constraints
statistics
users
roles
privileges
prepared_plans
migrations
```

### APIs

``` text
catalog.list_types()
catalog.describe_type()
catalog.list_properties()
catalog.list_indexes()
catalog.statistics()
catalog.list_roles()
catalog.list_privileges()
```

### Query surfaces

Potential future commands:

``` sql
SHOW DATABASES;
SHOW TYPES;
DESCRIBE person;
SHOW INDEXES;
```

### Acceptance

Catalog state must be:

-   durable
-   transactional
-   versioned
-   recoverable
-   observable
-   usable by optimizer
-   usable by external tooling

------------------------------------------------------------------------

# 10. ECO-6 --- First-Party Client Ecosystem

## Rust

Rust should remain the canonical low-level client.

``` rust
let db = Aikoql::connect("aikoql://localhost:7447")?;
let stmt = db.prepare("...")?;
let rows = stmt.query(params)?;
```

## Python

Extend the existing PyO3 SDK to expose:

``` python
connection
transaction
statement
prepared_statement
result_set
catalog
```

while retaining AIKOQL-native operations:

``` python
remember()
get()
forget()
find_similar()
traverse()
```

## TypeScript

Add:

``` text
@aikoql/client
```

supporting:

-   native connection
-   prepared statements
-   transactions
-   streaming
-   KOQL
-   semantic operations
-   MCP integration helpers

### Acceptance

All three clients must execute the same conformance suite.

------------------------------------------------------------------------

# 11. ECO-7 --- SQL Compatibility Layer

### Architecture

Do not build a second executor.

``` text
SQL Parser
    |
SQL AST
    |
SQL Semantic Adapter
    |
AIKOQL LogicalPlan
    |
PhysicalPlan
    |
Runtime
```

KOQL follows the same path:

``` text
KOQL Parser
    |
KOQL AST
    |
AIKOQL LogicalPlan
```

### Initial SQL scope

Start with:

``` text
SELECT
WHERE
ORDER BY
LIMIT
OFFSET
GROUP BY
COUNT
SUM
AVG
MIN
MAX
JOIN
INSERT
UPDATE
DELETE
```

Then progressively add:

``` text
CREATE TYPE
CREATE INDEX
ALTER
DROP
transactions
```

### Important

SQL should initially map to the existing knowledge-object model rather
than forcing AIKOQL to adopt PostgreSQL's internal relational storage
model.

------------------------------------------------------------------------

# 12. ECO-8 --- PostgreSQL Wire Compatibility

Only begin after:

-   Database API is stable
-   Native protocol is stable
-   SQL compatibility exists
-   prepared statements work
-   transaction semantics are stable
-   catalog is available

### Architecture

``` text
PostgreSQL Client
       |
PostgreSQL Wire
       |
AIKOQL PG Adapter
       |
Database API
       |
LogicalPlan
       |
PhysicalPlan
```

### Initial wire surface

Implement the minimum protocol required by:

-   `psql`
-   common PostgreSQL clients
-   connection pools
-   basic database tools

Potentially:

``` text
StartupMessage
Authentication
Query
Parse
Bind
Describe
Execute
Sync
Terminate
RowDescription
DataRow
CommandComplete
ReadyForQuery
ErrorResponse
```

### Acceptance

At minimum:

``` bash
psql -h localhost -p <port>
```

can:

-   connect
-   authenticate
-   execute supported SQL
-   run transactions
-   receive typed rows
-   receive errors correctly

------------------------------------------------------------------------

# 13. ECO-9 --- JDBC / ODBC Ecosystem

Once PostgreSQL protocol compatibility is sufficiently mature:

``` text
Java Application
      |
JDBC
      |
PostgreSQL-compatible wire
      |
AIKOQL
```

and:

``` text
BI Tool
   |
ODBC/JDBC
   |
AIKOQL
```

This avoids implementing an independent database protocol for every
ecosystem.

### Acceptance

Validate with representative:

-   Java application
-   JDBC connection pool
-   database GUI
-   BI client

Do not claim universal compatibility until tested.

------------------------------------------------------------------------

# 14. ECO-10 --- Graph/Cypher Compatibility

Do not implement full Neo4j compatibility initially.

### Initial target

Useful Cypher subset:

``` cypher
MATCH
WHERE
RETURN
ORDER BY
LIMIT
relationships
basic traversal
```

### Architecture

``` text
Cypher
  |
Cypher AST
  |
AIKOQL LogicalPlan
  |
PhysicalPlan
```

AIKOQL graph semantics remain authoritative.

### Important

The existing Neo4j provider should remain an ingestion/source connector,
not become the architectural basis of the graph query engine.

------------------------------------------------------------------------

# 15. ECO-11 --- Vector and Semantic API

Expose AIKOQL's native semantic capabilities rather than cloning Qdrant.

Potential API:

``` text
vector.search()
text.search()
semantic.search()
hybrid.search()
```

Example:

``` text
hybrid.search(
    text = "...",
    vector = [...],
    filter = {...},
    fusion = "rrf",
    k = 10
)
```

The important differentiation is that structured, graph, temporal,
provenance and semantic constraints can participate in one query model.

------------------------------------------------------------------------

# 16. ECO-12 --- Migration System

### CLI

``` bash
aikoql migrate create add_customer
aikoql migrate status
aikoql migrate up
aikoql migrate down
```

### Metadata

``` text
_schema_migrations
```

### Requirements

-   ordered migration IDs
-   checksum
-   transactional migration where possible
-   rollback metadata
-   version tracking
-   migration locking
-   idempotency
-   failed migration recovery

------------------------------------------------------------------------

# 17. ECO-13 --- Backup and Restore

### CLI

``` bash
aikoql backup ./db ./backup
aikoql restore ./backup ./db
aikoql verify-backup ./backup
```

### Requirements

-   consistent snapshot
-   storage V2 checkpoint integration
-   metadata/catalog backup
-   index rebuild/reuse strategy
-   integrity verification
-   restore compatibility checks
-   version metadata

Do not introduce an independent backup format unless Storage V2 cannot
support the required semantics.

------------------------------------------------------------------------

# 18. ECO-14 --- Observability

### Server endpoints

``` text
/health
/ready
/metrics
```

### Database metrics

``` text
queries_total
queries_failed
query_latency
transactions_total
transactions_failed
active_connections
active_transactions
storage_bytes
wal_bytes
compaction_count
index_size
index_lag
cache_hit_ratio
prepared_statement_count
```

### Query diagnostics

``` text
EXPLAIN
EXPLAIN ANALYZE
```

The existing logical/physical plan representation should become the
source for explain output.

------------------------------------------------------------------------

# 19. ECO-15 --- Connection Pooling

The server must support clean pooling semantics:

``` text
Application
     |
Connection Pool
  +-- C1
  +-- C2
  +-- C3
  +-- C4
     |
 AIKOQL Server
```

Requirements:

-   session reset
-   transaction cleanup
-   prepared statement lifecycle
-   authentication reuse
-   tenant isolation
-   cancellation cleanup
-   idle connection handling

A connection returned to a pool must never retain another request's:

-   transaction
-   authorization context
-   snapshot
-   temporary state
-   cancellation token

------------------------------------------------------------------------

# 20. Versioning and Compatibility

Every external protocol should expose a version.

Example:

``` text
AIKOQL protocol v1
KOQL AST v1
Native API v1
Catalog v1
```

Compatibility policy:

``` text
MAJOR.MINOR
```

Breaking changes require a major version.

Additive protocol changes should be negotiated through capabilities.

Example:

``` json
{
  "protocol": "aikoql",
  "version": 1,
  "capabilities": [
    "transactions",
    "streaming",
    "prepared_statements",
    "semantic_search",
    "graph",
    "temporal"
  ]
}
```

------------------------------------------------------------------------

# 21. Testing Strategy

Goal 2 must use conformance testing rather than protocol-specific unit
tests only.

## 21.1 Database conformance suite

Every protocol must produce the same logical result:

``` text
KOQL ─────┐
SQL ──────┼──> same LogicalPlan/result
Native ───┤
MCP ──────┤
Python ───┤
REST ─────┘
```

## 21.2 Protocol tests

Test:

-   malformed frames
-   oversized frames
-   authentication failures
-   authorization failures
-   connection timeout
-   request timeout
-   cancellation
-   disconnect during transaction
-   reconnect
-   concurrent requests
-   concurrent transactions
-   protocol version mismatch

## 21.3 Transaction tests

Test:

``` text
BEGIN
WRITE
READ
COMMIT
```

and:

``` text
BEGIN
WRITE
ROLLBACK
READ
```

plus:

-   concurrent sessions
-   snapshot isolation
-   crash during commit
-   client disconnect during transaction
-   server restart

## 21.4 Prepared statement tests

Test:

-   parameter types
-   invalid parameter count
-   plan reuse
-   catalog invalidation
-   index invalidation
-   transaction interaction

## 21.5 Compatibility tests

Maintain fixtures for:

``` text
psql
Python
TypeScript
Java
Go
MCP clients
```

------------------------------------------------------------------------

# 22. Benchmark Strategy

Goal 2 must preserve the existing benchmark philosophy.

The current competitor benchmark already exposes an important
distinction between:

-   embedded AIKOQL
-   client/server databases
-   MCP mode
-   protocol overhead

Future benchmarks must separate:

``` text
Engine latency
Protocol latency
Serialization latency
Client SDK latency
```

### Benchmark matrix

``` text
                    Embedded   Native   MCP   PostgreSQL-wire
Point read             ✓         ✓       ✓         ✓
Point write            ✓         ✓       ✓         ✓
Filter                 ✓         ✓       ✓         ✓
Transaction            ✓         ✓       ✓         ✓
Graph                  ✓         ✓       ✓         -
Vector                 ✓         ✓       ✓         -
Hybrid                 ✓         ✓       ✓         -
Prepared statement     ✓         ✓       ✓         ✓
Streaming              ✓         ✓       ✓         ✓
```

Never compare protocol-heavy numbers against embedded engine numbers
without explicitly labeling the measurement boundary.

------------------------------------------------------------------------

# 23. Security Requirements

Goal 2 must preserve the existing authorization model.

Every protocol must eventually resolve:

``` text
principal
tenant
roles
permissions
```

into the same AIKOQL security context.

``` text
MCP ─────┐
REST ────┤
Native ──┤
SQL ─────┼──> SecurityContext
Cypher ──┘
```

Never implement protocol-specific authorization rules that bypass the
kernel.

Requirements:

-   TLS
-   authentication
-   authorization
-   tenant isolation
-   credential rotation
-   audit
-   rate limiting
-   connection limits
-   query limits
-   request size limits

TLS can initially be provided through a deployment proxy if native TLS
significantly complicates the first protocol implementation, but native
TLS should be evaluated before production 1.x.

------------------------------------------------------------------------

# 24. Documentation Deliverables

Create:

``` text
docs/
├── DATABASE-API.md
├── NATIVE-PROTOCOL.md
├── CONNECTION-MODEL.md
├── TRANSACTIONS.md
├── PREPARED-STATEMENTS.md
├── CATALOG.md
├── SQL-COMPATIBILITY.md
├── POSTGRES-COMPATIBILITY.md
├── CYPHER-COMPATIBILITY.md
├── CLIENT-SDK.md
├── MIGRATIONS.md
├── BACKUP-RESTORE.md
├── OBSERVABILITY.md
└── COMPATIBILITY-MATRIX.md
```

Website:

``` text
/docs/database
/docs/protocol
/docs/sql
/docs/postgres
/docs/sdk
/docs/operations
/docs/compatibility
```

------------------------------------------------------------------------

# 25. Suggested Repository Structure

After Goal 1 server extraction:

``` text
crates/
├── kernel/
├── compiler/
├── runtime/
├── storage/
├── engines/
│   ├── graph/
│   ├── vector/
│   ├── semantic/
│   └── scheduler/
│
├── database/
│   ├── api/
│   ├── catalog/
│   ├── connection/
│   ├── transaction/
│   ├── statement/
│   └── session/
│
├── server/
│   ├── lifecycle/
│   ├── auth/
│   ├── connection/
│   ├── metrics/
│   └── protocol/
│
├── protocols/
│   ├── native/
│   ├── postgres/
│   ├── mcp/
│   ├── rest/
│   └── cypher/
│
├── sdk/
│   ├── python/
│   ├── typescript/
│   ├── java/
│   └── go/
│
└── providers/
    ├── postgres/
    ├── sqlite/
    ├── mongodb/
    └── neo4j/
```

The exact crate split should be decided during ECO-1 after measuring
dependency boundaries. Avoid creating crates merely for cosmetic
modularity.

------------------------------------------------------------------------

# 26. Dependency Graph

``` text
                    Storage V2
                        |
                      Kernel
                        |
                 Query Runtime
                        |
                 Physical Plan
                        |
                 Logical Plan
                        |
          +-------------+-------------+
          |             |             |
         KOQL          SQL          Cypher
          |             |             |
          +-------------+-------------+
                        |
                  Database API
                        |
              +---------+---------+
              |         |         |
           Native      MCP      REST
              |
        Client ecosystem
       /      |       |      \
   Rust    Python   TS     Java/Go
              |
        JDBC / ODBC
```

------------------------------------------------------------------------

# 27. TDD Contract

Every ECO milestone follows:

``` text
1. Current-state analysis
2. RED test
3. Minimal GREEN implementation
4. Regression suite
5. Performance measurement
6. Security verification
7. Documentation
8. fmt
9. clippy -D warnings
10. workspace tests
11. benchmark evidence where applicable
```

No protocol capability should be considered complete because a
happy-path integration test works.

------------------------------------------------------------------------

# 28. Milestone Acceptance Matrix

  Milestone   Primary outcome            Key acceptance
  ----------- -------------------------- --------------------------------------------
  ECO-1       Stable DB API              MCP/REST use common DB API
  ECO-2       Native protocol            Remote client can CRUD/query/transaction
  ECO-3       Connection/session model   No state leakage across clients
  ECO-4       Prepared statements        Plan reuse works correctly
  ECO-5       Catalog                    Durable metadata and introspection
  ECO-6       Rust/Python/TS clients     Shared conformance suite passes
  ECO-7       SQL adapter                SQL maps to common LogicalPlan
  ECO-8       PG wire                    `psql` can execute supported SQL
  ECO-9       JDBC/ODBC                  Representative ecosystem clients work
  ECO-10      Cypher adapter             Useful graph subset maps to LogicalPlan
  ECO-11      Semantic API               Structured + graph + vector + text compose
  ECO-12      Migrations                 Repeatable schema/catalog changes
  ECO-13      Backup/restore             Verified recoverable backup
  ECO-14      Observability              Metrics + health + EXPLAIN
  ECO-15      Pooling                    Safe connection reuse

------------------------------------------------------------------------

# 29. What Should Be Built First

The recommended order is:

``` text
Goal 1
  |
  +-- DatabaseInstance
  +-- Connection
  +-- Session
  +-- Transaction
  +-- Statement
  +-- ResultSet
  |
  v
ECO-1
  |
  v
ECO-2 Native Protocol
  |
  +-- ECO-3 Connection/Transaction
  |
  +-- ECO-4 Prepared Statements
  |
  +-- ECO-5 Catalog
  |
  v
ECO-6 Clients
  |
  v
ECO-7 SQL
  |
  v
ECO-8 PostgreSQL Wire
  |
  +-- ECO-9 JDBC/ODBC
  |
  +-- ECO-10 Cypher
  |
  +-- ECO-11 Semantic API
  |
  v
ECO-12..15 Operations
```

Do not start ECO-8 before ECO-1 through ECO-7 are stable.

------------------------------------------------------------------------

# 30. Definition of Goal 2 Complete

Goal 2 should be considered complete when AIKOQL can honestly support:

``` text
                     AIKOQL
                        |
        +---------------+---------------+
        |               |               |
      Agents        Applications      Tools
        |               |               |
       MCP          Native/SDK       SQL/PG
        |               |               |
        +---------------+---------------+
                        |
                  Database API
                        |
                  LogicalPlan
                        |
                  PhysicalPlan
                        |
                    Runtime
                        |
                    Storage V2
```

with:

-   stable remote database protocol
-   stable sessions
-   transactions
-   prepared statements
-   streaming
-   durable catalog
-   SQL support
-   PostgreSQL wire compatibility for the supported SQL subset
-   first-party SDKs
-   graph/semantic/vector capabilities
-   migrations
-   backup/restore
-   observability
-   security
-   benchmark evidence
-   protocol conformance tests

The result should still be recognizably **AIKOQL**, not "PostgreSQL
implemented in Rust."

------------------------------------------------------------------------

# 31. Strategic Position

The final product should be positioned around:

``` text
Traditional databases:
    Structured OR Graph OR Vector

AIKOQL:
    Structured
      +
    Graph
      +
    Vector
      +
    Semantic
      +
    Temporal
      +
    Provenance
      +
    Agent identity
      +
    Unified query/runtime
```

The ecosystem compatibility layer is therefore an **adoption
mechanism**, not the product's core identity.

The core moat remains the AIKOQL execution and knowledge model.

------------------------------------------------------------------------

# 32. Immediate Next Action

Before implementing Goal 2:

1.  Complete the Database 1.0/server extraction.
2.  Stabilize Storage V2 as the single production storage engine.
3.  Establish the
    `DatabaseInstance → Connection → Session → Transaction → Statement → ResultSet`
    abstraction.
4.  Add conformance tests around that API.
5.  Only then start ECO-2.

This minimizes architectural rework and makes the later SQL/PG/JDBC/SDK
work primarily protocol adaptation rather than another database
redesign.
