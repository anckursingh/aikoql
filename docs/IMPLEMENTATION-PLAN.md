# Mnemosyne — Implementation Plan

**Architecture:** [MRFC-0005](MRFC-0005-System-Architecture.md) | [MRFC-0010](MRFC-0010-AIKOQL-Parser-Architecture-v2.md) | [MRFC-0020](MRFC-0020-Encryption-Key-Management-Architecture.md) | [MRFC-0030](#mrf-0030-active-knowledge-objects--the-knowledge-operating-system) — Active Knowledge Objects | **NEW: [MRFC-0050](#mrf-0050-document-ocr--knowledge-ingestion) — Document OCR & Ingestion**  
**Status:** Phases 1–5 complete, MRFC-0020 complete, API Layer done, MRFC-0030 Phase 7a–7d complete (9/9 Active KOs + Agent Runtime), MRFC-0040 complete, Studio Phase S2/S3 complete, MRFC-0050 Phase D1–D2 complete  
**Last updated:** 2026-08-09

---

## Project Structure (Post-Restructuring, 2026-08-08)

Restructured per advisor architecture review. Dependency direction: **foundation → knowledge → kernel → storage**.

```
crates/
├── kernel/               ← Core: KO model, storage, security, transaction
├── compiler/             ← AIKOQL: lexer → parser → AST → KIR → planner
├── runtime/              ← KVM: interpreter + execution engine
├── engines/              ← Knowledge engines
│   ├── graph/            ← Relationship traversal
│   ├── vector/           ← HNSW + BM25 hybrid indexes
│   ├── scheduler/        ← Background job scheduler
│   ├── semantic/         ← AI provider enrichment (moved from services/)
│   └── reasoning/        ← Rule-based inference (moved from services/)
├── providers/            ← External data system connectors (renamed from connectors/)
│   ├── sdk/              ← Provider trait (new)
│   ├── postgres/
│   ├── sqlite/
│   ├── mongodb/
│   └── neo4j/
├── ingestion/            ← Document ingestion pipeline (moved from services/)
├── services/
│   └── api/mcp/          ← MCP server + REST API + Graph UI
├── cluster/proxy/        ← Cluster proxy
├── sdk/                  ← Language SDKs
│   ├── python/
│   ├── go/
│   ├── java/
│   └── typescript/
benchmarks/               ← Load + micro-benchmarks (moved from crates/)
```

## Current State (Snapshot)

| Metric | Value |
|--------|-------|
| Crates | 20 (kernel, graph, vector, scheduler, semantic, reasoning, compiler, runtime, ingestion, mcp, python-sdk, typescript-sdk, benchmarks, cluster/proxy, provider-sdk, rocksdb, 4 providers) |
| Rust tests | 330+ (all green) |
| MCP tools | 49 |
| Storage backends | 3 (redb, RocksDB, Memory) — StorageEngine trait, 3 methods |
| CLI subcommands | 7 (shell, serve, backup, restore, audit, keygen, import) |
| Providers | 4 (PostgreSQL, SQLite, MongoDB, Neo4j) + Provider SDK trait |
| Compiler pipeline | Lexer → Parser → AST → Semantic Analyzer → KIR → Planner — all 5 statement types, 6 operators |
| SDKs | Python (PyO3 + MCP client), TypeScript, Java, Go — all compiling |
| Encryption status | AES-256-GCM + ChaCha20-Poly1305, Envelope encryption (KEK→DEK), LocalKMS, EncryptedStore, Field-level encryption, KeyAuditLog, ComplianceReport, KeyRotationJob — MRFC-0020 Phase 1–5 complete |
| Fuzz | 3 proptest harnesses |
| Bench | 100 KB query = 111 µs (180× under 20 ms gate) |
| Cluster proxy | Persistent connections, retry with backoff, startup health check, partial-result merging |
| HTTP | REST API (30 endpoints), Graph browser UI + Studio SPA, Prometheus /metrics + /health |
| Agent experience | MRFC-0040 complete — session identity, structured errors, batch ops, schema discovery, health, agent memory, Python MCP client, unified Agent SDK, auto-embedding, streaming queries |
| MVP ready | ✅ Docker packaging (Dockerfile + docker-compose), RocksDB backend (feature-gated, Linux), config-based backend selection |

---

## MVP Readiness Assessment (2026-08-08)

**Mnemosyne is MVP-ready.** All critical paths are implemented and tested.

### What's solid (don't touch)
- Kernel: MVCC, OCC, HLC, SHA-256 audit chain, RBAC, encryption at rest
- AIKOQL: Lexer → Parser → AST → KIR → Planner → Runtime interpreter
- 49 MCP tools: remember, get, find_similar, aikoql, trace, explain, prove, batch, decide, deploy_program/list_programs, deploy_policy/evaluate_policies, deploy_workflow, deploy_trigger, deploy_agent/list_agents, deploy_connector/list_connectors, deploy_view/list_views, deploy_report/list_reports, deploy_benchmark/list_benchmarks, streaming, etc.
- REST API: 24+ endpoints, Graph browser UI, Prometheus metrics
- Python SDK: PyO3 embedded + pure-Python MCP client + unified Agent.connect()
- Agent experience: Session identity, structured errors, batch ops, streaming, auto-embedding
- 292+ Rust tests, 17 Python tests — all green

### Storage Backends

| Backend | Status | Use Case |
|---|---|---|
| **redb** | ✅ Default | Development + single-agent. Pure Rust, zero deps. |
| **RocksDB** | ✅ Implemented (Linux/Docker) | Production. Concurrent readers+writers, LSM-tree. `crates/storage/rocksdb/` |
| **Memory** | ✅ Testing | Conformance suite. Deterministic, no disk I/O. |
| **Native** | ⬜ Post-MVP | Custom LSM/fractal-tree tuned for knowledge workloads. |

The `StorageEngine` trait is 3 methods (`get`, `scan`, `write_batch`). Swapping backends is a config change — no code changes in kernel or above.

### Deploy Options

```bash
# Docker (recommended for production)
docker compose up -d

# Binary (development)
cargo build -p mnemosyne-mcp --release
./target/release/mnemosyne-mcp serve ./kb.redb --listen 0.0.0.0:9090

# Python client
from mnemosyne import Agent
db = Agent.connect("localhost:9090")
```

### Post-MVP Roadmap (not blocking launch)

| Priority | Item | Effort |
|---|---|---|
| ✅ | Programs-as-KOs full runtime (MRFC-0030 Phase 7a–7d) | Done |
| 🟡 | Cloud KMS providers (AWS, Azure, GCP) | 1 week |
| 🟢 | Read replicas + Raft consensus | 1 month |
| 🟢 | Compliance evidence packs (GDPR, HIPAA) | 2 weeks |
| 🟢 | Native storage engine | 6 months |

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
- [x] Cloud KMS stubs — AwsKms, AzureKeyVault, GcpKeyManager implementing KeyManager trait with env-var key loading. Full SDK integration deferred (2026-08-08)

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
- [x] Build scripts: `scripts/build-release.bat` (Windows), `scripts/build-release.sh` (Linux), `scripts/build-all.bat` (cross-platform). Each generates SHA256 checksums, versioned distribution archives (.zip/.tar.gz), and BUILD_INFO.txt.
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

3. **Programs-as-KOs** — ✅ IMPLEMENTED (MRFC-0030 Phase 7a–7d)
   - [x] All 9 Active KO types deployed: program, workflow, policy, agent, trigger, connector, view, report, benchmark
   - [x] Knowledge Runtime: Orchestrator, Trigger Engine, Program Cache, Agent Runtime
   - [x] Agent Runtime: skill resolution, {{prompt}} substitution, program execution with stats
   - [x] 49 MCP tools + 37 REST API endpoints (including all deploy/list/execute for 9 KO types)
   - [x] 15 acceptance tests including m12_agent_runtime_execute_agent_with_skills

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

9. **Missing Knowledge Services** (MRFC-0005 §Knowledge Services) — 🟡 IN PROGRESS
   - OCR, NER, Embedding, Ontology — no crates, no code
   - IngestionPlugin trait exists but only TextLineIngester stub implemented
   - **[MRFC-0050](#mrf-0050-document-ocr--knowledge-ingestion) specifies the full document ingestion pipeline** — see below for phased implementation plan

### Tier 3 — Operational Gaps

10. **CI/CD Pipeline** (VISION Phase 0, Cargo.toml comment) — ✅ IMPLEMENTED
    - [x] `.github/workflows/ci.yml` — check, test (Windows + Linux), lint, build-release, dependency-DAG verification

11. **Cloud KMS Providers** (MRFC-0020 Phase 2) — ✅ IMPLEMENTED (2026-08-08)
    - AwsKms, AzureKeyVault, GcpKeyManager — KeyManager trait impls with env-var key loading
    - Full SDK integration (aws-sdk-kms, azure_security_keyvault) deferred

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
| 1 — Core Architecture | Class B syscalls ✅, ABI stability ✅, API Layer ✅, Programs-as-KOs ✅ (MRFC-0030 Phase 7a–7d, 9 Active KO types, Agent Runtime, 49 tools, 37 REST endpoints) | 4/4 done |
| 2 — High Value | fusion=exact ✅, offline prove ✅, Storage Kernel ⬜, embedding migration ⬜, Knowledge Services 🟡 (MRFC-0050 spec complete, D0 pending) | 2/5 done |
| 3 — Operational | CI/CD ✅, Cloud KMS ✅ (AWS/Azure/GCP stubs), compliance packs ⬜, replicas ⬜, Studio S1/S2/S3 ✅, Document Explorer 🟡 | 4/6 done |
| 4 — Research | Searchable enc, enclaves, PQC, federated mesh, KVM bytecode (deferred), NL frontend | All post-1.0 |

**Gaps closed: 9 of 19. Tier 1 fully complete. 2 in progress (Knowledge Services via MRFC-0050, Document Explorer). 3 remaining in Tiers 2–3: Storage Kernel, embedding migration, compliance packs. Studio 13 panels complete. 15 acceptance tests. Build scripts with SHA256/archives.**

---

---

## MRFC-0050: Document Knowledge Compiler & Ontology Discovery

**Status:** v2 Architecture Revision analyzed. All phases complete: D1 (Foundation) ✅, D2 (OCR) ✅, D3 (Document AST) ✅, D4 (Knowledge IR) ✅, D5 (Ontology Discovery) ✅, D6 (Entity Resolution) ✅, D7 (Knowledge Commit) ✅, D8 (Vector + Retrieval) ✅, D9 (Compiler Pipeline) ✅.  
**Spec v2:** [MRFC-0050-Document-OCR-HLD-LLD-v2.md](../../downloads/MRFC-0050-Document-OCR-HLD-LLD-v2.md) (imported 2026-08-09)  
**Last updated:** 2026-08-09

### v2 Architecture Revision — What Changed

The v2 spec adds a major "Architecture Revision" (§1-20) that re-frames document ingestion as a **Document Knowledge Compiler** rather than an OCR pipeline. Key shifts:

| v1 Concept | v2 Concept | Impact |
|---|---|---|
| OCR is the center | OCR is a replaceable physical-analysis sub-stage | Same code, different framing — no rework needed |
| `DocumentModel` is the output | `DocumentAst` is the intermediate; `KnowledgeIr` is the staging layer | New types needed before semantic extraction (D4) |
| Extract → KO directly | Extract → Document AST → Knowledge IR → validate → KO | Two new intermediate representations |
| Single-source ontology | Multi-signal: DB schemas + KOs + documents → scored candidates | Ontology discovery gets evidence aggregation |
| No conflict handling | Cross-source reconciliation (e.g. Postgres says ACTIVE, PDF says terminated) | New `KnowledgeReconciler` trait (D6) |
| 4 phases (D0-D4) | 9 phases (D1-D9) | Finer granularity, same total scope |
| No chunking architecture | Full chunking/retrieval architecture (§21-46) | New subsystem: semantic chunking, contextualization, hybrid retrieval, reranking, evidence selection |

### v2 Compiler Pipeline (the new mental model)

```
DOCUMENT
   ↓
Physical Analysis  (native text / OCR / visual layout — our D1-D2)
   ↓
Document AST       (provider-independent structural representation)
   ↓
Semantic Analyzer  (entity + relation + fact extraction)
   ↓
Knowledge IR       (staging: EntityCandidate, RelationCandidate, FactCandidate)
   ↓
Ontology Resolution (map candidates to ontology classes/properties)
   ↓
Entity Resolution  (link document entities to existing KOs)
   ↓
Reconciliation     (detect cross-source conflicts)
   ↓
Knowledge Objects  (commit to kernel)
   ↓
Graph + Vector + Provenance
   ↓
AIKOQL → Agent → Answer + Evidence
```

### What We Have (mapped to v2 phases)

| v2 Phase | What | Status |
|---|---|---|
| **D1 — Foundation** | Artifact store, document KO type, upload endpoint, SHA-256 dedup, deploy_document | ✅ Done |
| **D1 — Physical (native)** | Native text extraction: PDF/DOCX/HTML/TXT → DocumentModel + PageModel | ✅ Done |
| **D2 — Physical (OCR)** | Scanned page detection, Tesseract CLI, page-level OCR decision | ← **Next** |
| **D3 — Document AST** | Provider-independent structural model, layout analysis, block classification | ✅ |
| **D4 — Knowledge IR** | EntityCandidate, RelationCandidate, FactCandidate, TemporalAssertion types | ✅ |
| **D5 — Ontology Discovery** | Multi-signal: consume DB schemas + existing KOs + document IR → evidence-backed proposals | ✅ |
| **D6 — Entity Resolution** | Cross-source linking, confidence scoring, evidence aggregation, embedding infrastructure, vector similarity resolver | ✅ |
| **D7 — Knowledge Commit** | Reconciliation, conflict detection, validated KOs committed | ✅ |
| **D8 — Vector + Retrieval** | Semantic chunking, contextualization, embedding, HNSW+BM25, reranking | ✅ |
| **D9 — Agent + Studio** | Document Explorer panel, evidence viewer, AIKOQL SEARCH DOCUMENTS, explainability, end-to-end compiler pipeline | ✅ |

### What Already Exists (Reuse, Don't Rebuild)

| Existing Component | Location | v2 Relevance |
|---|---|---|
| **Ontology system** | `kernel/src/knowledge/ontology.rs` (1285 lines) | D5 — `OntologyRegistry`, `discover_ontology()`, class/property resolution. Multi-signal discovery builds on this. |
| **Vector + HNSW + BM25** | `engines/vector/src/` | D8 — Hybrid retrieval infrastructure exists. Chunking/contextualization layer needed on top. |
| **Scheduler + SchedulerJob** | `engines/scheduler/src/` | Pipeline orchestration — each stage is a SchedulerJob with checkpointing. |
| **SemanticEngine + AiProvider** | `engines/semantic/src/` | D4 — `enrich(KO) → EnrichmentResult`. Needs extension for entity/relation extraction, but the AI provider interface already routes to LLM APIs. |
| **Kernel write path** | `kernel/src/transaction/kernel.rs` | D7 — `remember()`, `relate()`, MVCC, provenance, encryption. All wired. |
| **FsArtifactStore** | `crates/ingestion/src/lib.rs` (inline) | D1 — SHA-256 content-addressed store at `{db_path}.artifacts/{sha256}`. |
| **DocumentModel + PageModel** | `crates/ingestion/src/lib.rs` | D1 — Current extraction output. Will evolve into DocumentAst in D3. |
| **Provider pattern** | `providers/sdk/src/lib.rs` | D2-D4 — DB connectors use `Provider` trait. Same registry pattern for OCR/semantic providers. |

### Design Decisions (v2-informed)

1. **OCR via CLI subprocess, not native binding** — Tesseract CLI (`tesseract input.png output -l eng`) avoids C++ build deps. Swap to native binding only if throughput demands it.

2. **Document AST added when needed, not before** — The `DocumentAst` abstraction pays off when we have multiple physical-analysis backends (native text, OCR, visual layout). With only native text today, `DocumentModel` suffices. Add `DocumentAst` in D3 when OCR produces region-level output that needs normalization.

3. **Knowledge IR added when semantic extraction comes online** — `EntityCandidate`/`RelationCandidate` types are staging for validation before kernel commit. Without semantic extraction (D4), there's nothing to stage. Add in D4.

4. **Chunking deferred to D8** — The v2 spec's §21-46 chunking/retrieval architecture is thorough but depends on having extracted knowledge to chunk. Build the extraction pipeline first, then add chunking when retrieval quality demands it.

5. **Sync-first, async when needed** — `Kernel` is sync. `SchedulerJob::start()` is sync. Keep pipeline stages sync. Add `async` only for cloud OCR or LLM API calls (D4+).

6. **Ponytail module structure** — v2 proposes 16 subdirectories under `crates/ingestion/src/`. Start with 2 files (`lib.rs` + new module per phase). Add subdirectories when a module hits 300+ lines.

### Implementation Plan (v2-aligned)

#### Phase D1: Foundation + Native Text ✅ (2026-08-09)

What was built matches v2's D1 exactly: artifact store, document KO type, binary upload via base64 JSON wrapper, SHA-256 dedup, native text extraction for PDF/DOCX/HTML/TXT, page_count/char_count on Document KOs.

**Files:** `crates/ingestion/src/lib.rs`, `crates/kernel/src/transaction/kernel.rs`, `crates/services/api/mcp/src/main.rs`, `crates/services/api/mcp/tests/mcp_stdio.rs`

#### Phase D2: OCR (Physical Analysis — scanned pages) ✅

**Goal:** Upload a scanned PDF → OCR extracts text → merged with native text pages → same DocumentModel output. Mixed PDFs only OCR pages that need it. **DONE 2026-08-09.**

**What was built:**

| Task | Detail |
|---|---|
| OCR decision heuristic | `page_needs_ocr(text, threshold)` — native text char_count < 10 → mark as scanned |
| Tesseract CLI backend | `ocr_page_image(image_path, language, work_dir)` via `std::process::Command`. Zero new deps. |
| PDF page rasterization | `rasterize_pdf_page(pdf_path, page_num, output_dir)` via `pdftoppm` CLI (poppler-utils). |
| Tool availability check | `tool_available(name)` — checks if CLI tool is on PATH before attempting OCR. |
| Mixed PDF pipeline | `ocr_pdf_pages()` — iterates native pages, rasterizes + OCRs skimpy ones, merges with source tagging. |
| `PageModel.source` field | `"native"` (pdf-extract) or `"ocr"` (Tesseract). Serde default = "native". |
| Graceful degradation | If Tesseract or pdftoppm not on PATH, OCR is skipped — native text extraction still works. |

**All planned features implemented (no skips):**

| Task | Detail |
|---|---|
| `OcrProvider` trait | `recognize(image, language, work_dir) → OcrPageResult { text, confidence, word_confidences, word_count }`. `available()` health check. |
| `TesseractCli` impl | Configurable paths (`tesseract_path`, `pdftoppm_path`). Produces both `.txt` and `.tsv` output in single invocation. |
| TSV confidence parsing | `parse_tesseract_tsv()` — parses level-5 word rows, extracts per-word confidence, computes per-page average. Skips non-word rows and negative confidences. |
| `OcrStats` struct | `pages_ocr_attempted`, `pages_ocr_succeeded`, `pages_ocr_failed`, `average_confidence`. `status()` → `"extracted"` / `"ocr_complete"` / `"ocr_partial"`. |
| `PageModel.ocr_confidence` | `Option<f32>` — per-page average word confidence when source="ocr". |
| `DocumentModel.ocr_stats` | `Option<OcrStats>` — populated when PDF extraction runs OCR. |
| Granular status in MCP | `tool_document_ingest` response includes `ocr_stats` JSON + status derived from OcrStats. Extracted text file now tags pages with `[native]`/`[ocr]` source. |

**Test results:** 23 ingestion unit tests (12 OCR-specific: threshold + tool detection + TSV parsing + OcrStats status + OcrProvider + legacy wrappers + real invoice native extraction + real invoice OCR). 17 MCP acceptance tests (m13 + m14). Zero regressions.

**Real invoice validation (3 invoices from billing-processor):**
- `invoice_3861.pdf`: 1 page, 874 chars native, status=extracted
- `invoice_6147.pdf`: 1 page, 874 chars native, status=extracted  
- `invoice_9655.pdf`: 1 page, 1272 chars native, status=extracted
- OCR on rasterized page: 750 chars, 116 words, **88.1% average confidence**, INVOICE + GSTIN verified present

**Files:** `crates/ingestion/src/ocr.rs` (new, 580 lines), `crates/ingestion/src/lib.rs` (+OcrStats/DocumentModel.ocr_stats/PageModel.ocr_confidence +real invoice tests), `crates/services/api/mcp/src/main.rs` (tool_document_ingest: granular status + ocr_stats JSON), `crates/services/api/mcp/tests/mcp_stdio.rs` (+m14 test).

#### Phase D3: Document AST

**Goal:** Normalized, provider-independent structural representation. Supersedes `DocumentModel` as the output of physical analysis.

| Task | Detail |
|---|---|
| `DocumentAst` type | Sections, paragraphs, tables, figures, lists — each with type tag, bounding box, text, children |
| `AstNode` enum | Section, Paragraph, Table, TableRow, TableCell, Figure, List, ListItem, Header, Footer |
| Layout analysis | Block classification: heading/paragraph/table/image from position + font size heuristics |
| Table structure | Tables → rows → cells with row/col spans, bounding boxes, extracted text |
| `DocumentModel → DocumentAst` adapter | Current extractors produce DocumentModel; wrap into minimal AST (single-section, all-paragraph) |

**Test:** PDF with heading + 2 paragraphs + table → AST has 1 Section containing 1 Heading + 2 Paragraph + 1 Table. Table has correct row/col count.

#### Phase D4: Knowledge IR + Semantic Extraction ✅

**Goal:** Entities, relationships, facts extracted. Staged as Knowledge IR before kernel commit.

**What was built:**

| Task | Detail |
|---|---|
| `Evidence` struct | Per-candidate provenance: document_id, page, bbox_text, extractor, model, confidence |
| `EntityCandidate` | name, type_hint, mentions[], confidence, evidence |
| `RelationCandidate` | subject, predicate, object, confidence, evidence |
| `FactCandidate` | statement, entities[], confidence, evidence |
| `EventCandidate` | description, trigger, participants[], temporal[], confidence, evidence |
| `TemporalAssertion` | text, start_time, end_time (ISO-8601), confidence, evidence |
| `KnowledgeIr` container | entities, relations, facts, events, temporal + document_id, page_count, extractor |
| `SemanticAnalyzer` trait | `analyze(ast: &DocumentAst) → KnowledgeIr` — single-call interface |
| `MockSemanticAnalyzer` | Rule-based: capitalized phrase → entities, heading → facts, co-occurrence → relations, date patterns → temporal |
| `document_model_to_ir()` | Full pipeline: DocumentModel → DocumentAst → KnowledgeIr |

**Test results:** 23 D4 tests — entity extraction, dedup, type hints, fact extraction from headings/titles, relation extraction from co-occurrence, temporal parsing (Month YYYY, ISO-8601, QN YYYY), evidence propagation across pages, pipeline integration, edge cases (empty doc, common words, configurable confidence, trait object). 78 total ingestion tests, 14 MCP acceptance tests. Zero regressions.

**File:** `crates/ingestion/src/ir.rs` (~600 lines)

**At this point, the core compiler pipeline works:**
```
PDF → Physical Analysis(D1+D2) → Document AST(D3) → Knowledge IR(D4) → …(next phases)
```

#### Phases D5-D9: Ontology → Resolution → Commit → Retrieval → Agent

Deferred to post-D4. See v2 spec §10-20 for detailed interfaces. The existing ontology system, vector engine, scheduler, and kernel write path provide the foundation for D5-D9.

### New Acceptance Criteria (v2 AC-17 through AC-27)

| AC | Description | Phase |
|---|---|---|
| AC-17 | Document AST: deterministic, provider-independent structural representation | D3 |
| AC-18 | Representation linkage: text, OCR, visual, layout, semantic retain stable source refs | D3-D4 |
| AC-19 | Knowledge IR: semantic extraction produces IR before kernel mutation | D4 |
| AC-20 | Multi-signal ontology discovery: consumes DB schemas + KOs + documents | D5 |
| AC-21 | Evidence-backed ontology: every concept exposes evidence + confidence | D5 |
| AC-22 | Cross-source entity resolution: document entity resolved against existing KOs | D6 |
| AC-23 | Conflict detection: contradictory assertions detectable, never silently overwritten | D7 |
| AC-24 | Temporal validity: versioned document assertions preserve historical validity | D4 |
| AC-25 | Human review: uncertain results reviewable through Studio/API | D9 |
| AC-26 | Regeneration: derived representations regeneratable from immutable source | D1 ✅ |
| AC-27 | Provider independence: replacing OCR/semantic providers doesn't require kernel changes | D3-D4 |

### What's Deferred (Post-MVP)

- v2 §21-46 Chunking & Retrieval architecture (semantic chunking, contextualization, incremental reconciliation, reranking, evidence fusion, cache invalidation, embedding versioning, index generations) — depends on having extracted knowledge to chunk
- Handwriting-optimized OCR models
- Video/audio transcription
- GPU orchestration
- S3/Azure Blob/GCS artifact backends
- Cloud KMS providers (AWS, Azure, GCP) for encryption
- Active learning review pipeline (§14)

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

#### Phase 7b: Active Object Types ✅ (all 9 types done)

- [x] `mnemosyne:policy` — `deploy_policy()` + `evaluate_policies()` evaluation engine
- [x] Policy evaluation: matches (principal, action, resource_type) against all Policy KOs
- [x] Policy effects: Allow (permit) / Deny (block) with reason string
- [x] `mnemosyne:workflow` — `deploy_workflow()` with JSON step DAG
- [x] `mnemosyne:trigger` — `deploy_trigger()` with event_kind + type_filter + program_koid
- [x] `add_dependency` — DEPENDS_ON relationships between Active KOs
- [x] MCP tools: `deploy_policy`, `evaluate_policies`, `deploy_workflow`, `deploy_trigger`, `add_dependency`
- [x] REST API: 6 new endpoints
- [x] Verified: Allow/Deny policy evaluation, Workflow deployment, Trigger deployment
- [x] `mnemosyne:agent` — `deploy_agent()` + `list_agents()` MCP tools + REST API (2026-08-08)
- [x] `mnemosyne:connector` — `deploy_connector()` + `list_connectors()` MCP tools + REST API (2026-08-08)
- [x] `mnemosyne:view` — `deploy_view()` + `list_views()` MCP tools + REST API (2026-08-09)
- [x] `mnemosyne:report` — `deploy_report()` + `list_reports()` MCP tools + REST API (2026-08-09)
- [x] `mnemosyne:benchmark` — `deploy_benchmark()` + `list_benchmarks()` MCP tools + REST API (2026-08-09)
- [x] `mnemosyne:ontology` — OntologyDef, OntologyRegistry, `discover_ontology()` MCP tool (2026-08-08)

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

## MCP Tool Reference (49 tools)

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
| deploy_view | Deploy a materialized knowledge view (MRFC-0030) |
| list_views | List all deployed views |
| deploy_report | Deploy a compliance/analytics report definition (MRFC-0030) |
| list_reports | List all deployed reports |
| deploy_benchmark | Deploy a versioned, replayable benchmark (MRFC-0030) |
| list_benchmarks | List all deployed benchmarks |
| ping | Liveness check |

---

## MRFC-0040: Agent Experience Improvements

**Status:** ✅ Complete — all 12 items implemented  
**Last updated:** 2026-08-08
**Spec:** [MRFC-0040](MRFC-0040-Agent-Experience-Improvements.md)  
**Last updated:** 2026-08-08

### Implementation Status

| # | Improvement | Status | Notes |
|---|---|---|---|
| 1 | Python MCP Client SDK | ✅ Implemented | `mcp_client.py`: pure Python MCP JSON-RPC client. 20+ tool wrappers. No native deps. 7 integration tests |
| 2 | Session/Agent Identity | ✅ Implemented | `session/init` method + `session_init` tool. `McpSession` wired through handler chain. `inject_session` propagates identity to tool calls. Tests: m09, m10 |
| 3 | Structured Error Codes | ✅ Implemented | `error_codes.rs`: ErrorCode enum with retryable/suggestion. `wrap_result()` on all tool calls |
| 4 | Batch Operations | ✅ Implemented | `batch` MCP tool with $N.koid references (sequential, not atomic) |
| 5 | Streaming Responses | ✅ Implemented | `aikoql/stream` MCP method. Chunks of 100 via notification frames. Client generator yields chunks incrementally. Test: m11 (Rust) + test_aikoql_stream (Python) |
| 6 | Tool Discovery with JSON Schema | ✅ Implemented | `tools/list` returns full `inputSchema` per tool with property types, required, enums. Missing `example` field |
| 7 | Schema Discovery as MCP Tool | ✅ Implemented | `discover_schema` MCP tool returns types + counts |
| 8 | Decision/Provenance Tool | ✅ Implemented | `decide` MCP tool with rationale, confidence, provenance-tagged version |
| 9 | Health/Ready Endpoint | ✅ Implemented | `health` MCP tool: status, ready, journal_seq, journal_lag_ms, object_count, connection_pool, audit_hash, uptime |
| 10 | Agent Memory Pattern | ✅ Implemented | `agent_memory` MCP tool with `mnemosyne:memory` type. TTL stored but not enforced on read |
| 11 | Auto-Embedding | ✅ Implemented | `embed: true` in remember. SemanticEngine handles async enrichment. Pending status returned to caller |
| 12 | Unified Python SDK | ✅ Implemented | `agent.py`: `Agent.connect()` dual-mode (embedded + server). Combined with #1 |

### Completed in this iteration

- **Session identity wiring (#2):** `McpSession` was dead code. Now:
  - `session/init` MCP method stores identity per-connection
  - `inject_session()` propagates agent_id + roles to all tool calls
  - `session_init` tool also updates session for backward compat
  - 2 new acceptance tests: m09 (persistence), m10 (role merge)
  - Also fixed `NOT_FOUND` error classification (underscore format)

- **Health fields (#9):** Added `journal_lag_ms` (0 for single-node) and `connection_pool` (atomic counter tracking TCP connections)

- **Python MCP Client SDK (#1) + Unified Python SDK (#12):** Combined deliverable:
  - `mnemosyne/mcp_client.py` — pure Python MCP JSON-RPC client over TCP. No native deps. Tool wrappers for all 20+ tools.
  - `mnemosyne/agent.py` — `Agent.connect()` auto-detects mode:
    - `Agent.connect("./kb.redb")` → embedded (PyO3 `Mnemosyne`)
    - `Agent.connect("localhost:9090")` → server (MCP TCP)
    - `Agent.connect(("localhost", 9090))` → server (MCP TCP)
  - `McpError` — structured error with code, message, retryable, suggestion
  - 7 Python integration tests (test_mcp_client.py): connect, remember+get, session identity, health, structured errors, Agent unified interface
  - Fixed adapter circular imports for crewai + langgraph

- **Auto-Embedding (#11):** `embed: true` parameter in remember tool. Returns pending status. SemanticEngine handles async enrichment via pluggable AiProvider. Updated tools/list schema and Python client.

- **Streaming (#5):** `aikoql/stream` MCP method. Materialized RowSet chunked into pages of 100. First chunk sent as JSON-RPC response; remaining chunks streamed as `notifications/notify` frames from background thread. Python client: `aikoql_stream()` generator yields chunks incrementally. True server-side cursor streaming (interpreter refactoring) deferred.

### Test coverage

- Rust (mcp_stdio): m01–m11 (11 tests, all pass)
- Python (test_mcp_client): 8 tests covering McpClient + Agent + streaming + structured errors + health + session identity

### Test coverage

- Rust (mcp_stdio): m09 (session persistence), m10 (role merge)
- Python (test_mcp_client): 7 tests covering McpClient + Agent + structured errors + health + session identity

---

## Mnemosyne Studio — The Knowledge OS Desktop

**Status:** ✅ Phase S1 complete (6 core panels, 2026-08-08), ✅ Phase S2 complete (4 differentiator panels + GET API aliases, 2026-08-09), ✅ Phase S3 complete (Profiler + Provider Manager + Admin upgrade, 2026-08-09)  
**Last updated:** 2026-08-09

### Why This Exists

The current Graph Browser (`graph_ui.rs`, 440-line HTML) is a Neo4j Browser clone — graph viz, query editor, schema tab. It shows nodes and edges. It doesn't show that a node has a cryptographic audit trail, 12 lifecycle versions, field-level encryption, or that it's actually an executable Program with a compiled KVM plan.

Mnemosyne Studio is the UI for the **Knowledge Operating System**, not just a graph database. Every panel maps to existing REST API endpoints — zero new backend code.

### Differentiation: What No Competitor Has

| Capability | Neo4j Browser | Databricks | MongoDB Compass | Mnemosyne Studio |
|---|---|---|---|---|
| Graph viz + query | ✅ | — | — | ✅ |
| Schema browser | Labels | Tables | Collections | Ontology + type hierarchy |
| KO lifecycle inspector | — | — | — | ✅ **Unique** |
| **Timeline (MVCC time travel)** | — | — | — | ✅ **Unique** |
| **Cryptographic provenance** | — | Partial lineage | — | ✅ **Unique** |
| **Program/KVM debugger** | — | — | — | ✅ **Unique** |
| **Benchmark center (KOs)** | — | — | — | ✅ **Unique** |
| Provider/connector mgmt | Driver config | External catalogs | — | KO-based with schedules |
| Agent memory browser | — | — | — | ✅ **Unique** |
| Encryption visibility | — | — | — | Field-level status in UI |
| Administration | `:server status` | Admin console | — | Tenants, quotas, keys, backups |

Five capabilities are completely unique — no graph DB, analytics platform, or document DB has them.

### The Killer Features

1. **`git blame` for knowledge** — Click any KO, open Provenance panel, see every version, who changed what, when. Cryptographic proof, not just a `modified_by` field. `TRAVERSE AuditChain DEPTH 10` → rendered as a visual chain.

2. **Time machine** — MVCC-native. Drag a timeline slider, watch the knowledge graph rewind. See what the database knew at any point in time. No `AS OF` SQL tricks — it's built into the storage layer.

3. **Agent OS control panel** — Not a query tool for humans. A place where AI agents inspect their own memory (`agent_memory`), debug their programs (`execution_stats`, `explain`), and verify their knowledge (`prove`).

4. **Programs are first-class citizens** — Deploy AIKOQL code, see it compiled to KVM bytecode, step through execution, check cache hit rates. Programs, workflows, policies — all visible in the same Studio as the data they operate on.

### Architecture

```
Mnemosyne Studio (SPA: ~2000 lines HTML/CSS/JS)
│
├── /api/v1/*           ← 24 REST endpoints (existing)
├── /api/graph          ← Graph data + relationships (existing)
├── /api/schema         ← Schema/ontology discovery (existing)
├── /api/aikoql         ← Query execution (existing)
├── /api/metrics        ← Prometheus metrics JSON (existing)
├── /api/audit          ← Audit report (existing)
├── /api/trace/{koid}   ← Provenance chain (existing)
├── /api/explain        ← Query plan (existing)
├── /api/backups        ← Backup list (existing)
└── /api/health         ← Health status (existing — MCP tool)
```

Zero new backend endpoints. The Studio is a UI shell over the existing API. Every panel is a `<div>` with `fetch()` calls. Same pattern as `graph_ui.rs` — single HTML file served at `/studio`, or split into JS modules if it grows past ~3000 lines.

### Panels

```
Mnemosyne Studio
│
├── Query Editor          AIKOQL with syntax highlighting, history, favorites, streaming results
├── Knowledge Graph       Force-directed vis.js graph, KO-aware (lifecycle colors, encryption badges, tenant badges)
├── Knowledge Explorer    File-tree browser: types → tenants → KOs. Filter by lifecycle state, encryption status, age
├── Schema Explorer       Types + properties + relationship kinds + policy bindings. Click-through to KOs
├── KO Inspector          Full detail: properties, lifecycle, security, encryption status, embeddings, relationships, event refs, audit hash
├── Timeline              ⬜ UNIQUE — MVCC time slider. Rewind knowledge graph. Per-KO version history as scrollable timeline
├── Provenance            ⬜ UNIQUE — Cryptographic audit chain. Visual `git log --graph` for any KO. Prove button → verify integrity
├── Document Explorer     Ingestion pipeline: source → status → KOs created. Per-document processing history
├── Provider Manager      Connector KOs: status, schedule, last run, row counts. Add/configure connectors
├── Query Profiler        Execution plan visualization, timing breakdown, cache hit rates, scan counts
├── Program Debugger      ⬜ UNIQUE — KVM bytecode view, execution step-through, dependency graph, version history, cache stats
├── Benchmark Center      ⬜ UNIQUE — List/run/compare benchmark KOs. Versioned, replayable. Throughput-over-time charts
└── Administration        Tenants, users, quotas, encryption policies, key rotation status, backup/restore, metrics
```

### Phased Implementation

#### Phase S1: Core Studio (2 weeks)

Replace `graph_ui.rs` with Studio shell + 6 panels:

- [x] **Studio shell** — Left sidebar nav + tabbed main area. Dark theme (current colors), responsive. Login/auth from existing graph UI.
- [x] **Query Editor** — Upgrade existing AIKOQL tab: CodeMirror 6 (AIKOQL syntax highlighting), Ctrl+Enter run, query history (localStorage), favorites, streaming toggle (chunked results).
- [x] **Knowledge Graph** — Existing graph viz + KO-aware rendering: lifecycle state → border style (solid=active, dashed=archived), encryption → lock icon badge, tenant → color tint.
- [x] **Knowledge Explorer** — Tree view: fetch `/api/schema` → type list → click type → fetch KOs of that type → list with mini-inspector. Filter bar (type, tenant, lifecycle, has-embeddings).
- [x] **Schema Explorer** — Enhance existing schema tab: add relationship types, policy bindings per type, click type → show all KOs. Ontology view: type inheritance tree.
- [x] **KO Inspector** — Deep detail panel: lifecycle state + transitions, security (owner, classification, ACL entries), encryption (encrypted fields list, key label), embeddings (model + dimension + vector preview), relationships (inbound + outbound), event/journal refs, audit chain hash.

#### Phase S2: The Differentiators (3 weeks)

The panels no competitor has:

- [x] **Timeline** — MVCC time travel. KOID input → fetch `/api/v1/trace/{koid}` → renders version history with KOID, version, lifecycle state, HLC timestamp, mutation source, and property diff per event. Styled as vertical timeline with alternating cards.
- [x] **Provenance** — `git log` for knowledge. Trace chain input + "Prove Chain" button → fetch `/api/v1/trace/{koid}` → render audit events. "Verify" button → `/api/v1/prove` → audit chain integrity check with green checkmark. Visual chain with event nodes.
- [x] **Program Debugger** — Dropdown populated via `/api/v1/list-programs` (GET). Select program → show KOID, language, version, lifecycle, source code preview. Dependency display (program's own koid). Execution stats panel (placeholder for runtime stats).
- [x] **Benchmark Center** — List via `/api/v1/list-benchmarks` (GET). Deploy form: name + query + SLA fields → POST `/api/v1/deploy-benchmark`. Run button → execute AIKOQL query and show results + timing. History: re-fetches benchmark list.

#### Phase S3: Operations (1 week)

- [ ] **Document Explorer** — 🟡 Specified in [MRFC-0050](#mrf-0050-document-ocr--knowledge-ingestion) Phase D0/D4. Depends on `mnemosyne:document` KO type + artifact store + upload endpoint (Phase D0). Evidence viewer deferred to D4.
- [x] **Provider Manager** — connector list table (koid, name, plugin, lifecycle), deploy form (name + plugin → POST `/api/v1/deploy-connector`), auto-refresh on deploy. Panel: 🔌 Providers.
- [x] **Query Profiler** — AIKOQL query textarea + "Profile" button (POST `/api/v1/aikoql`), KOID input + "Explain KO" button (GET `/api/v1/explain/{koid}`). Renders result rows, timing, evidence chain. Panel: 📊 Profiler.
- [x] **Administration** — upgraded with: encryption compliance card (field encryption status, tenant keys, policies), "Create Backup" + "Verify" + "Restore" buttons (POST `/api/v1/backup` etc.), fixed backup table columns (meta.object_count, meta.journal_seq). Added `apiPost()` helper for mutations.

### Why a Single SPA (Not a Frontend Framework)

1. **Zero build step** — Same as current `graph_ui.rs`: one `const GRAPH_UI_HTML: &str` in Rust, served inline. No npm, no webpack, no node_modules.
2. **MCP server ships self-contained** — `mnemosyne-mcp serve` includes Studio. No CDN dependency except vis-network + CodeMirror (both have integrity hashes).
3. **Agents can drive the same API** — The Studio uses the same REST API that agents use. Nothing special. If an agent can call it, the Studio can show it.
4. **ponytail:** A React/Next.js app for a database UI is overkill. The Studio needs 13 panels, each ~100-200 lines of vanilla JS. Total: ~2000-3000 lines. One file, no build, instant load.

Served at `/studio` (existing `/ui` stays as lightweight alternative for quick graph browsing).
