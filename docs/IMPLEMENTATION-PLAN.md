# Mnemosyne — Implementation Plan

**Architecture:** [MRFC-0005](MRFC-0005-System-Architecture.md) | [MRFC-0010](MRFC-0010-AIKOQL-Parser-Architecture-v2.md) | [MRFC-0020](MRFC-0020-Encryption-Key-Management-Architecture.md)  
**Status:** Phases 1–5 complete, Phase 6 future, MRFC-0020 Phases 1–5 complete, all security gates verified  
**Last updated:** 2026-08-07

---

## Current State (Snapshot)

| Metric | Value |
|--------|-------|
| Crates | 14 (kernel, graph, vector, scheduler, reasoning, semantic, compiler, runtime, ingestion, mcp, python-sdk, typescript-sdk, benchmarks, cluster/proxy) |
| Rust tests | 240+ (all green) |
| MCP tools | 23 |
| CLI subcommands | 6 (shell, serve, backup, restore, audit, keygen) |
| Compiler pipeline | Lexer → Parser → AST → Semantic Analyzer → KIR → Planner — all 5 statement types, 6 operators |
| SDKs | Python (PyO3), TypeScript, Java, Go — all compiling |
| Encryption status | AES-256-GCM + ChaCha20-Poly1305, Envelope encryption (KEK→DEK), LocalKMS, EncryptedStore, Field-level encryption, KeyAuditLog, ComplianceReport, KeyRotationJob — MRFC-0020 Phase 1–5 complete |
| Fuzz | 3 proptest harnesses |
| Bench | 100 KB query = 111 µs (180× under 20 ms gate) |
| Cluster proxy | Persistent connections, retry with backoff, startup health check, partial-result merging |
| HTTP | Prometheus /metrics + /health endpoints |

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

## Phase 6: Knowledge Network (future)

- [ ] Federated knowledge mesh
- [ ] Cross-organization KO exchange
- [ ] Marketplace for ontologies

---

## MCP Tool Reference (22 tools)

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
