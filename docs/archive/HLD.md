# Aikoql High Level Design (HLD)

**Version:** 1.0  
**Status:** Draft  
**Project:** aikoql – The Knowledge Operating System for AI

---

# 1. Purpose

This document defines the high-level architecture of aikoql. It is the authoritative architectural blueprint for implementation teams and AI coding agents.

## Scope

- Overall system architecture
- Major subsystems
- Folder/workspace layout
- Responsibilities
- Module dependencies
- NFRs
- Acceptance criteria

Out of scope:
- Internal algorithms
- Binary formats
- Page layouts
- Query optimizer internals

---

# 2. Architectural Principles

1. Everything is a Knowledge Object (KO).
2. Kernel-first architecture.
3. Storage is AI-agnostic.
4. Semantic intelligence is layered.
5. One canonical object model.
6. Stable interfaces over implementations.
7. Modular Rust workspace.
8. Test-first implementation.

---

# 3. System Architecture

```
Applications
     │
SDKs / SQL / REST / GraphQL / MCP
     │
API Gateway
     │
Query Compiler
     │
Knowledge IR
     │
Optimizer
     │
Knowledge VM
     │
Knowledge Kernel
     │
Storage Kernel
     │
Filesystem / Cloud Storage
```

---

# 4. Cargo Workspace Layout

```text
aikoql/
├── crates/
│   ├── kernel/
│   │   ├── knowledge/
│   │   ├── storage/
│   │   ├── transaction/
│   │   ├── security/
│   │   └── lifecycle/
│   ├── engines/
│   │   ├── query/
│   │   ├── planner/
│   │   ├── optimizer/
│   │   ├── graph/
│   │   ├── vector/
│   │   ├── semantic/
│   │   ├── reasoning/
│   │   ├── scheduler/
│   │   └── indexing/
│   ├── runtime/
│   │   ├── kvm/
│   │   ├── bytecode/
│   │   └── execution/
│   ├── cluster/
│   │   ├── consensus/
│   │   ├── replication/
│   │   ├── sharding/
│   │   └── membership/
│   ├── services/
│   │   ├── api/
│   │   ├── auth/
│   │   ├── telemetry/
│   │   ├── backup/
│   │   └── migration/
│   ├── sdk/
│   │   ├── rust/
│   │   ├── python/
│   │   ├── java/
│   │   ├── go/
│   │   └── typescript/
│   ├── plugins/
│   ├── benchmarks/
│   ├── integration-tests/
│   └── fuzz/
├── docs/
├── rfcs/
└── tools/
```

---

# 5. Component Responsibilities

## Knowledge Kernel
Canonical implementation of KO/KR/KE/KV. Owns validation, lifecycle, metadata and semantic contracts.

## Storage Kernel
Page manager, allocator, WAL/Knowledge Journal, checkpoints, recovery, compression, encryption and storage drivers.

## Transaction Kernel
MVCC, snapshots, locking, visibility rules, commit, rollback and recovery.

## Query Engine
Parsers, AST, Knowledge IR generation and frontend adapters.

## Planner
Logical planning, cost estimation and hybrid planning.

## Optimizer
Rule-based and cost-based optimization across relational, graph, vector and semantic operators.

## Knowledge VM
Executes Knowledge Bytecode. Schedules operators and coordinates execution.

## Graph Engine
Relationship traversal, path expansion and graph indexes.

## Vector Engine
Embedding storage, ANN indexes, similarity search and reranking.

## Semantic Engine
NER, embeddings, summarization, classification and metadata generation.

## Reasoning Engine
Rule execution, ontology processing, provenance and inference.

## Scheduler
Background jobs, embedding generation, compaction, indexing and maintenance.

## Cluster
Consensus, replication, routing, metadata and shard management.

## Security
Authentication, RBAC, ACL, encryption, auditing and multi-tenancy.

## API Gateway
REST, SQL, GraphQL, gRPC and MCP endpoints.

## SDKs
Native client libraries.

## Plugins
Extension point for storage, AI providers, compression, authentication and query languages.

---

# 6. Dependency Rules

Allowed

API -> Query -> Planner -> KVM -> Kernel -> Storage

Forbidden

- Storage -> Query
- Storage -> AI
- Planner -> Storage internals
- AI -> Page manager

---

# 7. Non Functional Requirements

## Availability

- 99.99% availability (cluster)
- Graceful degradation in single-node mode

## Performance

- P99 object lookup <10 ms
- P99 vector search <50 ms
- Hybrid query <100 ms (benchmark dataset)

## Scalability

- Horizontal sharding
- Online rebalancing
- Pluggable storage backends

## Reliability

- ACID transactions
- Crash recovery
- Point-in-time recovery
- Zero committed data loss

## Security

- TLS
- Encryption at rest
- Object-level ACL
- Immutable audit log

## Maintainability

- RFC-driven development
- >90% unit coverage
- Property-based testing
- Fuzz testing
- Stable public APIs

## Observability

- OpenTelemetry
- Structured logging
- Metrics
- Distributed tracing

---

# 8. Acceptance Criteria

Architecture is accepted only if:

- Every component has a single responsibility.
- All dependencies follow the defined direction.
- Every subsystem exposes a stable interface.
- No subsystem bypasses the Knowledge Kernel.
- Every feature maps to one or more MRFCs.
- Every public API has conformance tests.
- All plugins compile against stable extension interfaces.
- Benchmark suite passes target thresholds.
- Security review completed.
- Architecture Decision Records exist for major design choices.

---

# 9. Traceability

| Component | Primary RFC |
|-----------|-------------|
| Knowledge Kernel | MRFC-0001-0004 |
| Storage Kernel | MRFC-0005+ |
| Transactions | MRFC-0011+ |
| Query Engine | MRFC-0020+ |
| KVM | MRFC-0030+ |
| Cluster | MRFC-0050+ |
| Security | MRFC-0060+ |

Expose relate/traverse via MCP and Python next task


claude --resume 5d64fb8c-856e-4840-967e-baa9f1233565