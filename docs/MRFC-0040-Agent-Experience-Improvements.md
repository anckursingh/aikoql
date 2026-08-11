# MRFC-0040: Agent Experience Improvements

**Status:** Complete — all 12 items implemented ✅  
**Last updated:** 2026-08-08
**Last updated:** 2026-08-08
**Based on:** Real-world MCP integration testing + Aethel SDLC agent system analysis  
**Target:** Make aikoql the best database for AI agents to use programmatically

## The 12 Real Frictions I Hit As an Agent Developer

I just wrote a 12-phase integration test that simulates exactly what an AI agent does. Here's what hurt:

### 1. No Python MCP Client SDK

Every agent developer must write their own JSON-RPC over stdio. This is error-prone and unnecessary.

```python
# What I had to write (70 lines of pipe management):
class McpClient:
    def __init__(self, bin_path):
        self.child = subprocess.Popen([bin_path, "serve", db], 
            stdin=PIPE, stdout=PIPE, stderr=DEVNULL)
        self.stdin = self.child.stdin
        self.reader = self.child.stdout
        # ... manual JSON-RPC framing, ID management, error parsing ...
```

```python
# What an agent developer should write:
from aikoql import Agent

db = Agent.connect("./aethel.redb")  # or Agent.connect("localhost:9090")
result = db.remember(type_name="Task", properties={"title": "Fix auth bug"})
tasks = db.aikoql("MATCH Task WHERE status == 'open' RETURN *")
```

**Fix:** Ship a proper `aikoql` Python package with MCP client, async support, auto-reconnect, and typed models.

### 2. No Agent Identity & Run Context

Every tool call requires passing `subject` and `roles` explicitly. An agent has a persistent identity with a run context.

```json
// What I had to do (pass identity on EVERY call):
{"tool": "remember", "args": {"subject": "admin", "type_name": "Task", ...}}
{"tool": "find_similar", "args": {"subject": "admin", "type_name": "Task", ...}}
{"tool": "aikoql", "args": {"query": "...", "subject": "admin", ...}}
```

```json
// What should happen (identity established once):
// Session setup:
{"method": "session/init", "params": {"agent_id": "pm-agent-7", "run_id": "run-42"}}
// Subsequent calls inherit identity:
{"tool": "remember", "args": {"type_name": "Task", ...}}
```

**Fix:** Add `session/init` to MCP protocol. Agent identity, run context, tenant — set once, inherited by all subsequent calls.

### 3. No Batch Operations

Creating multiple objects requires N sequential calls. This is slow and non-atomic.

```python
# What I had to do (N sequential calls):
task1 = db.call("remember", {"type_name": "Task", "properties": {...}})
task2 = db.call("remember", {"type_name": "Task", "properties": {...}})
task3 = db.call("remember", {"type_name": "Task", "properties": {...}})
# 3 round-trips, non-atomic
```

```python
# What should happen (single atomic batch):
results = db.batch([
    {"remember": {"type_name": "Task", "properties": {"title": "A"}}},
    {"remember": {"type_name": "Task", "properties": {"title": "B"}}},
    {"relate": {"from": "$1.koid", "to": "$2.koid", "rel_type": "blocks"}},
])
# 1 round-trip, atomic (all-or-nothing)
```

**Fix:** Add `tools/batch` — atomic multi-operation transaction with variable references (`$1`, `$2`).

### 4. Error Codes Aren't Machine-Parseable

The real-world test had to `unwrap()` and panic. In production, agents need to decide: retry? fallback? report?

```json
// Current (string, no structure):
{"error": "AccessDenied: bob does not have Write on 019fdc..."}
```

```json
// Needed (structured, machine-actionable):
{
  "error": {
    "code": "ACCESS_DENIED",
    "message": "bob does not have Write on 019fdc...",
    "retryable": false,
    "suggestion": "Request Read access or use a different subject"
  }
}
```

**Fix:** Standard error codes: `ACCESS_DENIED`, `VERSION_CONFLICT`, `NOT_FOUND`, `VALIDATION_ERROR`, `RATE_LIMITED`, `TIMEOUT`, `INTERNAL`. Each with `retryable: bool` and `suggestion: string`.

### 5. No Streaming for Large Results

Every query returns all results at once. For large result sets, agents should receive results incrementally.

```python
# Current (blocks until all results ready):
results = db.aikoql("MATCH CodeEmbedding RETURN *")  # Could be 100K rows, blocks for seconds
```

```python
# Needed (streaming iterator):
async for chunk in db.aikoql_stream("MATCH CodeEmbedding RETURN *"):
    for row in chunk.results:
        process(row)
    # Results arrive as they're scanned, not buffered
```

**Fix:** Streaming JSON-RPC responses (newline-delimited JSON sequences) or WebSocket transport for queries.

### 6. Tool Discovery is Incomplete

`tools/list` returns names + descriptions, but agents need more to compose valid calls:

```json
// Current (basic):
{"name": "remember", "description": "Commit a knowledge object"}

// Needed (full schema):
{
  "name": "remember",
  "description": "Create or update a Knowledge Object. Returns KOID, version, commit_ts.",
  "parameters": {
    "type_name": {"type": "string", "required": true, "description": "Object type"},
    "properties": {"type": "object", "required": false, "description": "Key-value properties"},
    "koid": {"type": "string", "required": false, "description": "KOID for updates (omit for create)"},
    "expected_version": {"type": "integer", "required": false, "description": "OCC guard"},
    "idempotency_key": {"type": "string", "required": false, "description": "Retry-safe key"}
  },
  "example": {
    "type_name": "Task",
    "properties": {"title": "Fix login bug", "priority": 1},
    "idempotency_key": "agent-42-task-001"
  }
}
```

**Fix:** Expand `tools/list` with full JSON Schema for parameters + examples.

### 7. Schema Discovery Should Be a MCP Tool

Schema discovery exists as an HTTP endpoint but should be an MCP tool. Agents shouldn't need to know about HTTP vs MCP.

```python
# Current: agent must switch to HTTP
schema = requests.get("http://localhost:9091/api/v1/schema").json()
types = schema["data"]["types"]  # nested in "data" wrapper

# Needed: MCP tool, clean response
types = db.call("discover_schema", {})
# → {"types": ["Task", "CodeEmbedding"], "properties": {"Task": ["title", "status", ...]}}
```

**Fix:** Add `discover_schema` MCP tool. Same data, MCP-native format.

### 8. No Type System for Agent Decisions

Agents make decisions — approve, reject, escalate, delegate. These should be first-class operations.

```python
# Current: agent decisions are just notes on a KO
db.call("remember", {"koid": task_id, "properties": {"status": "approved"}, ...})

# Needed: decision as a first-class operation with provenance
db.call("decide", {
    "koid": task_id,
    "decision": "approve",
    "rationale": "All tests pass, review complete",
    "confidence": 0.95
})
```

**Fix:** Add `decide` MCP tool — records a decision with rationale + confidence as a provenance-tagged KnowledgeEvent.

### 9. No Health/Ready Semantics for Agent Orchestration

Agents in production need to know if the DB is healthy before sending work.

```python
# Current: try and handle error
try:
    db.call("remember", {...})
except Exception:
    # Was it a connection error? DB corruption? Rate limit? Lock contention?
    pass
```

```python
# Needed: structured health check
health = db.call("health", {})
# → {"status": "healthy", "ready": true, "journal_lag_ms": 0, "connection_pool": "3/10"}
```

**Fix:** Add `health` MCP tool with structured readiness + connection pool stats.

### 10. Agent Memory Should Be a Built-In Pattern

The Aethel analysis found the entire agent memory subsystem is ephemeral. aikoql should provide a persistent agent memory pattern out of the box.

```python
# Current: agent must build its own memory store
# (or lose everything on restart, as Aethel does)

# Needed: built-in agent memory
db.call("remember", {
    "type_name": "aikoql:memory",
    "properties": {
        "agent_id": "pm-agent-7",
        "run_id": "run-42",
        "key": "last_user_request",
        "value": "Fix the login page timeout bug",
        "ttl": 3600
    }
})

# Retrieve agent memory
memories = db.call("agent_memory", {
    "agent_id": "pm-agent-7",
    "run_id": "run-42",
    "limit": 10
})
```

**Fix:** Pre-register `aikoql:memory` type. Add `agent_memory` MCP tool with TTL-based expiry.

### 11. Vector Embedding Generation is External

Agents must generate embeddings externally (OpenAI API, sentence-transformers) and pass them to aikoql. This should be integrated.

```python
# Current:
embedding = openai.embeddings.create(input=text, model="text-embedding-3-small")
db.call("remember", {"type_name": "Doc", "properties": {"text": text}, "semantic": {"embedding": embedding}})

# Needed:
db.call("remember", {"type_name": "Doc", "properties": {"text": text}, "embed": true})
# aikoql calls the configured embedding model automatically
```

**Fix:** Wire `SemanticEngine` as a first-class service. Configure embedding model once — all `remember` calls with `embed: true` auto-generate vectors.

### 12. The Two-SDK Problem (PyO3 vs MCP)

Python has two incompatible SDKs:
- `aikoql_py` (PyO3) — opens its own redb file. Embedded.
- Raw MCP client (stdio/TCP) — talks to a server. Needs manual implementation.

Agents don't know which to use and they can't share data.

```python
# Current: two incompatible paths
import aikoql_py  # Embedded — opens ./kb.redb directly
kernel = aikoql_py.Kernel.open("./kb.redb")

# vs

import subprocess, json  # MCP — talks to a server
proc = subprocess.Popen(["aikoql-mcp", "serve", "./kb.redb"], ...)
```

**Fix:** Unify. Ship ONE `aikoql` Python package that supports both modes transparently:

```python
from aikoql import Agent

# Embedded mode (same process):
db = Agent.connect("./kb.redb")

# Server mode (separate process or remote):
db = Agent.connect("localhost:9090")
```

## Prioritized Implementation

| # | Improvement | Effort | Impact | Status | Target |
|---|---|---|---|---|---|
| 1 | **Python MCP Client SDK** | Medium | 🔴 Critical | ✅ | Done (2026-08-08) |
| 2 | **Session/Agent Identity** | Small | 🔴 Critical | ✅ | Done (2026-08-08) |
| 3 | **Structured Error Codes** | Small | 🔴 Critical | ✅ | Done |
| 4 | **Batch Operations** | Medium | 🟠 High | ✅ | Done |
| 5 | **Schema Discovery as MCP Tool** | Small | 🟠 High | ✅ | Done |
| 6 | **Health/Ready Endpoint** | Small | 🟠 High | ✅ | Done (2026-08-08) |
| 7 | **Tool Discovery with JSON Schema** | Medium | 🟡 Medium | ✅ | Done |
| 8 | **Agent Memory Pattern** | Medium | 🟠 High | ✅ | Done |
| 9 | **Streaming Responses** | Large | 🟡 Medium | ✅ | Done (2026-08-08) |
| 10 | **Auto-Embedding** | Medium | 🟡 Medium | ✅ | Done (2026-08-08) |
| 11 | **Decision/Provenance Tool** | Small | 🟡 Medium | ✅ | Done |
| 12 | **Unified Python SDK** | Large | 🟠 High | ✅ | Done (2026-08-08) |

**Completed:** 12/12 ✅ — all MRFC-0040 items are now implemented.
