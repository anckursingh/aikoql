---
title: Getting Started
description: Install and run Mnemosyne in 5 minutes
---

# Getting Started

## CLI Commands

```
Mnemosyne comes with 9 CLI commands:
  shell [DB]             Interactive knowledge shell
  serve [--listen ADDR] [--metrics-addr ADDR] [DB]  Start MCP + HTTP server
  ingest-dir [PATH] [DB] Ingest directory into knowledge base
  report [PATH]          Print knowledge report for directory (read-only)
  backup <db>            Create verified backup
  restore <backup>       PITR restore from backup
  keygen <path>          Generate master encryption key
  encrypt <db>           Encrypt an existing database
  decrypt <db>           Decrypt a database
```

### Ingest a Codebase

```bash
# Analyze any directory without storing (read-only report)
mnemosyne report ~/my-project

# Ingest and store as Knowledge Objects
mnemosyne ingest-dir ~/my-project ./kb.redb
```

The ingest engine classifies every file:
- `.md` → Markdown Knowledge Compiler (sections, ADRs, facts)
- `.rs` → Rust Code Parser (DEPENDS_ON, IMPLEMENTS, TESTED_BY)
- Mixed sources → Merged + deduplicated + staleness-checked

## Installation

### Download Binary

Mnemosyne ships as a single, self-contained binary. No dependencies, no installers.

**Windows (3.4 MB):**
```bash
curl -LO https://mnemosyne.dev/releases/latest/mnemosyne-windows.exe
mv mnemosyne-windows.exe mnemosyne.exe
```

**Linux (3.7 MB, static musl — any distro):**
```bash
curl -LO https://mnemosyne.dev/releases/latest/mnemosyne-linux
chmod +x mnemosyne-linux && mv mnemosyne-linux /usr/local/bin/mnemosyne
```

**macOS (build from source):**
```bash
git clone https://github.com/anckursingh/mnemosyne
cd mnemosyne && cargo build --release -p mnemosyne-mcp
```

### Verify

```bash
mnemosyne --version
# mnemosyne-mcp 0.1.0
```

## 5-Second Start

### Interactive Shell

```bash
mnemosyne shell :memory:
```
```
Mnemosyne> CREATE Person name == "Alice", role == "Architect"
Created: 019fdc... (v1)

Mnemosyne> MATCH Person RETURN *
── 1 row(s) ──
  019fdc...  v1  Person   Alice Architect

Mnemosyne> .tables
  Person

Mnemosyne> .exit
Bye.
```

### MCP Server (stdio mode)

```bash
mnemosyne serve ./my-knowledge.redb
```

Connects via stdin/stdout — perfect for Claude Code, VS Code, and other MCP clients. Add to your MCP config:

```json
{
  "mcpServers": {
    "mnemosyne": {
      "command": "mnemosyne",
      "args": ["serve", "./my-knowledge.redb"]
    }
  }
}
```

### TCP Server + Web UI

```bash
mnemosyne serve --listen 127.0.0.1:9090 --metrics-addr 127.0.0.1:9091 ./my-knowledge.redb
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
  -d '{"type_name":"Note","properties":{"body":"Hello Mnemosyne"}}'

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
mnemosyne shell ./kb.redb

# Create objects
Mnemosyne> CREATE Employee name == "Alice", dept == "Engineering", salary == 125000

# Search
Mnemosyne> MATCH Employee WHERE dept == "Engineering" RETURN name, salary

# Backup
Mnemosyne> .backup

# See all commands
Mnemosyne> .help
```

## Connecting from Code

### Python
```python
import mnemosyne_py
kernel = mnemosyne_py.Kernel.open("./kb.redb")
result = kernel.remember({"type_name": "Note", "properties": {"body": "Hello"}})
```

### TypeScript
```typescript
import { MnemosyneClient } from 'mnemosyne-sdk';
const client = new MnemosyneClient({ command: './mnemosyne' });
await client.connect();
await client.remember({ type_name: 'Note', properties: { body: 'Hello' } });
```

### Go
```go
import "github.com/ancku/mnemosyne-sdk"
client := mnemosyne.NewClient("127.0.0.1:9090")
client.Connect()
result, _ := client.Remember(map[string]interface{}{"type_name": "Note"})
```

## Encryption (Optional)

Enable encryption at rest:

```bash
# Generate a master key
mnemosyne keygen ./master.key

# Set environment variable
export MNEMOSYNE_PASSPHRASE="your-secure-passphrase"

# Start with encryption
mnemosyne serve --listen :9090 --metrics-addr :9091 ./encrypted-kb.redb
```

See [Encryption Guide](/docs/guides/encryption) for details on key rotation, field-level encryption, and compliance.

## Next Steps

- [Architecture Overview](/docs/architecture) — Understanding the Knowledge OS
- [API Reference](/docs/api-reference) — All endpoints and tools
- [Import Data](/docs/guides/import) — PostgreSQL, SQLite, MongoDB, Neo4j
- [Programs-as-KOs](/docs/guides/programs) — Deploy your first knowledge program
