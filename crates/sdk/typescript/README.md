# @aikoql/client — TypeScript SDK

The first-party TypeScript client for the [aikoql](https://github.com/anckursingh/aikoql) knowledge database: the canonical Database API (remember, get, query, traverse, hybrid search, context compile, prepare/execute) over the MCP transport, with async/await, `AsyncIterable` streaming, `AbortSignal` cancellation, and a connection pool. Zero runtime dependencies.

```ts
import { Client } from "@aikoql/client";

const client = await Client.dial("127.0.0.1:9090", { token: "s3cret" });
const agent = await client.agent();
await agent.remember("person", { name: "ada" });
for await (const row of client.queryStream("MATCH person RETURN *")) { /* ... */ }
await client.close();
```

```bash
npm install @aikoql/client
```

- Runs on Node ≥ 24 (native type-stripping) with a browser-compatible core
- Tests: `AIKOQL_MCP_BIN=$PWD/target/release/aikoql-mcp node --test tests/`
- License: Apache-2.0
