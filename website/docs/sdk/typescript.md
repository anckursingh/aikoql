---
title: TypeScript SDK
description: MCP JSON-RPC client for Node.js
---

# TypeScript SDK

```typescript
import { AikoqlClient } from 'aikoql-sdk';

const client = new AikoqlClient({ command: './aikoql' });
await client.connect();

// Create
const result = await client.remember({
  type_name: 'Employee',
  properties: { name: 'Alice', role: 'Architect' },
  tenant: 'acme',
});
console.log(`Created: ${result.koid}`);

// Search
const results = await client.findSimilar({
  type_name: 'Employee',
  text: 'engineer',
});

// aikoql
const rows = await client.aikoql({
  query: 'MATCH Employee RETURN name, role',
});
```

Zero runtime dependencies. Uses `node:child_process` for MCP stdio transport.
