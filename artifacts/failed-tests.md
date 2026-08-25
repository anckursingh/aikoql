# Failed / Open Tests

> generated from TESTING-PLAN.md §9.1 by scripts/certify.js

## MVP-ONT-001 Auto-discovery pg/mongo/neo4j — NOT_IMPLEMENTED

- Priority: P1 · Gate: —
- NOT_IMPLEMENTED — needs connectors (TP-5); A9 bridge + fixtures only

## MVP-CON-001..004 PostgreSQL/MongoDB/Neo4j/PGVector — NOT_IMPLEMENTED

- Priority: P0 · Gate: G4
- NOT_IMPLEMENTED — fixtures + A9 bridge only; connector matrix is TP-5 (scope conflict with §2 of MVP-QA-001 — see 9.3)

## MVP-CON-005 Source timeout — NOT_IMPLEMENTED

- Priority: P1 · Gate: —
- connector-side NOT_IMPLEMENTED; product-side rollback semantics = t06k transact all-or-nothing

## MVP-CON-006 Auth failure, no secrets in logs — OPEN

- Priority: P0 · Gate: G3
- product auth failures covered (MCP token tests); **TDD**: log-redaction assertion (credentials never in ordinary logs)

## MVP-CON-007 Outage ≠ deletion — OPEN

- Priority: P1 · Gate: —
- repo-side ✅ (`git_change_set_propagates_git_failure`, no-changes → cached); connector-side ❌ with connectors

## MVP-E2E-001 PostgreSQL → KO → query — NOT_IMPLEMENTED

- Priority: P0 · Gate: G4
- NOT_IMPLEMENTED — connectors (TP-5)

## MVP-E2E-004 Multi-source query — OPEN

- Priority: P0 · Gate: G4
- fixture-level ✅ (`multi_source_ontology.rs` merges pg/mongo/neo4j/doc fixtures); live-connector ❌

## INV-001..010 invariants — OPEN

- Priority: — · Gate: G14
- no-orphan (t06b/c), idempotence, provenance, evidence, authz closure, temporal consistency, atomicity (t06k), restart, determinism ✅; INV-010 source isolation = MVP-CON-007 (repo ✅ / connector ❌)

## Gate-level blockers

- MVP-ONT-001 Auto-discovery pg/mongo/neo4j [not_implemented] — NOT_IMPLEMENTED — needs connectors (TP-5); A9 bridge + fixtures only
- MVP-CON-001..004 PostgreSQL/MongoDB/Neo4j/PGVector [not_implemented] — NOT_IMPLEMENTED — fixtures + A9 bridge only; connector matrix is TP-5 (scope conflict with §2 of MVP-QA-001 — see 9.3)
- MVP-CON-005 Source timeout [not_implemented] — connector-side NOT_IMPLEMENTED; product-side rollback semantics = t06k transact all-or-nothing
- MVP-CON-006 Auth failure, no secrets in logs [open] — product auth failures covered (MCP token tests); **TDD**: log-redaction assertion (credentials never in ordinary logs)
- MVP-CON-007 Outage ≠ deletion [open] — repo-side ✅ (`git_change_set_propagates_git_failure`, no-changes → cached); connector-side ❌ with connectors
- MVP-E2E-001 PostgreSQL → KO → query [not_implemented] — NOT_IMPLEMENTED — connectors (TP-5)
- MVP-E2E-004 Multi-source query [open] — fixture-level ✅ (`multi_source_ontology.rs` merges pg/mongo/neo4j/doc fixtures); live-connector ❌
- INV-001..010 invariants [open] — no-orphan (t06b/c), idempotence, provenance, evidence, authz closure, temporal consistency, atomicity (t06k), restart, determinism ✅; INV-010 source isolation =
- P0 at 87% (26/30 pass)
- P1 at 77% (10/13 pass)
- Connectors gate not fully passing
- E2E gate not fully passing
