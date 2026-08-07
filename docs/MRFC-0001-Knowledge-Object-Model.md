# MRFC-0001: Knowledge Object Model (KOM)

- **Status:** Draft v1.0
- **Project:** Mnemosyne
- **Category:** Foundation
- **Depends on:** None
- **Supersedes:** None

> This RFC is **normative**. Keywords **MUST**, **SHALL**, **SHOULD**, **MAY**, and **MUST NOT** are interpreted as defined by RFC 2119.

---

# 1. Abstract

The Knowledge Object Model (KOM) defines the canonical semantic representation for every entity managed by Mnemosyne. Every subsystem MUST depend on this model rather than on storage layouts, query languages, or AI providers.

---

# 2. Goals

- Define the canonical Knowledge Object.
- Define lifecycle and versioning semantics.
- Define subsystem contracts.
- Enable storage, graph, vector, and document projections.
- Enable deterministic implementation.

## Non-goals

- Page layout
- Binary encoding
- Query language
- Distributed protocols

---

# 3. Terminology

| Term | Meaning |
|------|---------|
| KO | Knowledge Object |
| KR | Knowledge Relationship |
| KE | Knowledge Event |
| KV | Knowledge View |
| KOID | Immutable global identifier |

---

# 4. Normative Requirements

1. Every persisted entity SHALL be represented by exactly one KO.
2. Every KO MUST possess one immutable KOID.
3. Every mutation MUST create a new logical version.
4. Relationships SHALL exist as KR objects.
5. Events SHALL be append-only.
6. Views SHALL NOT own persistent state.
7. Storage MUST remain AI-agnostic.
8. Semantic metadata MUST be optional.
9. Unknown extensions MUST survive round-trip serialization.
10. Implementations MUST reject invalid lifecycle transitions.

---

# 5. Canonical Knowledge Object

```
KnowledgeObject
├── Identity
├── Metadata
├── Properties
├── Semantic
├── RelationshipRefs
├── EventRefs
├── Security
├── Lifecycle
└── Extensions
```

For each block, the implementation SHALL define:
- Rust type
- Validation rules
- Thread-safety guarantees
- Serialization hooks
- Version evolution policy

---

# 6. Lifecycle

```
Draft
  |
Active
  |
Verified
  |
Archived
  |
Deleted
```

Illegal transitions MUST return a deterministic error.

---

# 7. Invariants

- KOID never changes.
- Events are immutable.
- Relationships are first-class.
- Versions are monotonically increasing.
- Views never own data.
- Referential integrity SHALL be enforced according to configured policy.

---

# 8. Concurrency

- Readers SHALL observe snapshot isolation.
- Writers SHALL create new versions.
- Concurrent writers SHALL follow transaction conflict rules (MRFC-00xx).

---

# 9. API Contract

Every implementation SHALL expose an equivalent abstraction to:

```rust
pub trait KnowledgeEntity {
    fn id(&self) -> KOID;
    fn metadata(&self) -> &Metadata;
    fn properties(&self) -> &PropertyMap;
    fn relationships(&self) -> &[RelationshipRef];
    fn events(&self) -> &[EventRef];
    fn security(&self) -> &SecurityDescriptor;
    fn semantic(&self) -> Option<&SemanticBlock>;
}
```

---

# 10. Validation Rules

- Mandatory fields MUST exist.
- Duplicate property identifiers MUST be rejected.
- Unknown core fields MUST fail validation.
- Unknown extension fields MUST be preserved.

---

# 11. Error Model

| Code | Meaning |
|------|---------|
| INVALID_OBJECT | Object schema invalid |
| INVALID_SCHEMA | Schema mismatch |
| VERSION_CONFLICT | Optimistic concurrency failure |
| ACCESS_DENIED | Authorization failure |
| INVALID_STATE | Illegal lifecycle transition |

---

# 12. Security

- ACL evaluation MUST occur before mutation.
- Every lifecycle transition MUST emit an audit event.
- Integrity checksum SHOULD protect serialized objects.

---

# 13. AI Contract

Storage SHALL NOT:
- Generate embeddings
- Call LLMs
- Modify semantic metadata

Knowledge Engine SHALL own semantic enrichment.

---

# 14. Conformance Tests

Implementations MUST include:

- Unit tests
- Property-based tests
- Fuzz tests
- Serialization round-trip tests
- Lifecycle transition tests
- Concurrency tests

---

# 15. AI Implementation Checklist

The coding agent SHALL produce:

- [ ] Rust crates
- [ ] Public traits
- [ ] Domain structs
- [ ] Error enums
- [ ] Validators
- [ ] Lifecycle manager
- [ ] Property tests
- [ ] Benchmarks
- [ ] Documentation
- [ ] Examples

No behavior may be invented beyond this RFC. Ambiguities MUST be reported rather than assumed.

---

# 16. Acceptance Criteria

- All invariants proven by tests.
- 100% schema validation.
- Stable public trait.
- Deterministic lifecycle.
- Backward compatible evolution.

---

# 17. Future RFC Dependencies

- MRFC-0002 Knowledge Relationship
- MRFC-0003 Knowledge Event
- MRFC-0004 Knowledge View
- MRFC-0005 Knowledge Binary Format
- MRFC-0006 Object Identity & Versioning
- MRFC-0007 Storage Kernel API
