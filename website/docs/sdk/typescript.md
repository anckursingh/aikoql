---
title: TypeScript SDK
description: MCP JSON-RPC client for Node.js
---

# TypeScript SDK

```typescript
import { MnemosyneClient } from 'mnemosyne-sdk';

const client = new MnemosyneClient({ command: './mnemosyne' });
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

// AIKOQL
const rows = await client.aikoql({
  query: 'MATCH Employee RETURN name, role',
});
```

Zero runtime dependencies. Uses `node:child_process` for MCP stdio transport.
