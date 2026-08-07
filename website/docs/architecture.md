---
title: Architecture
description: The Knowledge Operating System architecture
---

# Architecture

## The Knowledge OS Stack

Mnemosyne is organized as a layered operating system for knowledge:

```
┌──────────────────────────────────────────────┐
│           ACTIVE KNOWLEDGE OBJECTS            │
│  Program · Workflow · Agent · Policy          │
│  Prompt · Trigger · Connector · Benchmark     │
├──────────────────────────────────────────────┤
│           KNOWLEDGE RUNTIME                   │
│  Compiler → KVM · Orchestrator · Policy Engine│
├──────────────────────────────────────────────┤
│           KNOWLEDGE KERNEL                    │
│  MVCC · OCC · HLC · RBAC · Audit · CDC        │
├──────────────────────────────────────────────┤
│           STORAGE KERNEL                      │
│  redb · EncryptedStore                        │
└──────────────────────────────────────────────┘
```

## Core Design Principle

> **Everything is a Knowledge Object.**

Inspired by three landmark systems:

| System | Abstraction | Everything is a... |
|---|---|---|
| Git | Object | Commit, Blob, Tree, Tag |
| Kubernetes | Resource | Deployment, Service, ConfigMap |
| Unix | File | Data, Device, Socket, Process |
| **Mnemosyne** | **Knowledge Object** | Data, Program, Policy, Agent, Trigger |

A Knowledge Object has:
- **Identity** — immutable KOID
- **Versioning** — MVCC, every change is a new version
- **Provenance** — who created it, when, why
- **Access Control** — who can read/write/execute it
- **Dependencies** — which schemas, programs, ontologies it depends on
- **Events** — every mutation is a KnowledgeEvent
- **Audit Trail** — SHA-256 hash chain, independently verifiable

## Knowledge Kernel

### Transaction Pipeline

```
RememberRequest → Validation → OCC Check → HLC Assignment → Write Batch → Journal → Ack
```

- **MVCC** — Multi-Version Concurrency Control. Readers never block writers.
- **OCC** — Optimistic Concurrency Control. Conflicts detected deterministically.
- **HLC** — Hybrid Logical Clock. Causally consistent timestamps without NTP dependency.
- **SHA-256 Audit Chain** — Every commit extends the journal hash. Tamper-evident.

### Storage

- **redb** — Embedded ACID-compliant key-value store. Single file per database.
- **MemoryEngine** — In-memory engine for testing and ephemeral workloads.
- **EncryptedStore** — Wraps any StorageEngine with AES-256-GCM encryption.

### Event System (CDC)

Every mutation emits a `KnowledgeEvent`:
```
Create → Created event
Update → Updated event
Delete → Forgotten event
Lifecycle → Evolved event
```

Subscribers receive events via durable subscriptions with replay and checkpoint.

## Knowledge Runtime

### Compiler Pipeline

```
AIKOQL Source → Lexer → Parser → AST → Semantic Analyzer → KIR → Planner → Runtime
```

### Planner Optimizations

1. **Filter Merge** — Consecutive Filters combined into one
2. **Filter Pushdown** — Filters pushed before expensive Search operators
3. **Scan Dedup** — Duplicate Scans on the same type removed (cross-program fusion)

### KVM — Knowledge Virtual Machine

```
Program KO (AIKOQL)
    ↓
Compiler → Knowledge IR (KIR)
    ↓
Planner → Optimized IR
    ↓
Interpreter → RowSet
```

v1 is a tree-walking interpreter. JIT compilation (Cranelift) and WASM support are post-1.0.

## Active Knowledge Objects (MRFC-0030)

4 tiers of executable artifacts, all KOs:

| Type | Purpose |
|---|---|
| `mnemosyne:program` | AIKOQL code as versioned KO |
| `mnemosyne:workflow` | DAG of programs |
| `mnemosyne:policy` | RBAC rule as KO |
| `mnemosyne:trigger` | Event → Condition → Action |
| `mnemosyne:agent` | AI agent with prompt + memory + tools |
| `mnemosyne:connector` | Import/export plugin definition |

Every Active KO shares the same lifecycle as data: identity, versioning, provenance, access control, audit.

## Encryption (MRFC-0020)

```
Application Encryption (optional)
    ↓
Knowledge Encryption (field/object level)
    ↓
Storage Encryption (page/WAL level)
    ↓
Disk Encryption (OS-provided)
```

- **AES-256-GCM** — Primary cipher. Cipher-cached for performance (16.6% overhead).
- **ChaCha20-Poly1305** — Secondary cipher for crypto agility.
- **Envelope Encryption** — Key Encryption Key wraps per-tenant Data Encryption Keys.
- **Key Rotation** — Online rotation without data re-encryption.
- **Field-Level** — Mark specific properties as encrypted per schema type.

## Protocol Surface

| Entry Point | Protocol | Use Case |
|---|---|---|
| `mnemosyne serve` | MCP (JSON-RPC) over stdio/TCP | AI agents |
| `:9091/api/v1/*` | REST (HTTP/JSON) | Web apps, curl |
| `:9091/ui` | Graph Browser (vis-network) | Human exploration |
| `mnemosyne shell` | Interactive REPL | Human queries |
| `:9091/health` | HTTP health check | Kubernetes probes |
| `:9091/metrics` | Prometheus text format | Monitoring |

## Crate Map

```
crates/
├── kernel/           Knowledge Kernel (MVCC, OCC, HLC, RBAC, audit)
├── compiler/         AIKOQL parser, semantic analyzer, planner
├── runtime/          Physical plan interpreter
├── engines/
│   ├── graph/        Relationship index + BFS traversal
│   ├── vector/       HNSW + Tantivy (BM25) hybrid search
│   └── scheduler/    Background jobs (index, compaction, rotation)
├── services/
│   ├── api/mcp/      MCP server, REST API, Graph Browser UI
│   ├── reasoning/    If-then rule engine
│   ├── semantic/     AI embedding enrichment
│   └── ingestion/    Document ingestion plugin SDK
├── connectors/
│   ├── postgres/     PostgreSQL import
│   ├── sqlite/       SQLite import
│   ├── mongodb/      MongoDB import
│   └── neo4j/        Neo4j import
├── sdk/
│   ├── python/       PyO3 native bindings
│   ├── typescript/   MCP JSON-RPC client
│   ├── go/           TCP JSON-RPC client
│   └── java/         Zero-dependency JSON-RPC client
└── cluster/proxy/    Multi-shard proxy with retry/backoff
```

## Dependencies

**Zero external runtime dependencies.** The binary is a single self-contained file:
- Windows: 3.4 MB (PE32+ x86-64)
- Linux: 3.7 MB (ELF64 static musl, no glibc)
- Embedded database (redb), no external DB server required
