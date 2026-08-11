# Aikoql Quickstart

aikoql is a knowledge database with built-in encryption, hybrid vector+text search, and an MCP (Model Context Protocol) interface for AI agent integration.

## 5-Second Start

```bash
# Download and run (stdio mode — perfect for MCP clients like Claude Code):
./aikoql-mcp

# Or TCP server mode (for multiple clients):
./aikoql-mcp --listen 127.0.0.1:9090 --metrics-addr 127.0.0.1:9091 ./data/aikoql.redb
```

## Usage Modes

### Stdio Mode (default)
The MCP server runs over stdin/stdout. Ideal for desktop AI tools (Claude Code, VS Code, etc.) that spawn the binary as a child process.

```
aikoql-mcp [database_path]
```

### TCP Mode
Accepts multiple MCP client connections over TCP.

```
aikoql-mcp --listen 127.0.0.1:9090 [database_path]
```

### TCP + Metrics (REST API + Studio)
Starts the HTTP server with REST API, health endpoints, and the Studio web UI:

```
aikoql-mcp --listen 127.0.0.1:9090 --metrics-addr 127.0.0.1:9091 [database_path]
```

### Metrics-Only Mode (Studio UI)
For local use where you only need the Studio web interface and REST API (no MCP over TCP):

```
aikoql-mcp ./aikoql.redb --metrics-addr 127.0.0.1:9191
```

> **Note:** In metrics-only mode, the process must have an open stdin to stay alive.
> Run with: `sleep 99999 | aikoql-mcp ./aikoql.redb --metrics-addr 127.0.0.1:9191`

Endpoints available on the metrics port:
| Endpoint | Description |
|----------|-------------|
| `GET /health` | Health check with uptime |
| `GET /metrics` | Prometheus-compatible metrics |
| `GET /studio` | **Studio web UI** (see below) |
| `POST /api/login` | Studio authentication |
| `POST /api/v1/documents` | Document upload + ingest |
| `POST /api/v1/documents/compile` | Run full D1-D9 pipeline |

## Studio UI

aikoql includes a built-in web-based Studio for visual knowledge management. No separate install — it's served directly by the binary.

### Starting Studio

```bash
# Start with metrics-addr (any port):
# Windows:
sleep 99999 | .\target\release\aikoql-mcp.exe .\aikoql.redb --metrics-addr 127.0.0.1:9191

# Linux:
sleep 99999 | ./aikoql-mcp ./aikoql.redb --metrics-addr 127.0.0.1:9191
```

Open **http://127.0.0.1:9191/studio** in your browser. Login with `admin` / `admin`.

### Studio Panels

| Panel | What it does |
|-------|-------------|
| **⌨ Query Editor** | Run aikoql queries, explain plans, stream results |
| **🕸 Knowledge Graph** | Visual graph traversal and exploration |
| **📁 Explorer** | Browse knowledge objects by type |
| **🏷 Schema** | View and manage type schemas |
| **🔬 Inspector** | Inspect individual knowledge objects |
| **🦉 Ontology** | Manage ontology classes, properties, relationships |
| **⚙ Admin** | System configuration, tenants, roles |
| **⏳ Timeline** | Temporal view of knowledge mutations |
| **🔗 Provenance** | Trace evidence chains and data lineage |
| **📄 Documents** | **Document ingestion and knowledge compilation** |

### Document Explorer Workflow

The **📄 Documents** panel implements the full D1-D9 pipeline:

1. **Upload** — Choose a PDF, DOCX, HTML, or TXT file
2. **Ingest** — Extracts text, detects OCR vs native, stores as knowledge object
3. **Compile** — Runs the full D1-D9 knowledge compiler pipeline:

| Phase | What happens |
|-------|-------------|
| D1-D2 | Physical analysis — text extraction + OCR detection |
| D3 | Document AST — structured block tree |
| D4 | Knowledge IR — entities, relations, facts, temporal assertions |
| D5 | Ontology proposals — class, property, relationship discovery |
| D6 | Entity resolution — match entities against existing knowledge base |
| D7 | Reconciliation — commit plan (create/update/skip/review) |
| D8 | Chunking + embedding — vector-ready document chunks |
| D9 | Compiler pipeline — orchestrates D3→D8 in one call |

The compilation results display all phases: stats, IR entities, ontology proposals, resolution matches, commit plan actions, evidence trail, and embedded chunks.

Try it with the sample invoice PDF at:
`C:/Users/ancku/CascadeProjects/ai-crm-platform/services/billing-processor/output/invoice_9655.pdf`

## Available MCP Tools (26 total)

| Category | Tools |
|----------|-------|
| **Knowledge CRUD** | `remember`, `forget`, `evolve`, `get` |
| **Search** | `find_similar` (vector+text hybrid), `aikoql` (query language) |
| **Graph** | `relate`, `traverse` |
| **Documents** | `document_ingest`, `document_status`, `document_compile` |
| **Agent Runtime** | `agent_memory`, `session_init`, `batch` |
| **Audit** | `trace`, `explain`, `prove`, `verify`, `audit_report` |
| **Backup** | `backup`, `verify_backup`, `restore`, `list_backups` |
| **Compliance** | `compliance_report` (encryption audit) |
| **Ops** | `health`, `metrics`, `ping`, `eval_recall`, `eval_staleness`, `eval_contradictions` |

### Document Pipeline Tools

| Tool | Input | Output |
|------|-------|--------|
| `document_ingest` | Base64-encoded file + MIME type | KOID of ingested document |
| `document_status` | KOID | Extraction status + page/chars/OCR stats |
| `document_compile` | KOID | Full `CompilationResult`: IR, ontology, resolution, commit plan, chunks, evidence trail, stats |

## Configuration

Copy `aikoql.toml` alongside the binary and edit values. CLI flags override config.

Environment variables:
- `RUST_LOG` — log level (trace, debug, info, warn, error). Default: info.
- `AIKOQL_PASSPHRASE` — KMS passphrase for encryption (if enabled).

## Encryption (MRFC-0020)

Encryption at rest is built-in but optional. To enable:

1. Set `encryption.enabled = true` in `aikoql.toml`
2. Set `encryption.key_path` to where the master key will be stored
3. Set `encryption.passphrase` or the `AIKOQL_PASSPHRASE` env var

The first run creates the key file. Subsequent runs use the existing key.
AES-256-GCM with envelope encryption (KEK→DEK→Data). Field-level encryption policies per schema type.

## Building from Source

Requires: Rust toolchain (https://rustup.rs). No other dependencies for Windows.

```bash
# Windows — fast path (just the MCP server):
cargo build --release -p aikoql-mcp
# → target/release/aikoql-mcp.exe

# Windows — full build with scripts:
scripts\build-release.bat

# Linux — native build:
cargo build --release -p aikoql-mcp
bash scripts/build-release.sh

# Linux — cross-compile from Windows:
# Requires x86_64-linux-musl-gcc (try WSL Ubuntu for native Linux builds).
rustup target add x86_64-unknown-linux-musl
cargo build --release -p aikoql-mcp --target x86_64-unknown-linux-musl
```

### Running Tests

```bash
# Unit + integration tests (MCP server):
cargo test -p aikoql-mcp -- --test-threads=1

# Ingestion pipeline tests (190+ tests):
cargo test -p aikoql-ingestion

# Multi-source ontology merge tests:
cargo test -p aikoql-ingestion --test multi_source_ontology

# E2E Playwright tests (requires npx playwright install):
cd tests/e2e && npx playwright test
```

## Connecting from Code

### TypeScript/JavaScript
```typescript
import { AikoqlClient } from './aikoql-sdk';

const client = new AikoqlClient({ command: './aikoql-mcp' });
await client.connect();
const result = await client.remember({ type_name: 'note', properties: { body: 'Hello' } });
```

### Python
```python
import aikoql_py

kernel = aikoql_py.Kernel.open("./aikoql.redb")
koid = kernel.remember({"type_name": "note", "properties": {"body": "Hello"}})
```

### Go
```go
client := aikoql.NewClient("./aikoql-mcp")
client.Connect()
client.Remember(aikoql.RememberRequest{...})
```

### Java
```java
AikoqlClient client = new AikoqlClient("./aikoql-mcp");
client.connect();
String result = client.remember("{\"type_name\": \"note\", ...}");
```

## Data Storage

By default, aikoql stores all data in a single [redb](https://github.com/cberner/redb) file. This is an embedded ACID-compliant database — no external database server required.

- Backups: `backup` tool creates verified snapshots. `restore` recovers with PITR metadata.
- Encryption: All data encrypted at rest when enabled (AES-256-GCM, ChaCha20-Poly1305 available).
- Audit: Immutable SHA-256 hash chain. Every mutation is journaled.

## Platform Support

| Platform | Binary | Status |
|----------|--------|--------|
| Windows 10/11 | `aikoql-mcp.exe` | ✅ Full (build + Studio + E2E) |
| Linux x86_64 | `aikoql-mcp` | ✅ Full (native build or cross-compile) |
| macOS (ARM/x86) | `aikoql-mcp` | Build from source |

## Next Steps

- Open **http://127.0.0.1:9191/studio** — explore the Studio UI
- Read [MRFC-0050](docs/MRFC-0050-Document-OCR-HLD-LLD.md) — document pipeline design
- Read [MRFC-0040](docs/MRFC-0040-Agent-Experience-Improvements.md) — agent runtime improvements
- Read [IMPLEMENTATION-PLAN.md](docs/IMPLEMENTATION-PLAN.md) — architecture overview
- Read [MRFC-0020](docs/MRFC-0020-Encryption-Key-Management-Architecture.md) — encryption design
- Run `aikoql-mcp --help` for all CLI options
