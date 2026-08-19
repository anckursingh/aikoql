---
title: API Reference
description: Complete reference for all MCP tools and REST endpoints
---

# API Reference

aikoql exposes its full MCP tool registry (autodiscovered via `tools/list`) and 40+ REST endpoints.

## Authentication

All mutation endpoints require a Bearer token:

```bash
# Get a token
curl -X POST http://localhost:9091/api/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"admin"}'

# Use the token
curl http://localhost:9091/api/v1/remember \
  -H 'Authorization: Bearer YOUR_TOKEN' \
  -H 'Content-Type: application/json' \
  -d '{"type_name":"Note","properties":{"body":"Hello"}}'
```

**Default credentials:** `admin/admin` (full access), `user/user` (read-only).

## Knowledge CRUD

### `remember` — Create or update a Knowledge Object

```
POST /api/v1/remember
MCP: remember
```

**Parameters:**
| Field | Type | Required | Description |
|---|---|---|---|
| `type_name` | string | yes | Object type |
| `properties` | object | no | Key-value properties |
| `tenant` | string | no | Tenant identifier |
| `koid` | string | no | KOID for updates (omit for create) |
| `expected_version` | integer | no | OCC version guard |
| `idempotency_key` | string | no | Retry-safe exact-once key |
| `tags` | string[] | no | Metadata tags |

**Example:**
```json
{
  "type_name": "Employee",
  "tenant": "acme",
  "properties": {"name": "Alice", "role": "Architect", "salary": 125000},
  "tags": ["hr", "engineering"]
}
```

**Response:**
```json
{"data": {"koid": "019fdc...", "version": 1, "commit_ts": 117054338102263808}}
```

### `get` — Fetch a Knowledge Object by KOID

```
GET /api/v1/get/{koid}
MCP: get
```

### `forget` — Delete or tombstone a Knowledge Object

```
POST /api/v1/forget
MCP: forget
```

### `evolve` — Transition lifecycle state

```
POST /api/v1/evolve
MCP: evolve
```

States: `Draft → Active → Archived → Deleted`

### `verify` — Check ACL permission

```
POST /api/v1/verify
MCP: verify
```

## Search

### `find_similar` — Hybrid vector + text search

```
POST /api/v1/find-similar
MCP: find_similar
```

**Parameters:**
| Field | Type | Description |
|---|---|---|
| `type_name` | string | Filter by object type |
| `text` | string | Text query (BM25) |
| `vector` | float[] | Vector query (HNSW cosine) |
| `fusion` | string | `rrf`, `weighted`, `vector`, `text` |
| `k` | integer | Result count (default: 10) |

### `aikoql` — Execute aikoql query

```
POST /api/v1/aikoql
MCP: aikoql
```

```json
{"query": "MATCH Employee WHERE dept == \"Engineering\" RETURN name, salary"}
```

Temporal and epistemic operators (v0.3):

```aikoql
MATCH Employee AS_OF 1724025600000 RETURN *            -- state at a point in time
MATCH Employee BETWEEN 1724025600000 AND 1724630400000 RETURN *
MATCH Employee HISTORICAL RETURN *                     -- every committed version
MATCH Employee EPISTEMIC verified RETURN *             -- only verified knowledge
MATCH Employee EPISTEMIC asserted RETURN *             -- incl. observations/claims
```

Default `MATCH` already filters to facts valid *now* (`valid_at(now)`); `HISTORICAL` and `AS_OF` escape that boundary.

## Knowledge Transactions (v0.3)

Knowledge as a versioned, evidence-backed, evolving object — not just CRUD.

| MCP tool | What it does |
|---|---|
| `transition_epistemic` | Move a KO's epistemic status under the constrained 7-state table (`observed → asserted → verified → contradicted → superseded`); supersession stamps `valid_to` and wires the SUPERSEDES edge |
| `derive` | Derive a new KO from premise KOs: validates sources, wires DERIVED_FROM edges, stamps a derivation record (operation, actor, model, timestamp, reason, sources) |
| `observe` | Record a direct observation. **Evidence mandatory** — unbacked observations are rejected |
| `assert_knowledge` | Assert knowledge on explicit authority (e.g. `human_approved`, `source_code`, `test_verified`). Authority + evidence mandatory |
| `verify_knowledge` | Independently verify a KO: bumps the confidence context (confirmations + `last_verified`, score never lowered). Not a status flip |
| `contradict` | Register a competing assertion: counter-claim + CONTRADICTS edge + persisted `aikoql:conflict` KO with per-assertion authority/evidence snapshots. Original claim untouched until resolution |
| `supersede` | Replace a claim with a new generation: old KO → Superseded + `valid_to` + SUPERSEDES edge; derived dependents swept for staleness |
| `merge` | Merge 2+ sources into one KO as a first-class derivation (`manual` \| `newest_wins` \| `authority_wins`) |
| `invalidate` | Withdraw support for a KO and everything derived from it: target → Contradicted where legal; every DERIVED_FROM dependent gets the invalidation stamp + `valid_to=now` |
| `resolve_conflict` | Apply a resolution decision to a Conflict KO (`resolved_a_preferred` \| `resolved_b_preferred` \| `resolved_both_valid` \| `resolved_replaced`). Rationale mandatory — the kernel never silently picks |
| `resolve_conflict_by_authority` | Resolve by recorded assertion authority — higher wins; a tie is an error |
| `record_experience` | Record an agent run outcome as an `aikoql:experience` KO (TTL-bounded validity, evidence mandatory, confidence context, `reuse_conditions` gating) |
| `find_experiences` | Match recorded experiences for reuse against a task: reuse-condition gating, confidence-weighted ranking, expired/invalidated filtered, ACL-scoped |

**Example — the full knowledge lifecycle in 6 calls:**

```json
{"method":"tools/call","params":{"name":"assert_knowledge","arguments":{
  "subject":"agent-a","type_name":"Claim","properties":{"text":"kernel commits under one pipe lock"},
  "authority":"source_code","evidence":[{"source_artifact":"crates/kernel/src/transaction/kernel.rs","method":"ast_extraction","confidence":0.9}]}}}

{"method":"tools/call","params":{"name":"transition_epistemic","arguments":{
  "subject":"agent-a","koid":"<koid>","to":"verified"}}}

{"method":"tools/call","params":{"name":"trace","arguments":{
  "subject":"agent-a","koid":"<koid>"}}}
// → versions (with commit_ts), events, derivation, confidence, invalidation, evidence
```

## Lineage: `trace` vs `explain`

`trace` (v0.3) is the full lineage of a fact — one call answers all six questions:

- **WHY** — derivation reason / invalidation reason
- **FROM WHAT** — derivation sources (premise KOIDs + types) and evidence source artifacts
- **DERIVED HOW** — derivation operation, model, timestamp
- **BY WHOM** — actor of every derivation, verification, and invalidation
- **WHEN** — `valid_from`/`valid_to`, per-version commit timestamps
- **WITH WHICH EVIDENCE** — evidence trail (source_artifact, location, revision, method, confidence)

`explain` remains the short-form provenance + confidence summary.

## Graph

### `relate` — Create a directed relationship

```
POST /api/v1/relate
MCP: relate
```

```json
{"from": "019fdc...", "to": "019fdd...", "rel_type": "knows"}
```

### `traverse` — Walk the relationship graph

```
POST /api/v1/traverse
MCP: traverse
```

```json
{"koid": "019fdc...", "depth": 2, "direction": "outbound"}
```

### `trace` — Full lineage of a Knowledge Object

```
GET /api/v1/trace/{koid}
MCP: trace
```

## Audit

### `prove` — Verify audit trail integrity

```
POST /api/v1/prove
MCP: prove
```

### `explain` — Provenance + confidence

```
POST /api/v1/explain
MCP: explain
```

### `abi_version` — ABI version + offline proof export

```
GET /api/v1/abi-version
MCP: abi_version
```

## Programs-as-KOs (MRFC-0030)

### `deploy_program` — Deploy aikoql as versioned KO

```
POST /api/v1/deploy-program
MCP: deploy_program
```

```json
{"name": "FindEngineers", "body": "MATCH Employee WHERE dept == \"Engineering\" RETURN *", "language": "aikoql"}
```

### `execute_program` — Execute a program by KOID

```
POST /api/v1/execute-program
MCP: execute_program
```

```json
{"koid": "019fdc...", "params": {"dept": "Engineering"}}
```

### `execute_workflow` — Run a Workflow KO

```
POST /api/v1/execute-workflow
MCP: execute_workflow
```

### `list_programs` — List all deployed programs

```
POST /api/v1/list-programs
MCP: list_programs
```

### `deploy_policy` — Deploy RBAC policy as KO

```
POST /api/v1/deploy-policy
MCP: deploy_policy
```

```json
{"name": "HRRead", "effect": "Allow", "principal": "hr-team", "action": "Read", "resource_type": "Employee"}
```

### `evaluate_policies` — Check (principal, action, resource)

```
POST /api/v1/evaluate-policies
MCP: evaluate_policies
```

### `deploy_workflow` — DAG of programs as KO

```
POST /api/v1/deploy-workflow
MCP: deploy_workflow
```

### `deploy_trigger` — Event-condition-action trigger

```
POST /api/v1/deploy-trigger
MCP: deploy_trigger
```

## Backup & Restore

### `backup` — Create verified backup

```
POST /api/v1/backup
MCP: backup
CLI: aikoql backup ./kb.redb
```

### `restore` — PITR restore

```
POST /api/v1/restore
MCP: restore
CLI: aikoql restore kb.redb.backup.12345
```

### `backups` — List available backups

```
GET /api/v1/backups
MCP: list_backups
```

### `verify-backup` — Check backup integrity

```
POST /api/v1/verify-backup
MCP: verify_backup
```

## Observability

### `metrics-info` — Database metrics

```
GET /api/v1/metrics-info
MCP: metrics
```

### `audit` — Compliance audit report

```
GET /api/v1/audit
MCP: audit_report
```

### `compliance` — Encryption compliance

```
GET /api/v1/compliance
MCP: compliance_report
```

### `schema` — Schema discovery

```
GET /api/v1/schema
```

Returns all types with properties, counts, and tenants.

### `graph` — Graph browser data

```
GET /api/v1/graph?koid=...&tenant=acme&detail=1
```

### `openapi.json` — OpenAPI 3.0 specification

```
GET /api/v1/openapi.json
```

## Evals

### `eval/recall` — Measure recall@k

```
POST /api/v1/eval/recall
MCP: eval_recall
```

### `eval/staleness` — Index lag distribution

```
POST /api/v1/eval/staleness
MCP: eval_staleness
```

### `eval/contradictions` — Find conflicting KOs

```
POST /api/v1/eval/contradictions
MCP: eval_contradictions
```

## Agent Knowledge Interface (MRFC-0070)

Compile, reconcile, and bridge knowledge for AI agents.

### `agent/compile-context` — Compile minimum sufficient context for a task

```
POST /api/v1/agent/compile-context
MCP: compile_context
```

```json
{"task": "Fix the login bug in auth.rs", "token_budget": 8000}
```

Returns ranked, deduplicated context package with entities, facts, and relationships.

### `agent/reconcile` — Git diff → affected entities → auto-proposals

```
POST /api/v1/agent/reconcile
MCP: reconcile
```

```json
{"git_diff": "...", "knowledge_base_path": "./kb.redb"}
```

### `agent/connector-bridge` — DB schema metadata → KnowledgeIr

```
POST /api/v1/agent/connector-bridge
MCP: connector_bridge
```

```json
{"connector_type": "postgresql", "schema_metadata": {...}}
```

### `agent/filter-secrets` — PII/secret filtering (11 secret types)

```
POST /api/v1/agent/filter-secrets
MCP: filter_secrets
```

### `agent/explain-component` — Explain a code component from knowledge graph

```
POST /api/v1/agent/explain-component
MCP: explain_component
```

### `agent/explain-decision` — Explain an ADR from knowledge graph

```
POST /api/v1/agent/explain-decision
MCP: explain_decision
```

### `agent/trace-requirement` — Trace requirement → implementation path

```
POST /api/v1/agent/trace-requirement
MCP: trace_requirement
```

### `agent/find-conflicts` — Detect contradictory facts across sources

```
POST /api/v1/agent/find-conflicts
MCP: find_conflicts
```

### `agent/find-stale` — Detect stale facts

```
POST /api/v1/agent/find-stale
MCP: find_stale
```

### `agent/validate-change` — Validate a proposed change against constraints

```
POST /api/v1/agent/validate-change
MCP: validate_change
```

### `agent/propose-update` — Auto-propose knowledge update from change

```
POST /api/v1/agent/propose-update
MCP: propose_update
```

### Memory Tools

```
POST /api/v1/agent/memory-search   MCP: memory_search
POST /api/v1/agent/memory-store    MCP: memory_store
POST /api/v1/agent/memory-update   MCP: memory_update
POST /api/v1/agent/memory-delete   MCP: memory_delete
```

## Document Pipeline (D1-D9)

### `documents` — Ingest a document (PDF, DOCX, Markdown)

```
POST /api/v1/documents
MCP: document_ingest
```

```json
{"path": "/uploads/architecture-decision.pdf", "title": "ADR-0042"}
```

Runs the full D1-D9 pipeline: Upload → OCR → Document AST → KnowledgeIr → Ontology → Resolution → Commit.

### `documents/compile` — Compile an ingested document to KOs

```
POST /api/v1/documents/compile
MCP: document_compile
```

### `list-documents` — List all ingested documents with status

```
GET /api/v1/list-documents
MCP: document_list
```

### `documents/{koid}/status` — Get document pipeline status

```
GET /api/v1/documents/{koid}/status
MCP: document_status
```

## Connector Management

### `deploy-connector` — Deploy a connector as KO

```
POST /api/v1/deploy-connector
MCP: deploy_connector
```

### `list-connectors` — List all connectors

```
POST /api/v1/list-connectors
MCP: list_connectors
```

## Active KOs: Views, Reports, Benchmarks

### Views

```
POST /api/v1/deploy-view      MCP: deploy_view
POST /api/v1/list-views       MCP: list_views
```

### Reports

```
POST /api/v1/deploy-report    MCP: deploy_report
POST /api/v1/list-reports     MCP: list_reports
```

### Benchmarks

```
POST /api/v1/deploy-benchmark MCP: deploy_benchmark
POST /api/v1/list-benchmarks  MCP: list_benchmarks
```

## Trigger Management

### `deploy-trigger` — Deploy event-condition-action trigger

```
POST /api/v1/deploy-trigger
MCP: deploy_trigger
```

### `check-triggers` — Evaluate pending triggers

```
POST /api/v1/check-triggers
MCP: check_triggers
```

## Class B (Inference)

### `reason` — Rule execution with provenance

```
POST /api/v1/reason
MCP: reason
```

### `infer` — Similarity-based inference

```
POST /api/v1/infer
MCP: infer
```

### `predict` — Property prediction from similar objects

```
POST /api/v1/predict
MCP: predict
```

## Error Responses

All errors follow a consistent format:

```json
{
  "error": "description of what went wrong"
}
```

HTTP status codes: `200` (success), `400` (bad request), `401` (unauthorized), `404` (not found), `500` (internal error).
