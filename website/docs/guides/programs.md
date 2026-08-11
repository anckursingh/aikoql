---
title: Programs-as-KOs
description: Deploy, execute, version, and audit knowledge programs
---

# Programs-as-KOs

Programs in aikoql are **first-class Knowledge Objects**. They have identity, versioning, provenance, access control, and audit trail — just like any other KO.

## Deploy a Program

```bash
curl -X POST http://localhost:9091/api/v1/deploy-program \
  -H 'Authorization: Bearer TOKEN' \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "FindEngineers",
    "body": "MATCH Employee WHERE dept == \"Engineering\" RETURN *",
    "language": "aikoql"
  }'
```

Response:
```json
{"data": {"koid": "019fdc...", "version": 1, "name": "FindEngineers", "language": "aikoql"}}
```

The program is now stored as a Knowledge Object of type `aikoql:program`. You can query it, trace it, and audit it.

## Execute a Program

```bash
curl -X POST http://localhost:9091/api/v1/execute-program \
  -H 'Authorization: Bearer TOKEN' \
  -H 'Content-Type: application/json' \
  -d '{"koid": "019fdc..."}'
```

Response:
```json
{
  "data": {
    "results": [
      {"koid": "019fdd...", "type_name": "Employee", "properties": {"name": "Alice", "dept": "Engineering"}}
    ],
    "count": 1
  }
}
```

## Parameter Substitution

Programs support `{{param}}` placeholders:

```aikoql
MATCH Employee WHERE dept == "{{dept}}" RETURN *
```

Execute with parameters:

```json
{"koid": "019fdc...", "params": {"dept": "Engineering"}}
```

The `{{dept}}` placeholder is substituted before compilation.

## Version Your Program

Programs are versioned like any KO. To update:

```bash
# Update the body (v1 → v2)
curl -X POST http://localhost:9091/api/v1/remember \
  -H 'Authorization: Bearer TOKEN' \
  -d '{"koid":"019fdc...","type_name":"aikoql:program","properties":{"body":"MATCH Employee WHERE dept == \"Design\" RETURN *","version":2},"expected_version":1}'
```

Every version remains queryable:

```aikoql
SHOW HISTORY FindEngineers
```

## Compose Workflows

A Workflow KO chains multiple programs:

```json
{
  "name": "DocumentPipeline",
  "steps": [
    {"order": 1, "program": "OCRProcessor"},
    {"order": 2, "program": "EntityExtractor"},
    {"order": 3, "program": "CommitToKernel"}
  ]
}
```

Execute:

```bash
curl -X POST http://localhost:9091/api/v1/execute-workflow \
  -H 'Authorization: Bearer TOKEN' \
  -d '{"koid": "019fdd..."}'
```

Output:
```
Workflow: DocumentPipeline
  Step 1: OCRProcessor → OK: 5 results in 12ms
  Step 2: EntityExtractor → (cache hit) OK: 3 results in 0ms
  Step 3: CommitToKernel → OK: 3 results in 1ms
```

## Program Cache

Compiled programs are cached by (KOID, version). The second execution of the same program version uses the cached plan, giving sub-millisecond response times.

## Access Control

Programs execute with the **caller's identity**. If the caller doesn't have Read access to the target type, the program returns no results. This means:

- Alice deploys `FindSalaries` — she can execute it because she wrote it
- Bob tries to execute `FindSalaries` — he only sees Employees he has Read access to
- `evaluate_policies()` checks Policy KOs at execution time

## Audit Trail

Every program execution is recorded:

```aikoql
MATCH aikoql:program WHERE name == "FindEngineers"
TRACE EACH
```

Shows who deployed it, who executed it, when, and what versions exist.

## Why Programs-as-KOs?

| Traditional DB | aikoql |
|---|---|
| `CREATE FUNCTION` — separate namespace | Program is a KO — same namespace as data |
| No versioning for functions | Every program change is a new version |
| No provenance for code | Full audit trail: who wrote it, when, why |
| No dependency tracking | Programs reference schemas, ontologies, other programs via RelationshipRef |
| Execute as DB user | Execute with caller's identity, Policy KO enforcement |
