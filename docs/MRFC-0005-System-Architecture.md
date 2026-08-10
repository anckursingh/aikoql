
# MRFC-0005: Mnemosyne System Architecture

**RFC ID:** MRFC-0005
**Status:** Draft
**Version:** 1.0
**Category:** Foundational Architecture Standard

## Purpose

This document is the canonical architecture specification for Mnemosyne. It defines the architectural layers, responsibilities, dependency rules, execution flows, workspace layout, and governance rules. Every RFC, crate, module, and pull request SHALL conform to this specification.

## Current Implementation Status (2026-08-05)

Phase 1 (Trustworthy Memory Substrate) is **complete**. The system ships as an embedded library + MCP server with 6 crates, 11,400 lines of Rust, and 128 active tests. The Knowledge Kernel and two Knowledge Services exist; Compiler, Runtime, API Layer, and Storage Kernel split are future.

| Layer | Status | What exists |
|-------|--------|-------------|
| Applications | ⚠️ Partial | MCP server (14 tools, stdio), Python SDK (PyO3 + LangGraph/CrewAI) |
| API Layer | ❌ Future | No auth gateway, no protocol translation layer |
| Compiler Layer | ❌ Phase 3 | No parser, IR, planner, optimizer |
| Runtime Layer | ❌ Phase 3 | No KVM, executor — MCP tools call kernel directly |
| Knowledge Services | ⚠️ 3 of 10 | Graph ✅, Vector ✅, Indexing (partial via scheduler). Reasoning, Semantic, OCR, NER, Embedding, Ontology, Ingestion: future |
| Knowledge Kernel | ⚠️ 4 of 8 managers | AuthManager ✅, SchemaRegistry ✅, EventManager ✅, IndexCoordinator ⚠️. Object/Relationship/Lifecycle/Subscription: embedded in orchestrator. Kernel Scheduler: in `mnemosyne-scheduler` |
| Storage Kernel | ⚠️ In kernel crate | MVCC ✅, StorageEngine trait ✅, WAL/Recovery/Checkpoint/Buffer/Compression/Encryption: future |
| Physical Storage | ✅ | redb (durable, embedded), MemoryEngine (test) |

**Active crates (6):** `mnemosyne-kernel`, `mnemosyne-graph`, `mnemosyne-vector`, `mnemosyne-scheduler`, `mnemosyne-mcp`, `mnemosyne-py` (Python SDK).

**Key architectural pattern:** Kernel defines traits (`StorageEngine`, `VectorIndex`, `TextIndex`, `IndexMaintainerApi`). Engine/Service crates provide implementations behind those traits. The kernel never depends on engine crates at runtime. Tests use dev-dependencies.

**Superseded documents** (archived in `docs/archive/`): HLD.md, PRD.md, ARCHITECTURE-REVIEW.md, knowledge-kernel-review.md, PHASE-1-MILESTONE-{1,2,3}.md.

## Vision

Mnemosyne is a **Knowledge Computing Platform**, not merely a multi-model database. The canonical abstraction is the **Knowledge Object (KO)**. Rows, graphs, vectors, documents, and events are representations or projections of knowledge rather than primary storage abstractions.

## Architectural Principles

1. Knowledge Object is the canonical source of truth.
2. Downward-only dependencies.
3. Compiler and execution are separate.
4. Runtime and Kernel are separate.
5. Knowledge semantics and storage semantics are separate.
6. Every mutation emits a Knowledge Event.
7. Historical knowledge is immutable.
8. AI services are plugins around the kernel, not inside it.
9. Physical storage is replaceable.
10. Stable interfaces between layers.

## Layered Architecture

```text
Applications
    ↓
API Layer
    ↓
Compiler Layer
    ↓
Runtime Layer
    ↓
Knowledge Services
    ↓
Knowledge Kernel
    ↓
Storage Kernel
    ↓
Physical Storage
```

### Applications
Responsibilities:
- CLI
- SDKs
- AI Agents
- Desktop UI
- MCP
- Session management

Forbidden:
- Direct storage access
- Direct kernel access

### API Layer
Responsibilities:
- Authentication
- Authorization
- Validation
- Protocol translation
- Rate limiting

Protocols:
REST, gRPC, GraphQL, AIKOQL, WebSocket

### Compiler Layer
**Phase:** 3 (post-Knowledge-Services). No code exists. The Knowledge IR requires at least two query frontends to justify its existence; currently only MCP exists.

Modules:
- Parser
- Semantic Analyzer
- Knowledge IR
- Planner
- Optimizer
- Bytecode Generator *(post-1.0: physical-plan interpreter first; bytecode when profiling justifies — per Architecture Review R11)*

Responsible only for compilation.

### Runtime Layer
**Phase:** 3 (post-Compiler). No code exists. MCP tools call kernel syscalls directly; the kernel IS the runtime today.

Modules:
- Knowledge VM *(post-1.0: physical-plan interpreter first; KVM when profiling justifies — per Architecture Review R11)*
- Executor
- Runtime Scheduler
- Worker Pool
- Execution Context

Responsible only for execution.

### Knowledge Services
**Phase:** 2 (in progress). Services consume Knowledge Events and enrich knowledge asynchronously. Implementation status per service:

| Service | Status | Crate |
|---------|--------|-------|
| Graph | ✅ Done | `mnemosyne-graph` — relate, traverse with relationship indexes |
| Vector | ✅ Done | `mnemosyne-vector` — HNSW ANN, Tantivy BM25 behind kernel traits |
| Indexing | ⚠️ Partial | `mnemosyne-scheduler` — IndexMaintainer does KE-driven async maintenance |
| Reasoning | ❌ Phase 3 | Rule execution, ontology, inference |
| Semantic | ❌ Phase 3 | NER, summarization, classification |
| Embedding | ❌ Phase 3 | Background embedding generation via scheduler |
| OCR | ❌ Phase 4+ | Document ingestion — plugin, not core |
| Ingestion | ❌ Phase 4+ | Multimodal input pipeline — plugin, not core |
| NER | ❌ Phase 3 | Named entity recognition |
| Ontology | ❌ Phase 4+ | Ontology management and alignment |

### Knowledge Kernel
Managers:
- Object Manager
- Relationship Manager
- Event Manager
- Lifecycle Manager
- Schema Manager
- Authorization Manager
- Subscription Manager
- Kernel Scheduler

Owns:
- KO
- KR
- KE
- Views
- Provenance
- Lifecycle
- Security

Forbidden:
- OCR
- LLM
- Embedding generation
- Vector search

### Storage Kernel
Modules:
- MVCC
- WAL
- Recovery
- Checkpoint
- Buffer Manager
- Storage Interface
- Compression
- Encryption

Owns storage correctness only.

### Physical Storage
Examples:
- RocksDB
- Native Engine (future)

Stores bytes only.

## Canonical Request Flow

Application
→ API
→ Compiler
→ Knowledge IR
→ Planner
→ Optimizer
→ Bytecode
→ Knowledge VM
→ Knowledge Kernel
→ Storage Kernel
→ Storage Engine

## Query Flow

AIKOQL/SDK
→ Parser
→ KIR
→ Planner
→ Optimizer
→ KVM
→ Knowledge Kernel
→ Knowledge Services
→ Graph / Vector / Reasoning
→ Response

## Document Ingestion Flow

PDF/Image
→ Ingestion Service
→ OCR
→ Layout Detection
→ Table Extraction
→ Entity Extraction
→ Relationship Extraction
→ KO Builder
→ Knowledge Kernel
→ Commit
→ Event Bus
→ Embedding / Graph / Index / Reasoning

## Knowledge Event Flow

Commit
→ Knowledge Event
→ Subscription Manager
→ Event Bus
→ Knowledge Services

## Scheduler Model

*Current state: one `IndexMaintainer` in `mnemosyne-scheduler` handles re-indexing and catch-up. Decomposes into three schedulers when workload diversity demands it (Phase 3+).*

Kernel Scheduler *(Phase 3)*:
- Cleanup
- MVCC maintenance
- Internal housekeeping

Runtime Scheduler *(Phase 3)*:
- Worker scheduling
- Parallel execution

Knowledge Scheduler *(Phase 2 — partial)*:
- Re-indexing ✅ (IndexMaintainer)
- Embeddings (Phase 3)
- AI workflows (Phase 3)
- OCR (Phase 4+)

## Dependency Rules

Allowed:
Applications → API → Compiler → Runtime → Knowledge Kernel → Storage Kernel → Storage

Forbidden:
- Services → Storage
- Runtime → RocksDB
- Compiler → Services
- Applications → Kernel
- Storage Kernel → AI Services

## Workspace Layout

```text
mnemosyne/
├── crates/
│   ├── compiler/
│   ├── runtime/
│   ├── knowledge-kernel/
│   ├── storage-kernel/
│   ├── services/
│   ├── cluster/
│   ├── bindings/
│   ├── cli/
│   ├── integration-tests/
│   └── benchmarks/
├── docs/
├── rfcs/
└── tools/
```

## Architecture Governance

Every feature proposal SHALL identify:
- Owning layer
- Public API
- Data ownership
- Events emitted
- Events consumed
- Dependency direction
- Kernel invariants affected
- Performance impact
- Security impact

Changes violating this architecture SHALL be rejected.
