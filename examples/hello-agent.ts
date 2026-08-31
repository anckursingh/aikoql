// examples/hello-agent.ts — the fresh-developer seven-step flow from
// QUICKSTART.md, in TypeScript against the bundled SDK
// (crates/sdk/typescript). The same flow runs end-to-end as a cargo
// test: crates/services/api/mcp/tests/wave31_oss.rs.
//
// Run from the repo root (aikoql-mcp on PATH, or pass the binary path
// in the config below):
//
//   npx tsx examples/hello-agent.ts

import { AikoqlClient } from '../crates/sdk/typescript/src/index';

async function main() {
  // 1. install is `npm i -g aikoql-mcp` (or a released binary); the
  // client below spawns `aikoql-mcp` from PATH.
  const db = new AikoqlClient({ command: './aikoql-mcp' });

  // 2. start: connect + MCP initialize handshake.
  await db.connect();

  // 3. ingest: remember a note.
  const note = await db.remember({
    type_name: 'note',
    properties: { body: 'Hello from the aikoql quickstart.' },
  });
  console.log('remembered', note.koid);

  // 4. query: recall it.
  const found = await db.findSimilar({ text: 'quickstart', k: 5 });
  console.log('recall found', found.length, 'object(s)');

  // 5. add a second source, then recall both.
  const note2 = await db.remember({
    type_name: 'note',
    properties: { body: 'Second source: ingestion extracts knowledge IR from documents.' },
  });
  const both = await db.findSimilar({ text: 'quickstart pipeline', k: 5 });
  console.log('two sources recall', both.length, 'object(s); second koid', note2.koid);

  // 6. a knowledge-backed agent: commit a claim under the agent's own
  // subject, then recall its own knowledge.
  await db.remember({
    subject: 'hello-agent',
    type_name: 'claim',
    properties: { body: 'agent believes the quickstart works' },
  });
  const agentRecall = await db.findSimilar({ subject: 'hello-agent', text: 'agent believes', k: 5 });
  console.log('agent recall', agentRecall.length, 'object(s)');

  // 7. debug: why does this object say what it says, and what is its
  // lineage?
  const why = await db.explain(note.koid);
  const lineage = await db.trace(note.koid);
  console.log('explain:', JSON.stringify(why));
  console.log('lineage versions:', lineage.length);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
