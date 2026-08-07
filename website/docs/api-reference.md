---
title: API Reference
description: Complete reference for all MCP tools and REST endpoints
---

# API Reference

Mnemosyne exposes 38 MCP tools and 35 REST endpoints.

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

### `aikoql` — Execute AIKOQL query

```
POST /api/v1/aikoql
MCP: aikoql
```

```json
{"query": "MATCH Employee WHERE dept == \"Engineering\" RETURN name, salary"}
```

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

### `deploy_program` — Deploy AIKOQL as versioned KO

```
POST /api/v1/deploy-program
MCP: deploy_program
```

```json
{"name": "FindEngineers", "body": "MATCH Employee WHERE dept == \"Engineering\" RETURN *", "language": "AIKOQL"}
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
CLI: mnemosyne backup ./kb.redb
```

### `restore` — PITR restore

```
POST /api/v1/restore
MCP: restore
CLI: mnemosyne restore kb.redb.backup.12345
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
