# MRFC-0011: Knowledge Syscall ABI (KS-ABI)

- **Status:** Draft v1.0
- **Project:** Mnemosyne
- **Category:** Foundation / API
- **Depends on:** MRFC-0001 (Knowledge Object Model), MRFC-0002 (Knowledge Relationship), MRFC-0003 (Knowledge Event), MRFC-0010 (Consistency & Isolation Levels)
- **Supersedes:** None

> This RFC is **normative**. Keywords **MUST**, **SHALL**, **SHOULD**, **MAY**, and **MUST NOT** are interpreted as defined by RFC 2119.

---

# 1. Abstract

The Knowledge Syscall ABI (KS-ABI) defines the primary, stable programming interface of the Mnemosyne Knowledge Kernel. All other interfaces — SQL, Cypher, GraphQL, REST, natural language, MCP — are adapters that MUST compile down to KS-ABI operations. The ABI is deliberately small, semantically frozen, and governed by a never-break-userspace rule. It is the contract on which the Mnemosyne ecosystem is built.

---

# 2. Goals

- Define the complete, minimal syscall surface of the Knowledge Kernel.
- Partition syscalls by determinism class and execution domain.
- Guarantee perpetual backward compatibility of the syscall surface.
- Enable deterministic implementation and conformance testing.
- Make the kernel programmable by both humans and AI agents.

## Non-goals

- Query-language syntax (adapters' concern).
- Internal execution algorithms.
- Scheduler policy and cost management.
- Distributed routing (cluster MRFCs).

---

# 3. Terminology

| Term | Meaning |
|------|---------|
| Syscall | One atomic operation of the KS-ABI |
| Class A | Deterministic syscall (commit or query domain) |
| Class B | Probabilistic syscall (scheduler domain) |
| Commit domain | The synchronous, deterministic mutation path (MRFC-0008) |
| Query domain | The synchronous, deterministic read path under snapshot isolation |
| Scheduler domain | The asynchronous, probabilistic enrichment path |
| Claim | A KO produced by a Class B syscall, tagged with provenance and confidence |
| ABI | The binary-stable logical contract of the syscall set |

---

# 4. Normative Requirements

1. The syscall surface SHALL consist of exactly the operations defined in §6. New syscalls MAY be added only by a new ratified MRFC (see §9).
2. Every mutation of knowledge SHALL pass through exactly one Class A mutation syscall. Adapters MUST NOT bypass the kernel.
3. Class A syscalls SHALL be deterministic: identical inputs under an identical snapshot MUST produce identical observable outputs and emitted Knowledge Events.
4. Class B syscalls SHALL execute only in the scheduler domain and SHALL re-enter the store exclusively as provenance-tagged Claims via Class A syscalls.
5. No syscall in either class SHALL invoke an LLM, embedding provider, or any non-deterministic external service on the commit or query path.
6. Every syscall SHALL enforce authorization (RBAC + object ACL) before execution and SHALL emit an audit Knowledge Event per MRFC-0001 §12.
7. Every syscall result SHALL carry the snapshot timestamp (`read_ts` or `commit_ts`) under which it executed.
8. Syscall semantics, once ratified, MUST NOT change incompatibly (see §9, never-break-userspace).
9. Implementations MUST reject unknown syscalls with `UNSUPPORTED_OPERATION` and MUST preserve unknown request extensions for forward compatibility.
10. All syscalls SHALL be idempotent-safe: retried calls with the same idempotency key MUST NOT produce duplicate committed effects.

---

# 5. The Syscall Surface

| # | Syscall | Class | Domain | Latency class |
|---|---------|-------|--------|---------------|
| 1 | `remember` | A | Commit | ms |
| 2 | `forget` | A | Commit | ms |
| 3 | `evolve` | A | Commit | ms |
| 4 | `find_similar` | A | Query | 10s ms |
| 5 | `trace` | A | Query | ms |
| 6 | `explain` | A | Query | ms |
| 7 | `prove` | A | Query | ms |
| 8 | `verify` | A | Kernel boundary | ms |
| 9 | `notify` | A | CDC stream | ms (delivery async) |
| 10 | `reason` | B | Scheduler | s–min |
| 11 | `infer` | B | Scheduler | s–min |
| 12 | `predict` | B | Scheduler | s–min |
| 13 | `merge` / `split` | B-assisted | Scheduler → approval → Commit | min–hours |

The surface is intentionally closed at thirteen operations. Everything else is composition, adapters, or plugins.

---

# 6. Syscall Semantics

Rust-like signatures are illustrative; bindings in every language SHALL expose equivalent semantics.

## 6.1 `remember` (Class A — Commit)

```rust
fn remember(&self, req: RememberRequest) -> Result<Remembered, KError>;
```

- Atomically commits a new KO or KO version, its RelationshipRefs, and the corresponding KE in one write batch (MRFC-0008).
- Postconditions: a new monotonically increasing version exists; exactly one `KnowledgeCreated | KnowledgeUpdated` KE is appended; indexes update asynchronously per MRFC-0009.
- Errors: `INVALID_OBJECT`, `INVALID_SCHEMA`, `VERSION_CONFLICT`, `ACCESS_DENIED`.

## 6.2 `forget` (Class A — Commit)

```rust
fn forget(&self, id: KOID, mode: ForgetMode) -> Result<Forgotten, KError>;
```

- `ForgetMode::Tombstone` retains metadata for lineage; `ForgetMode::Erase` performs legal erasure (GDPR-class) while preserving a hash-only audit stub.
- SHALL emit `KnowledgeForgotten` KE. Erasure SHALL NOT rewrite history of other objects; dependent references resolve per configured referential policy (MRFC-0001 §7).
- Errors: `NOT_FOUND`, `ACCESS_DENIED`, `INVALID_STATE`.

## 6.3 `evolve` (Class A — Commit)

```rust
fn evolve(&self, id: KOID, transition: LifecycleTransition) -> Result<Evolved, KError>;
```

- Executes exactly one validated lifecycle transition of the MRFC-0001 state machine (Draft → Active → Verified → Archived → Deleted).
- Illegal transitions MUST return `INVALID_STATE` deterministically. Every transition SHALL emit an audit KE.

## 6.4 `find_similar` (Class A — Query)

```rust
fn find_similar(&self, q: SimilarityQuery) -> Result<Scored<KO>, KError>;
```

- Hybrid recall over vector, full-text, metadata filters, and optional graph-context expansion; fusion strategy and weights are explicit request parameters.
- Results SHALL disclose `index_lag` per consulted index (MRFC-0009). Read-your-writes is provided by delta-overlay within the calling transaction.
- Errors: `INVALID_QUERY`, `ACCESS_DENIED`.

## 6.5 `trace` (Class A — Query)

```rust
fn trace(&self, id: KOID, depth: TraceDepth) -> Result<Lineage, KError>;
```

- Returns the complete lineage of a fact: version chain, originating and derived KEs, contributing relationships, and agent/human authorship per version.

## 6.6 `explain` (Class A — Query)

```rust
fn explain(&self, id: KOID, version: Option<Version>) -> Result<Explanation, KError>;
```

- Answers "why is this believed": provenance (source, ingestion path), confidence, verification status, and evidence object references.
- MUST be pure data assembly from stored provenance; MUST NOT invoke any model.

## 6.7 `prove` (Class A — Query)

```rust
fn prove(&self, claim: KOID) -> Result<Proof, KError>;
```

- Verifies and returns the evidence chain for a claim: hash-chain integrity of the audit stream, version signatures where enabled, and source-attestation status.
- A `Proof` SHALL be independently verifiable outside the kernel (exported with all required hashes).

## 6.8 `verify` (Class A — Kernel boundary)

```rust
fn verify(&self, subject: Subject, object: KOID, action: Action) -> Result<Verdict, KError>;
```

- Evaluates RBAC + object ACL + integrity checksum + optional confidence threshold.
- All other syscalls SHALL internally invoke `verify` before mutation or disclosure.

## 6.9 `notify` (Class A — CDC stream)

```rust
fn notify(&self, subscription: Subscription) -> Result<Stream<KE>, KError>;
```

- Subscribes to the Knowledge Event stream with filter predicates (object, type, relationship, tenant).
- Delivery SHALL be at-least-once with resume tokens; ordering per partition SHALL follow commit order.

## 6.10 `reason` (Class B — Scheduler)

```rust
fn reason(&self, program: ProgramKORef, scope: Scope) -> Result<JobHandle, KError>;
```

- Schedules rule-based or LLM-assisted inference over a scope. Results re-enter the store only as Claims via `remember`, tagged `origin=reason`, carrying program identity, model identity, and confidence.
- SHALL be replayable: identical program + identical scope snapshot SHOULD yield equivalent Claim sets (deterministic programs MUST; LLM programs SHOULD within tolerance).

## 6.11 `infer` (Class B — Scheduler)

- Derives facts from ontology/rules over committed knowledge (e.g., transitive closure, type materialization). Derived facts SHALL be new versions/Claims, never silent in-place mutation.

## 6.12 `predict` (Class B — Scheduler)

- Produces forecast Claims with mandatory `model_id`, `confidence`, and `valid_until`. Predictions MUST be distinguishable from observations in every projection and query result.

## 6.13 `merge` / `split` (Class B-assisted — Scheduler → approval → Commit)

- Proposes entity/knowledge reconciliation (duplicate resolution, concept decomposition) as data. The proposal is a Claim set; a human or policy approval commits it via `evolve`/`remember`. Reconciliation SHALL preserve full lineage: merged objects remain traceable to their originals.

---

# 7. The Determinism Law

1. The commit and query domains SHALL contain no non-deterministic operation, wall-clock dependence (other than assigned commit timestamps from the HLC), or external service call.
2. Class B outputs MUST NOT influence Class A execution except by being committed as Claims through Class A syscalls.
3. Any violation of this law is a severity-1 defect and SHALL fail conformance.

---

# 8. Error Model

Extends MRFC-0001 §11. All syscalls SHALL return these codes; adapters MUST preserve them verbatim.

| Code | Meaning |
|------|---------|
| `INVALID_OBJECT` | Object schema invalid |
| `INVALID_SCHEMA` | Schema mismatch |
| `INVALID_QUERY` | Malformed similarity/trace request |
| `VERSION_CONFLICT` | Optimistic concurrency failure |
| `ACCESS_DENIED` | Authorization failure |
| `INVALID_STATE` | Illegal lifecycle transition |
| `NOT_FOUND` | KOID does not exist in snapshot |
| `UNSUPPORTED_OPERATION` | Unknown or unratified syscall |
| `INDEX_LAG_EXCEEDED` | Requested freshness cannot be met |
| `JOB_REJECTED` | Scheduler admission control refusal |

---

# 9. ABI Stability & Evolution Policy

1. **Never break userspace.** Ratified syscall semantics MUST NOT change incompatibly within a major version, and major-version breakage requires a superseding MRFC plus a two-release deprecation window.
2. Evolution is **additive only**: new optional request fields, new response fields, new error codes, new syscalls via new MRFCs.
3. Implementations and clients MUST ignore unknown fields and preserve them on round-trip (aligned with MRFC-0001 req. 9).
4. Each syscall SHALL report its `abi_version`; kernels MUST serve all ratified versions concurrently.
5. Plugins and adapters SHALL declare the KS-ABI versions they require; the kernel SHALL refuse incompatible adapters at load time with `UNSUPPORTED_OPERATION`.

---

# 10. Security

1. `verify` SHALL be enforced inside the kernel for every syscall, including adapter-originated calls.
2. Every mutation and every Class B job admission SHALL emit an audit KE in the hash-chained audit stream.
3. `prove` outputs SHALL be exportable and offline-verifiable for regulatory evidence.
4. `notify` subscriptions SHALL enforce the same ACL filters as direct reads.

---

# 11. Conformance Tests

Implementations MUST include:

- Determinism tests: identical Class A replays produce byte-identical KE streams.
- Idempotency tests: retried mutations commit exactly once.
- Lifecycle-transition matrices for `evolve` (all legal/illegal pairs).
- Provenance tests: `trace`/`explain`/`prove` correctness against synthetic claim graphs.
- ABI tests: unknown-field round-trip, version negotiation, adapter compatibility refusal.
- Scheduler tests: Class B results appear only as tagged Claims; no commit-path external calls (verified by fault-injection of provider endpoints).
- Concurrency tests (`loom`-modelled) for concurrent `remember`/`evolve` on shared KOIDs.

---

# 12. AI Implementation Checklist

The coding agent SHALL produce:

- [ ] Syscall trait definitions (Class A and Class B)
- [ ] Request/response domain structs with extension preservation
- [ ] Error enums mapped to §8
- [ ] Kernel-boundary `verify` integration
- [ ] Idempotency-key handling
- [ ] Determinism conformance tests
- [ ] Property tests and ABI round-trip tests
- [ ] Documentation and per-language binding examples

No behavior may be invented beyond this RFC. Ambiguities MUST be reported rather than assumed.

---

# 13. Acceptance Criteria

- All thirteen syscalls pass the conformance suite.
- Determinism law verified under fault injection.
- ABI stability demonstrated by mixed-version client/kernel interop tests.
- Every syscall emits required audit events.
- Adapters (SQL, MCP) demonstrate compilation to KS-ABI with error-code preservation.

---

# 14. Future RFC Dependencies

- MRFC-0012 Knowledge Programs (programs-as-KOs; execution model for `reason`)
- MRFC-0013 Scheduler Domain & Admission Control
- MRFC-0014 Adapter Compilation Contracts (SQL/Cypher/GraphQL/NL/MCP → KS-ABI)
- MRFC-0015 Federated `notify` (cross-kernel knowledge event exchange, Phase 5)