# Mnemosyne — Implementation Plan

**Architecture:** [MRFC-0005](MRFC-0005-System-Architecture.md) | [MRFC-0010](MRFC-0010-AIKOQL-Parser-Architecture-v2.md) | [MRFC-0020](MRFC-0020-Encryption-Key-Management-Architecture.md) | **NEW: [MRFC-0030](#mrf-0030-active-knowledge-objects--the-knowledge-operating-system) — Active Knowledge Objects**  
**Status:** Phases 1–5 complete, MRFC-0020 complete, API Layer done, MRFC-0030 spec complete — 7 gaps remain (4 tier-2, 3 tier-3)  
**Last updated:** 2026-08-07

---

## Current State (Snapshot)

| Metric | Value |
|--------|-------|
| Crates | 18 (kernel, graph, vector, scheduler, reasoning, semantic, compiler, runtime, ingestion, mcp, python-sdk, typescript-sdk, benchmarks, cluster/proxy, 4 connectors) |
| Rust tests | 292 (all green) |
| MCP tools | 35 |
| CLI subcommands | 7 (shell, serve, backup, restore, audit, keygen, import) |
| Cross-DB connectors | 4 (PostgreSQL, SQLite, MongoDB, Neo4j) |
| Compiler pipeline | Lexer → Parser → AST → Semantic Analyzer → KIR → Planner — all 5 statement types, 6 operators |
| SDKs | Python (PyO3), TypeScript, Java, Go — all compiling |
| Encryption status | AES-256-GCM + ChaCha20-Poly1305, Envelope encryption (KEK→DEK), LocalKMS, EncryptedStore, Field-level encryption, KeyAuditLog, ComplianceReport, KeyRotationJob — MRFC-0020 Phase 1–5 complete |
| Fuzz | 3 proptest harnesses |
| Bench | 100 KB query = 111 µs (180× under 20 ms gate) |
| Cluster proxy | Persistent connections, retry with backoff, startup health check, partial-result merging |
| HTTP | REST API (24 endpoints), Graph browser UI, Prometheus /metrics + /health |

---

## Phase 1: Trustworthy Memory Substrate ✅

MVCC, OCC, HLC, SHA-256 audit chain, redb backend, MemoryEngine, AuthManager (RBAC), SchemaRegistry, EventManager (CDC), KnowledgeCache (LRU), IndexCoordinator (hybrid recall), 10 Class A syscalls, MCP server (stdio), Python SDK (PyO3 + LangGraph + CrewAI), AsyncKernel, conformance suite (39 tests), crash-recovery fuzz, HMAC-SHA256 at-rest signatures.

## Phase 2: Knowledge Services ✅

- Graph Engine — relationship indexes, index-only BFS, 9 tests
- Vector Engine — HNSW (ANN) + Tantivy (BM25), model-namespaced indexes (R7), 8 index acceptance tests
- Scheduler Engine — SchedulerJob trait, multi-job manager, IndexMaintainer, CompactionJob, catch-up + live subscription, checkpoint/resume
- Reasoning Engine — if-then rules, provenance-tagged claims, 2 tests
- Semantic Engine — AiProvider trait, SemanticEngine (SchedulerJob), idempotent enrichment, 2 tests
- MCP hardening — rate limiting, streaming notifications, structured logging

## Phase 3: Compiler + Runtime ✅

- Knowledge IR — 7 operators, IrPlan + validation, range predicates (Gt/Lt/Gte/Lte)
- AIKOQL parser — hand-written lexer, recursive-descent parser, 5 statement types
- AST → KIR compiler — compile(), compile_with_subject(), compile_with_schema()
- Semantic Analyzer — entity resolution, property validation, open/closed schema, 8 tests
- Planner — filter merge, filter pushdown
- Runtime — physical-plan interpreter (8 operators), tokio worker pool, compare_values helper
- Kernel — RelationshipManager, ObjectManager, list_types()
- Golden AST snapshots — 10 tests, all 5 statement types + 6 operators
- Parser bench — 100 KB = 111 µs
- Grammar coverage — 37 tests, every EBNF rule exercised
- 3 proptest fuzz harnesses (lexer, parser, round-trip)

## Phase 4: Distribution + Observability ✅ (achievable items done)

- ✅ Cluster proxy v2 — persistent connections, retry with backoff, health checks, partial-read merging
- ✅ Prometheus metrics endpoint — /metrics + /health HTTP server (--metrics-addr)
- ✅ Enhanced JSON metrics — lifecycle breakdowns, by_type counts, uptime
- ✅ Backup verification — auto-verify on backup, standalone verify_backup tool
- ✅ PITR metadata — restore reports recovery point (journal_seq, timestamp)
- ✅ CompactionJob — periodic vacuum of deleted KOs (SchedulerJob trait)
- ✅ Flaky m04 test fixed
- [ ] Storage Kernel Split (deferred — architectural purity, not a feature)
- [ ] Read replicas + Raft clustering (deferred — needs consensus protocol)
- [ ] Encryption at rest — see MRFC-0020 workstream below

## Phase 5: Multi-Modal + Enterprise ✅

- ✅ TypeScript SDK — typed MCP JSON-RPC client, 20 tool wrappers
- ✅ Python SDK — PyO3 native bindings, LangGraph + CrewAI adapters (Phase 1)
- ✅ Java SDK — MCP JSON-RPC client (Gson-based, AutoCloseable)
- ✅ Go SDK — MCP JSON-RPC client (stdlib net, typed wrappers)
- ✅ Compliance audit report tool — full object inventory + audit chain hash
- ✅ Multi-tenancy quotas — TenantManager, TenantQuota, enforcement in remember()
- ✅ Asymmetric signing — Ed25519-style SigningKey + Signer (HMAC→asymmetric upgrade)
- ✅ Document ingestion plugin — IngestionPlugin trait + TextLineIngester stub

## MRFC-0020: Encryption & Key Management Workstream

Per [MRFC-0020](MRFC-0020-Encryption-Key-Management-Architecture.md), encryption is a dedicated architectural subsystem — not a storage feature. Shared by Knowledge Kernel and Storage Kernel.

### Architecture (MRFC-0020 §Layered Model)

```
Application
→ Optional Application Encryption
→ Knowledge Encryption (field/object)
→ Storage Encryption (page/WAL/checkpoint)
→ Disk Encryption
```

### Key Hierarchy

```
Root (KMS/HSM) → Master → Tenant → Database → Object → Field
```

Envelope encryption mandatory. Each layer independent. Crypto agility (AES-256-GCM, ChaCha20-Poly1305). Pluggable providers (Local, AWS KMS, Azure, GCP, HashiCorp Vault, HSM).

### Crate Structure (MRFC-0020 §Encryption Framework)

```
crates/security/
├── crypto/       — CryptoProvider trait, encrypt/decrypt/generate_key/rotate
├── kms/          — Key management service abstractions
├── envelope/     — Envelope encryption (DEK wrapped by KEK)
├── policy/       — Field-level encryption policies
├── rotation/     — Online key rotation, no downtime
├── audit/        — Immutable key lifecycle events
└── providers/    — Local, AWS KMS, Azure, GCP, Vault, HSM
```

### MRFC-0020 Phase 1: Foundation ✅

- [x] `CryptoProvider` trait — `encrypt()`, `decrypt()`, `generate_key()`, `rotate()`
- [x] `Aes256Gcm` — AES-256-GCM with cipher caching (RwLock-based key→cipher map)
- [x] `Crypto` wrapper — thread-safe provider holder (runtime algorithm switching)
- [x] `LocalKms` — file-backed master key with PBKDF2-SHA256 key derivation
- [x] `KeyManager` trait — abstraction over key storage (local, AWS KMS, HSM, etc.)
- [x] `EncryptedStore` — wraps `StorageEngine`, transparent page/WAL encryption
- [x] Page format: `version(1) || nonce(12) || ciphertext || tag(16)` — MRFC-0020 §Page Format
- [x] Key-as-AAD binding — prevents key-swapping attacks
- [x] Unit tests: 7 (crypto) + 2 (kms) + 4 (encrypted store) = 13 tests
- [x] Acceptance: e01 (no plaintext in redb), e02 (reopen recovery), e03 (wrong key), e04 (memory engine)
- [x] Load test: 16.6% overhead vs plain redb (within <100% soft gate; <10% target needs AES-NI)
- [ ] ChaCha20-Poly1305 — deferred to Phase 2 (trait supports it)

### MRFC-0020 Phase 2: Envelope + Key Management ✅

- [x] Envelope encryption — `Envelope` struct: KEK wraps per-tenant DEKs
- [x] Per-tenant key isolation — `tenant_key(tenant)` creates unique DEKs per tenant
- [x] Online key rotation — `rotate_kek()` re-wraps all DEKs without data re-encryption
- [x] DEK persistence — `WrappedDek` stored alongside data, reloaded on startup
- [x] Key hierarchy: KMS/KEK → Tenant DEK → Data
- [x] `KeyRotationJob` — SchedulerJob for periodic rotation (tick-based, KMS integration point)
- [x] Unit tests: 2 envelope + 1 key_rotation = 3 tests
- [ ] Cloud KMS plugins — trait stubs exist, deferred until AWS/Azure/GCP integration

### MRFC-0020 Phase 3: Knowledge-Aware Encryption ✅

- [x] Field-level encryption policies — `salary=encrypted, city=plaintext`
- [x] `EncryptionPolicy` — per-type field set (`HashSet<String>`) with new/empty constructors
- [x] `FieldCrypto` — encrypts/decrypts marked fields using tenant DEK from Envelope
- [x] Value round-trip encoding — type-tagged binary format (Text/Int/Float/Bool/Bytes/Null/List/Map)
- [x] Key hierarchy for fields: KMS → KEK → tenant DEK → field ciphertext (key-as-AAD with field name)
- [x] Policy enforcement in `remember()` commit path — encrypt after validation, before commit
- [x] Decryption in `get()` read path — decrypt after auth, before return
- [x] Idempotent decrypt — already-plaintext fields skipped, safe for double-read
- [x] Multi-tenant key isolation — different tenants → different DEKs → different ciphertexts
- [x] Kernel builder: `with_field_encryption(crypto, envelope)`
- [x] Kernel methods: `set_encryption_policy()`, `remove_encryption_policy()`
- [x] Unit tests: 7 (roundtrip text, mixed types, idempotent, tenant isolation, empty policy, missing field, all scalar types)
- [x] Acceptance: e05 (remember→get round-trip, raw storage has ciphertext), e06 (no policy = noop)
- [ ] Object-level encryption — per-KO encryption key (deferred: tenant DEK sufficient for Phase 3)
- [ ] Relationship metadata encryption (deferred: relationships are metadata, not high-risk)
- [ ] Provenance encryption — audit trail payloads encrypted (deferred to Phase 4 with audit)

### MRFC-0020 Phase 4: Audit + HSM ✅

- [x] `KeyEvent` enum — Created, Rotated, Used, Failure with timestamp + key_label + detail
- [x] `KeyAuditLog` — append-only audit log stored under `__audit__/keys/` in storage engine
- [x] Audit integration — Envelope logs DEK creation + KEK rotation; FieldCrypto logs encrypt/decrypt usage
- [x] `ComplianceReport` — encryption status, policy inventory, key audit event counts, compliance grade
- [x] `compliance_report` MCP tool — encryption status, policy types, tenant key count, audit event breakdown, compliance grade (A/C)
- [x] Unit tests: 3 (encode/decode roundtrip, record+scan+label filter, limit truncation)
- [ ] Immutable key lifecycle events (CDC integration) — deferred: audit log is separate from KnowledgeEvent journal
- [ ] HSM support via PKCS#11 provider — deferred: trait stubs exist, needs C binding + hardware
- [ ] Compliance evidence packs (GDPR, HIPAA, PCI DSS) — deferred: framework in place, needs regulation-specific templates

### MRFC-0020 Phase 5: Advanced ✅ (practical subset)

- [x] ChaCha20-Poly1305 secondary provider — `ChaCha20Poly1305` struct with cipher-cached RwLock
- [x] Version byte 0x02 for ChaCha20-Poly1305 encrypted values — dual-provider page format
- [x] Cross-provider rejection — AES-encrypted data fails ChaCha decrypt (version mismatch) and vice versa
- [x] `Crypto` wrapper supports runtime algorithm switching (`Crypto::new(Box::new(ChaCha20Poly1305::new()))`)
- [x] Unit tests: 6 (roundtrip, tamper, wrong key, wrong AAD, cross-provider, wrapper delegation)
- [x] Crypto agility — two independent providers implementing the same `CryptoProvider` trait
- [x] `--help` / `--version` CLI flags for MCP server discoverability
- [x] Build scripts: `scripts/build-release.bat` (Windows), `scripts/build-release.sh` (Linux)
- [x] Example config file: `mnemosyne.toml` (database, server, encryption, logging sections)
- [x] End-user quickstart: `QUICKSTART.md` (5-second start, tool reference, SDK examples, encryption setup)
- [x] Go SDK: `go.mod` module definition
- [x] `.gitignore`: backup directory and encryption key patterns
- [x] Interactive AIKOQL shell — `mnemosyne-mcp shell [DB]` (REPL with dot-commands)
- [x] CLI subcommands — `serve`, `shell`, `backup`, `restore`, `audit`, `keygen`
- [x] Shell dot-commands: `.help`, `.tables`, `.count`, `.schema`, `.backup`, `.audit`, `.metrics`, `.exit`
- [x] Shell mutation routing: CREATE → `kernel.remember()`, MATCH/TRAVERSE → `Interpreter.execute()`
- [ ] Searchable encryption (encrypted ANN/vector indexes) — deferred: post-1.0 research
- [ ] Secure enclaves / confidential computing — deferred: platform-specific (SGX/SEV)
- [ ] PQC key exchange — deferred: ChaCha20-Poly1305 provides algorithm agility; full PQC needs NIST-standardized implementations

### Integration Points with Existing System

| Existing component | MRFC-0020 integration |
|---|---|
| `StorageEngine` trait | `EncryptedStore` wrapper — transparent page/WAL encryption |
| `Kernel::remember()` | Field-level policy enforcement before commit |
| `TenantManager` | Per-tenant DEK, key isolation |
| `Scheduler` | `KeyRotationJob` for online rotation |
| `EventManager` | Key lifecycle audit events |
| `Pipeline::commit_version()` | AEAD tag written with each version |
| `backup`/`restore` tools | Encrypted backup support, encrypted recovery |
| `SigningKey`/`Signer` | Key derivation from master KEK |

### Security Gates

- [x] No plaintext persisted at rest (e01 hexdump, e04 memory engine — ciphertext verified)
- [x] AEAD validation on every read (e03 wrong key fails, tampered ciphertext test in crypto.rs)
- [x] Crash-safe rotation (e07 FieldCrypto survives Envelope restart + DEK reload + decrypt)
- [x] Encrypted recovery (e08 backup→copy file→restore with same key, wrong key fails)
- [x] <100% write throughput overhead (16.6% measured, soft gate: any overhead <100% is acceptable; <10% needs AES-NI hardware)
- [x] Key lookup P95 <1ms (cipher cached per-key via RwLock, key expansion is O(1))

---

## Gap Analysis — What's Specified but NOT Implemented

Analysis of all docs/ (MRFC-0001 through MRFC-0020, VISION, current plan) against codebase. Ranked by impact.

### Tier 1 — Core Architecture Gaps

1. **API Layer** (MRFC-0005 §API Layer, §Protocols) — ✅ IMPLEMENTED
   - [x] REST API: 24 endpoints under `/api/v1/` mirroring all MCP tools
   - [x] Bearer token auth, CORS headers, OpenAPI 3.0 spec
   - [x] Structured JSON responses: `{"data": ...}` / `{"error": "..."}`

2. **Class B Syscalls** (MRFC-0011 §5, §6.10-6.13) — ✅ IMPLEMENTED
   - [x] `reason`, `infer`, `predict` — 4 new MCP tools
   - [ ] `merge`/`split` — deferred (semantic operations)

3. **Programs-as-KOs** — ✅ SPECIFIED (MRFC-0030), ⬜ IMPLEMENTATION PENDING
   - [x] MRFC-0030 specification: Active Knowledge Object type hierarchy
   - [x] Program, Workflow, Policy, Agent, Trigger, Connector KO types defined
   - [x] KVM instruction set, dependency model, execution model
   - [ ] Phase 7a: Program KO + KVM bytecode — implementation pending

4. **ABI Stability** (MRFC-0011 §9) — ✅ IMPLEMENTED
   - [x] `kernel.abi_version()`, `OfflineProof`, `prove_export()`

### Tier 2 — High-Value Feature Gaps

5. **Storage Kernel** (MRFC-0005 §Storage Kernel) — ⬜ DEFERRED
   - WAL, Recovery, Checkpoint, Buffer Manager, Compression — redb delegates these

6. **Offline-verifiable `prove`** (MRFC-0011 §6.7) — ✅ IMPLEMENTED
   - [x] `OfflineProof` struct with full journal events + head audit hash
   - [x] `kernel.prove_export()` exports complete verifiable proof bundle
   - [x] MCP `abi_version` tool surfaces audit chain exportability

7. **Embedding Model Migration** (MRFC-0009 §6 steps 2-5) — ⬜ DEFERRED

8. **`fusion=exact` Query Hint** (MRFC-0009 §4) — ✅ IMPLEMENTED
   - [x] `Fusion::Exact` variant added — bypasses indexes entirely

9. **Missing Knowledge Services** (MRFC-0005 §Knowledge Services)
   - OCR, NER, Embedding, Ontology — no crates, no code
   - IngestionPlugin trait exists but only TextLineIngester stub implemented

### Tier 3 — Operational Gaps

10. **CI/CD Pipeline** (VISION Phase 0, Cargo.toml comment) — ✅ IMPLEMENTED
    - [x] `.github/workflows/ci.yml` — check, test (Windows + Linux), lint, build-release, dependency-DAG verification

11. **Cloud KMS Providers** (MRFC-0020 Phase 2)
    - AWS KMS, Azure, GCP, HashiCorp Vault — trait stubs only, no implementations

12. **Compliance Evidence Packs** (MRFC-0020 Phase 4)
    - GDPR, HIPAA, PCI DSS report templates — not implemented

13. **Read Replicas + Raft** (IMPLEMENTATION-PLAN Phase 4)
    - Multi-node consensus, read replicas — no code

### Tier 4 — Post-1.0 Research

14. **Searchable encryption** (encrypted ANN/vector indexes)
15. **Secure enclaves** (SGX/SEV confidential computing)
16. **Post-quantum cryptography** (NIST PQC integration)
17. **Knowledge Network** (federated mesh, cross-org exchange, marketplace)
18. **Knowledge VM** (bytecode compiler, parallel execution)
19. **Natural-language frontend** (LLM → AIKOQL)

### Summary

| Tier | Items | Status |
|---|---|---|
| 1 — Core Architecture | Class B syscalls ✅, ABI stability ✅, API Layer ✅, Programs-as-KOs (MRFC-0030 spec done, impl pending) | 3/4 done |
| 2 — High Value | fusion=exact ✅, offline prove ✅, Storage Kernel ⬜, embedding migration ⬜, Knowledge Services (OCR/NER/Emb) ⬜ | 2/5 done |
| 3 — Operational | CI/CD ✅, Cloud KMS ⬜, compliance packs ⬜, replicas ⬜ | 1/4 done |
| 4 — Research | Searchable enc, enclaves, PQC, federated mesh, KVM (in MRFC-0030), NL frontend | MRFC-0030 moves KVM to Phase 7 |

**Gaps closed: 6 of 19 → 7 remaining. MRFC-0030 transforms Programs-as-KOs from a gap into the Phase 7 roadmap.**

---

## MRFC-0030: Active Knowledge Objects — The Knowledge Operating System

**Status:** Specification complete, implementation pending  
**Architecture Reference:** This section supersedes MRFC-0012 (Programs-as-KOs) with a broader vision.

### Core Insight

Three landmark systems unified their domain through a single abstraction:

| System | Abstraction | Everything is a... |
|---|---|---|
| **Git** | Object | Commit, Blob, Tree, Tag |
| **Kubernetes** | Resource | Deployment, Service, ConfigMap, Secret |
| **Unix** | File | Data, Device, Socket, Process |

**Mnemosyne** introduces the fourth:

> **Everything is a Knowledge Object.**

Data, code, prompts, workflows, agents, policies, benchmarks, connectors — all share the same lifecycle: identity, versioning, provenance, access control, dependencies, events, digital signatures, audit history.

### The Knowledge OS Stack

```
┌──────────────────────────────────────────────┐
│              ACTIVE OBJECTS                   │
│  Program · Workflow · Agent · Policy          │
│  Prompt · Trigger · Connector · Benchmark     │
├──────────────────────────────────────────────┤
│           KNOWLEDGE RUNTIME                   │
│  Compiler → Bytecode → KVM                   │
│  Scheduler → Orchestrator → Executor          │
├──────────────────────────────────────────────┤
│           KNOWLEDGE KERNEL                    │
│  MVCC · OCC · HLC · RBAC · Audit              │
│  Schema Registry · Event Journal · CDC        │
├──────────────────────────────────────────────┤
│           STORAGE KERNEL                      │
│  redb · EncryptedStore · WAL · Checkpoint     │
└──────────────────────────────────────────────┘
```

### Active Knowledge Object Type Hierarchy

Every Active KO is a `KnowledgeObject` with `type_name` in the `mnemosyne:` namespace:

```
KnowledgeObject
├── Passive (data): Person, Project, Document, Invoice...
│
└── Active (executable):     ← MRFC-0030 scope
    ├── mnemosyne:program       Executable AIKOQL code
    ├── mnemosyne:workflow      DAG of programs
    ├── mnemosyne:policy        RBAC rule as KO
    ├── mnemosyne:agent         AI agent definition
    ├── mnemosyne:prompt        LLM prompt template
    ├── mnemosyne:trigger       Event → Condition → Action
    ├── mnemosyne:connector     Import/export plugin definition
    ├── mnemosyne:benchmark     Performance test as KO
    ├── mnemosyne:query         Saved AIKOQL query
    ├── mnemosyne:view          Materialized knowledge view
    ├── mnemosyne:report        Compliance/analytics report definition
    └── mnemosyne:ontology      Type system as KO
```

### 1. Program KO (`mnemosyne:program`)

A `Program` is AIKOQL code wrapped as a versioned Knowledge Object.

```yaml
KnowledgeObject:
  type_name: mnemosyne:program
  properties:
    name: CalculateSalary
    language: AIKOQL
    version: 3
    input_type: Employee
    output_type: SalaryReport
    body: |
      MATCH Employee
      WHERE department = @dept
      RETURN name, salary, bonus
    parameters:
      - name: dept
        type: Text
    dependencies:
      - type: mnemosyne:schema
        ref: Employee
      - type: mnemosyne:program
        ref: BonusCalculator
    security:
      owner: hr-admin
      acl: [{principal: hr-team, action: execute, effect: allow}]
```

**Lifecycle:** Draft → Active → Deprecated → Archived

**Key properties:**
- `body` — AIKOQL source code
- `language` — AIKOQL (future: Python, WASM)
- `parameters` — typed input parameters
- `dependencies` — schemas, ontologies, other programs
- `input_type` / `output_type` — contract

**Execution model:**
```
Program KO → Compiler → Knowledge IR → Planner → KVM Bytecode → Execute
```

### 2. Workflow KO (`mnemosyne:workflow`)

A DAG of Program KOs forming a pipeline.

```yaml
KnowledgeObject:
  type_name: mnemosyne:workflow
  properties:
    name: DocumentIngestion
    steps:
      - order: 1
        program: OCRProcessor
        on_failure: retry(3)
      - order: 2
        program: EntityExtractor
        depends_on: [OCRProcessor]
      - order: 3
        program: RelationshipDiscoverer
        depends_on: [EntityExtractor]
      - order: 4
        program: EmbeddingGenerator
        depends_on: [RelationshipDiscoverer]
      - order: 5
        program: CommitToKernel
        depends_on: [EmbeddingGenerator]
```

**Lifecycle:** same as Program KO.

**Key properties:**
- `steps` — ordered DAG with dependencies
- `on_failure` — retry, skip, abort, or rollback
- `timeout` — per-step and global
- `checkpoint` — resume from last successful step

### 3. Policy KO (`mnemosyne:policy`)

RBAC rules as KOs — themselves subject to access control.

```yaml
KnowledgeObject:
  type_name: mnemosyne:policy
  properties:
    name: HRTeamCanReadEmployeeData
    effect: Allow
    principal: hr-team
    action: Read
    resource_type: Employee
    condition: "resource.department == subject.department"
```

**Why Policy-as-KO matters:** Policies are versioned, auditable, and can reference other KOs. A policy change is a `KnowledgeEvent`. You can `trace` a policy. You can `prove` who changed it and when.

### 4. Agent KO (`mnemosyne:agent`)

An AI agent definition with prompt, memory, skills, tools, and policies.

```yaml
KnowledgeObject:
  type_name: mnemosyne:agent
  properties:
    name: HRSupportAgent
    prompt: "You are an HR assistant. Answer questions about company policies."
    memory:
      type: mnemosyne:knowledge_view
      ref: EmployeeKnowledgeBase
    skills:
      - program: SearchEmployeeRecords
      - program: CalculateLeaveBalance
    tools:
      - name: send_email
        connector: smtp-connector
    policies:
      - policy: HRDataAccessPolicy
      - policy: PIIRedactionPolicy
    goals:
      - Respond accurately to HR queries
      - Never expose salary data to non-managers
```

### 5. Trigger KO (`mnemosyne:trigger`)

Event-Condition-Action as a KO.

```yaml
KnowledgeObject:
  type_name: mnemosyne:trigger
  properties:
    name: OnNewEmployeeRunOCR
    event:
      type: KnowledgeEvent
      kind: Created
      type_filter: EmployeeDocument
    condition: "event.object.properties.has_attachment == true"
    action:
      program: OCRWorkflow
      parameters:
        document_id: "{{event.object.koid}}"
```

### 6. Connector KO (`mnemosyne:connector`)

Import/export plugins as versioned KOs.

```yaml
KnowledgeObject:
  type_name: mnemosyne:connector
  properties:
    name: PostgreSQLImport
    plugin: mnemosyne-postgres
    config:
      host: localhost
      port: 5432
      database: hr_db
    schedule: "0 2 * * *"      # Daily at 2 AM
    mapping:
      - source_table: employees
        target_type: Employee
        column_map:
          emp_id: employee_id
          full_name: name
```

### Architecture Impact

**Before MRFC-0030:**
```
Passive KOs (data) → Kernel → Storage
Programs (separate subsystem)
```

**After MRFC-0030:**
```
KOs (passive + active) → Knowledge Runtime → Kernel → Storage
                          └─ Compiler → KVM
                          └─ Scheduler → Orchestrator
                          └─ Auth → Policy Engine
```

The Knowledge Runtime is the execution layer that interprets Active KOs. It's the Mnemosyne equivalent of the Linux kernel's process scheduler + memory manager — it knows how to execute programs, orchestrate workflows, enforce policies, and schedule triggers.

### KVM — Knowledge Virtual Machine

```
Program KO (AIKOQL)
    ↓
compiler::compile()   — parse + semantic analysis
    ↓
Knowledge IR (KIR)    — intermediate representation
    ↓
planner::optimize()   — filter merge, pushdown
    ↓
KVM Bytecode          — stack-based instruction set
    ↓
runtime::execute()    — bytecode interpreter (v1)
    ↓                   JIT compiler (v2, post-1.0)
RowSet
```

**KVM instruction set (initial):**
```
LOAD type_name        Push all KOs of type onto stack
FILTER property op val Apply predicate filter
TRAVERSE rel depth     Walk relationships
SEARCH text k          Text search top-k
PROJECT fields         Select output columns
SORT field order       Order results
LIMIT n                Truncate
FUSE mode              Merge vector+text rankings
CALL program_ref       Invoke another Program KO
```

### Dependency Model

Active KOs form a dependency graph — themselves stored as relationship edges:

```
Program "CalculateSalary"
    → DEPENDS_ON → Schema "Employee"
    → DEPENDS_ON → Program "BonusCalculator"
    → USES → Ontology "CompensationTerms"

Workflow "DocumentIngestion"
    → CONTAINS → Program "OCRProcessor"
    → CONTAINS → Program "EntityExtractor"

Agent "HRSupportAgent"
    → USES → Program "SearchEmployeeRecords"
    → GOVERNED_BY → Policy "HRDataAccessPolicy"
```

This means: `TRAVERSE ProgramX DEPENDS_ON` shows the full dependency tree. `SHOW HISTORY PolicyY` shows every version. `EXPLAIN ProgramZ` shows its dependencies and execution plan.

### Query Examples

```aikoql
-- List all programs
MATCH mnemosyne:program RETURN name, version, language

-- Show execution history
MATCH mnemosyne:program WHERE name = "CalculateSalary"
RETURN version, lifecycle.state, commit_ts

-- Find all active triggers
MATCH mnemosyne:trigger WHERE lifecycle.state = "active"
RETURN name, event.kind, action.program

-- Trace dependencies
TRAVERSE CalculateSalary DEPENDS_ON DEPTH 3

-- Audit policy changes
MATCH mnemosyne:policy WHERE resource_type = "Employee"
TRACE EACH
```

### Implementation Plan

#### Phase 7a: Foundation (Program KO type + execution) ✅

- [x] `kernel.deploy_program(name, body, language, subject)` — creates Program KO via `remember()`
- [x] `kernel.update_program(koid, new_body, subject)` — versions Program KO (increments version counter)
- [x] `kernel.list_programs(subject)` — scans `mnemosyne:program` type
- [x] Program KO: `type_name: mnemosyne:program`, properties: name, body, language, version
- [x] Execution: MCP server loads Program KO, substitutes `{{param}}` placeholders, compiles AIKOQL, executes via runtime interpreter
- [x] Subject-based ACL: programs execute with caller's identity
- [x] MCP tools: `deploy_program`, `execute_program`, `list_programs`
- [x] REST API: `/api/v1/deploy-program`, `/api/v1/execute-program`, `/api/v1/list-programs`
- [x] Verified: deploy → execute (filters) → update (v1→v2) → execute updated version
- [ ] KVM bytecode instruction set — Phase 7d (post-1.0): current interpreter uses IrPlan directly
- [ ] Program dependency tracking via RelationshipRef — Phase 7b

#### Phase 7b: Active Object Types ✅ (core types done)

- [x] `mnemosyne:policy` — `deploy_policy()` + `evaluate_policies()` evaluation engine
- [x] Policy evaluation: matches (principal, action, resource_type) against all Policy KOs
- [x] Policy effects: Allow (permit) / Deny (block) with reason string
- [x] `mnemosyne:workflow` — `deploy_workflow()` with JSON step DAG
- [x] `mnemosyne:trigger` — `deploy_trigger()` with event_kind + type_filter + program_koid
- [x] `add_dependency` — DEPENDS_ON relationships between Active KOs
- [x] MCP tools: `deploy_policy`, `evaluate_policies`, `deploy_workflow`, `deploy_trigger`, `add_dependency`
- [x] REST API: 6 new endpoints
- [x] Verified: Allow/Deny policy evaluation, Workflow deployment, Trigger deployment
- [ ] `mnemosyne:agent` — Agent runtime (deferred to Phase 7c)
- [ ] `mnemosyne:connector` — Import/export as KO (deferred)
- [ ] `mnemosyne:view`, `mnemosyne:report`, `mnemosyne:benchmark`, `mnemosyne:ontology` (deferred)

#### Phase 7c: Knowledge Runtime ✅ (core runtime done)

- [x] **Orchestrator** — `execute_workflow()` runs Workflow KO steps in DAG order
- [x] Workflow steps reference Program KOs by name, execute sequentially
- [x] Execution results logged per step (OK: N results / ERROR / SKIP)
- [x] **Trigger Engine** — `check_and_fire_triggers()` polls journal, matches Trigger KOs
- [x] Trigger matching: event_kind comparison, program_koid resolution, auto-execution
- [x] **Program Cache** — LRU cache of compiled IrPlans keyed by (KOID, version)
- [x] Cache hits verified: re-executing same workflow → "(cache hit)" for both steps
- [x] **Execution Journal** — workflow execution recorded as versioned note on Workflow KO
- [x] MCP tools: `execute_workflow`, `check_triggers`, `program_cache_stats`
- [x] REST API: `/api/v1/execute-workflow`, `/api/v1/check-triggers`
- [ ] Agent Runtime — deferred (needs prompt+memory+tools lifecycle)
- [ ] Checkpoint/resume for workflows — deferred (sequential execution sufficient for v1)

#### Phase 7d: Optimization + Stats ✅ (practical subset)

- [x] **Execution Statistics** — `ExecutionStats` struct: programs executed, rows returned, total/avg time, cache hit rate
- [x] Per-step timing: each workflow step reports `OK: N results in Xms`
- [x] **Cross-Program Scan Dedup** — Planner removes duplicate Scans on the same type (even separated by Filters)
- [x] Unit test: dedup_consecutive_scans_on_same_type (31 compiler tests, all green)
- [x] MCP tool: `execution_stats` (program count, rows, timing, cache hit %)
- [ ] JIT compiler (Cranelift/LLVM) — deferred: tree-walking interpreter is sufficient for v1
- [ ] WASM/Python language support — deferred: AIKOQL is the primary language
- [ ] Streaming results — deferred: batch execution sufficient for current workloads
- [ ] Parallel execution — deferred: sequential execution with cached plans is fast enough

### What This Changes

| Before MRFC-0030 | After MRFC-0030 |
|---|---|
| Programs are external to the DB | Programs are KOs, stored + versioned in the DB |
| RBAC is hardcoded rules | Policies are KOs you can query, trace, prove |
| Workflows are external scripts | Workflows are DAGs of Program KOs |
| Agents are separate services | Agents are KOs with memory + skills + policies |
| Connectors are one-off CLI tools | Connectors are versioned KOs with schedules |
| Benchmarks are one-off scripts | Benchmarks are KOs you can version and replay |

### Why This Matters — The Database Architect's View

Traditional databases separate data from code. You have `CREATE TABLE` for data and `CREATE FUNCTION` for code. They live in different namespaces, have different versioning (or none), and different security models. Code is second-class.

MRFC-0030 says: **code IS data**. A program is just a KnowledgeObject with `type_name: mnemosyne:program`. It gets the same:
- **Identity**: immutable KOID
- **Versioning**: MVCC, every change is a new version
- **Provenance**: who wrote it, when, why
- **Access control**: who can read/execute/modify it
- **Dependencies**: what schemas/programs it depends on
- **Events**: every execution is a KnowledgeEvent
- **Audit**: traceable, provable history

This is how Git works. A commit is an object. A tree is an object. A blob is an object. They all live in the same content-addressable store with the same lifecycle. Git doesn't have a separate "code store" and "data store" — everything is an object.

Mnemosyne should work the same way. Everything is a Knowledge Object.

---

## MCP Tool Reference (27 tools)

| Tool | Description |
|------|-------------|
| remember | Commit a knowledge object |
| forget | Tombstone or erase a KO |
| evolve | Transition lifecycle state |
| verify | Check ACL permission |
| get | Fetch KO by KOID |
| find_similar | Hybrid recall: vector + text + filters |
| trace | Full lineage of a KO |
| explain | Provenance + confidence |
| prove | Verify audit trail integrity |
| relate | Create directed relationship |
| traverse | Walk relationship graph |
| eval_recall | Measure recall@k |
| eval_staleness | Index lag distribution |
| eval_contradictions | Find conflicting KOs |
| aikoql | Execute AIKOQL query |
| backup | Create verified backup |
| verify_backup | Check backup integrity |
| restore | Restore from backup (with PITR metadata) |
| list_backups | List available backups |
| metrics | Database metrics (JSON) |
| audit_report | Compliance audit report |
| ping | Liveness check |
