# Mnemosyne Quickstart

Mnemosyne is a knowledge database with built-in encryption, hybrid vector+text search, and an MCP (Model Context Protocol) interface for AI agent integration.

## 5-Second Start

```bash
# Download and run (stdio mode — perfect for MCP clients like Claude Code):
./mnemosyne-mcp

# Or TCP server mode (for multiple clients):
./mnemosyne-mcp --listen 127.0.0.1:9090 --metrics-addr 127.0.0.1:9091 ./data/mnemosyne.redb
```

## Usage Modes

### Stdio Mode (default)
The MCP server runs over stdin/stdout. Ideal for desktop AI tools (Claude Code, VS Code, etc.) that spawn the binary as a child process.

```
mnemosyne-mcp [database_path]
```

### TCP Mode
Accepts multiple MCP client connections over TCP.

```
mnemosyne-mcp --listen 127.0.0.1:9090 [database_path]
```

### TCP + Metrics
Adds a Prometheus-compatible HTTP metrics endpoint:

```
mnemosyne-mcp --listen 127.0.0.1:9090 --metrics-addr 127.0.0.1:9091 [database_path]
```

Health check: `GET http://127.0.0.1:9091/health`
Metrics: `GET http://127.0.0.1:9091/metrics`

## Available MCP Tools (23 total)

| Category | Tools |
|----------|-------|
| **Knowledge CRUD** | `remember`, `forget`, `evolve`, `get` |
| **Search** | `find_similar` (vector+text hybrid), `aikoql` (query language) |
| **Graph** | `relate`, `traverse` |
| **Audit** | `trace`, `explain`, `prove`, `verify`, `audit_report` |
| **Backup** | `backup`, `verify_backup`, `restore`, `list_backups` |
| **Compliance** | `compliance_report` (encryption audit) |
| **Ops** | `metrics`, `ping`, `eval_recall`, `eval_staleness`, `eval_contradictions` |

## Configuration

Copy `mnemosyne.toml` alongside the binary and edit values. CLI flags override config.

Environment variables:
- `RUST_LOG` — log level (trace, debug, info, warn, error). Default: info.
- `MNEMOSYNE_PASSPHRASE` — KMS passphrase for encryption (if enabled).

## Encryption (MRFC-0020)

Encryption at rest is built-in but optional. To enable:

1. Set `encryption.enabled = true` in `mnemosyne.toml`
2. Set `encryption.key_path` to where the master key will be stored
3. Set `encryption.passphrase` or the `MNEMOSYNE_PASSPHRASE` env var

The first run creates the key file. Subsequent runs use the existing key.
AES-256-GCM with envelope encryption (KEK→DEK→Data). Field-level encryption policies per schema type.

## Building from Source

```bash
# Windows:
scripts\build-release.bat

# Linux:
bash scripts/build-release.sh
```

Requires: Rust toolchain (https://rustup.rs). No other dependencies.

## Connecting from Code

### TypeScript/JavaScript
```typescript
import { MnemosyneClient } from './mnemosyne-sdk';

const client = new MnemosyneClient({ command: './mnemosyne-mcp' });
await client.connect();
const result = await client.remember({ type_name: 'note', properties: { body: 'Hello' } });
```

### Python
```python
import mnemosyne_py

kernel = mnemosyne_py.Kernel.open("./mnemosyne.redb")
koid = kernel.remember({"type_name": "note", "properties": {"body": "Hello"}})
```

### Go
```go
client := mnemosyne.NewClient("./mnemosyne-mcp")
client.Connect()
client.Remember(mnemosyne.RememberRequest{...})
```

### Java
```java
MnemosyneClient client = new MnemosyneClient("./mnemosyne-mcp");
client.connect();
String result = client.remember("{\"type_name\": \"note\", ...}");
```

## Data Storage

By default, Mnemosyne stores all data in a single [redb](https://github.com/cberner/redb) file. This is an embedded ACID-compliant database — no external database server required.

- Backups: `backup` tool creates verified snapshots. `restore` recovers with PITR metadata.
- Encryption: All data encrypted at rest when enabled (AES-256-GCM, ChaCha20-Poly1305 available).
- Audit: Immutable SHA-256 hash chain. Every mutation is journaled.

## Platform Support

| Platform | Binary | Status |
|----------|--------|--------|
| Windows 10/11 | `mnemosyne-mcp.exe` | ✅ |
| Linux x86_64 | `mnemosyne-mcp` | ✅ |
| macOS (ARM/x86) | `mnemosyne-mcp` | Build from source |

## Next Steps

- Read [IMPLEMENTATION-PLAN.md](docs/IMPLEMENTATION-PLAN.md) for architecture overview
- Read [MRFC-0020](docs/MRFC-0020-Encryption-Key-Management-Architecture.md) for encryption design
- Run `mnemosyne-mcp --help` for all CLI options
