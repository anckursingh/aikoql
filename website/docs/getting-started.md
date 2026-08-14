---
title: Getting Started
description: Install and run aikoql in 5 minutes
---

# Getting Started

## CLI Commands

```
aikoql comes with 9 CLI commands:
  shell [DB]             Interactive knowledge shell
  serve [OPTIONS] [DB]   Start MCP server (stdio by default; --listen for TCP)
  ingest-dir [PATH] [DB] Ingest directory into knowledge base
  report [PATH]          Print knowledge report for directory (read-only)
  backup [DB]            Create verified backup
  restore BACKUP [DB]    Restore from backup
  audit [DB]             Print encryption compliance report
  keygen [PATH]          Generate master encryption key
  import <SOURCE> [ARGS] Import from postgres / sqlite / mongodb
```

### Ingest a Codebase

```bash
# Analyze any directory without storing (read-only report)
aikoql report ~/my-project

# Ingest and store as Knowledge Objects
aikoql ingest-dir ~/my-project ./kb.redb
```

Every entity (file, module, function, test, section) becomes its own Knowledge
Object with kernel relationships between them (`depends_on`, `implements`,
`tested_by`). Re-ingesting the same path is idempotent — it updates in place.

The ingest engine classifies every file:
- `.md` → Markdown Knowledge Compiler (sections, ADRs, facts)
- `.rs` → Rust Code Parser (DEPENDS_ON, IMPLEMENTS, TESTED_BY)
- Mixed sources → Merged + deduplicated + staleness-checked

## Installation

### Download Binary

aikoql ships as a single, self-contained binary. No dependencies, no installers.

**Windows:**
```bash
curl -LO https://github.com/anckursingh/aikoql/releases/download/v0.1.7/aikoql-mcp.exe
.\aikoql-mcp.exe --help
```

**Linux (static musl — any distro):**
```bash
curl -LO https://github.com/anckursingh/aikoql/releases/download/v0.1.7/aikoql-mcp-linux-musl
chmod +x aikoql-mcp-linux-musl && mv aikoql-mcp-linux-musl /usr/local/bin/aikoql
```

A glibc build (`aikoql-mcp-linux`) is also available for distros that prefer dynamic linking.

**macOS (Apple Silicon / Intel):**
```bash
# Apple Silicon
curl -LO https://github.com/anckursingh/aikoql/releases/download/v0.1.7/aikoql-mcp-macos-arm64
chmod +x aikoql-mcp-macos-arm64 && mv aikoql-mcp-macos-arm64 /usr/local/bin/aikoql

# Intel
curl -LO https://github.com/anckursingh/aikoql/releases/download/v0.1.7/aikoql-mcp-macos
chmod +x aikoql-mcp-macos && mv aikoql-mcp-macos /usr/local/bin/aikoql
```

### Verify

```bash
aikoql --version
# aikoql-mcp 0.1.7
```

## 5-Second Start

### Interactive Shell

```bash
aikoql shell :memory:
```
```
aikoql> CREATE Person name == "Alice", role == "Architect"
Created: 019fdc... (v1)

aikoql> MATCH Person RETURN *
── 1 row(s) ──
  019fdc...  v1  Person   Alice Architect

aikoql> .tables
  Person

aikoql> .exit
Bye.
```

### MCP Server (stdio mode)

```bash
aikoql serve ./my-knowledge.redb
```

Connects via stdin/stdout — perfect for Claude Code, VS Code, and other MCP clients. Add to your MCP config:

```json
{
  "mcpServers": {
    "aikoql": {
      "command": "aikoql",
      "args": ["serve", "./my-knowledge.redb"]
    }
  }
}
```

### TCP Server + Web UI

```bash
aikoql serve --listen 127.0.0.1:9090 --metrics-addr 127.0.0.1:9091 ./my-knowledge.redb
```

- MCP endpoint: `tcp://127.0.0.1:9090`
- Graph Browser: `http://127.0.0.1:9091/ui`
- REST API: `http://127.0.0.1:9091/api/v1/`
- Health check: `http://127.0.0.1:9091/health`

## First Commands

### Using the REST API

```bash
# Create an object
curl -X POST http://127.0.0.1:9091/api/v1/remember \
  -H 'Content-Type: application/json' \
  -d '{"type_name":"Note","properties":{"body":"Hello aikoql"}}'

# Search
curl -X POST http://127.0.0.1:9091/api/v1/aikoql \
  -H 'Content-Type: application/json' \
  -d '{"query":"MATCH Note RETURN *"}'

# Schema discovery
curl http://127.0.0.1:9091/api/v1/schema
```

### Using the Shell

```bash
# Open a file database
aikoql shell ./kb.redb

# Create objects
aikoql> CREATE Employee name == "Alice", dept == "Engineering", salary == 125000

# Search
aikoql> MATCH Employee WHERE dept == "Engineering" RETURN name, salary

# Backup
aikoql> .backup

# See all commands
aikoql> .help
```

## Connecting from Code

### Python
```python
import aikoql_py
kernel = aikoql_py.Kernel.open("./kb.redb")
result = kernel.remember({"type_name": "Note", "properties": {"body": "Hello"}})
```

### TypeScript
```typescript
import { AikoqlClient } from 'aikoql-sdk';
const client = new AikoqlClient({ command: './aikoql' });
await client.connect();
await client.remember({ type_name: 'Note', properties: { body: 'Hello' } });
```

### Go
```go
import "github.com/ancku/aikoql-sdk"
client := aikoql.NewClient("127.0.0.1:9090")
client.Connect()
result, _ := client.Remember(map[string]interface{}{"type_name": "Note"})
```

## Encryption (Optional)

Enable encryption at rest:

```bash
# Generate a master key
aikoql keygen ./master.key

# Set environment variable
export AIKOQL_PASSPHRASE="your-secure-passphrase"

# Start with encryption
aikoql serve --listen :9090 --metrics-addr :9091 ./encrypted-kb.redb
```

See [Encryption Guide](/docs/guides/encryption) for details on key rotation, field-level encryption, and compliance.

## Next Steps

- [Architecture Overview](/docs/architecture) — Understanding the Knowledge OS
- [API Reference](/docs/api-reference) — All endpoints and tools
- [Import Data](/docs/guides/import) — PostgreSQL, SQLite, MongoDB, Neo4j
- [Programs-as-KOs](/docs/guides/programs) — Deploy your first knowledge program
