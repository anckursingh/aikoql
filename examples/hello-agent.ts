// examples/hello-agent.ts — the fresh-developer seven-step flow from
// QUICKSTART.md, in TypeScript against the standard MCP client
// (@modelcontextprotocol/sdk). P3-M9: MCP is the blessed integration
// surface — no hand-rolled SDK to drift. The same flow's artifact laws
// run end-to-end as a cargo test:
// crates/services/api/mcp/tests/wave31_oss.rs.
//
// Run from the repo root (aikoql-mcp on PATH, or edit the command below):
//
//   npm i @modelcontextprotocol/sdk && npx tsx examples/hello-agent.ts

import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';

async function main() {
  // 1. install is `npm i -g aikoql-mcp` (or a released binary); the
  // transport below spawns `aikoql-mcp` from PATH.
  const client = new Client({ name: 'hello-agent', version: '1.0.0' });

  // 2. start: connect + MCP initialize handshake.
  await client.connect(
    new StdioClientTransport({ command: 'aikoql-mcp', args: ['serve', './kb'] }),
  );

  // The documented flow calls the real tool registry, verbatim.
  const remember = (args: Record<string, unknown>) =>
    client.callTool({ name: 'remember', arguments: args });
  const findSimilar = (args: Record<string, unknown>) =>
    client.callTool({ name: 'find_similar', arguments: args });
  const explain = (koid: string) =>
    client.callTool({ name: 'explain', arguments: { koid } });
  const trace = (koid: string) =>
    client.callTool({ name: 'trace', arguments: { koid } });
  const koidOf = (res: { content: Array<{ text: string }> }): string =>
    JSON.parse(res.content[0].text).koid;

  // 3. ingest: remember a note.
  const note = await remember({
    type_name: 'note',
    properties: { body: 'Hello from the aikoql quickstart.' },
  });
  console.log('remembered', koidOf(note));

  // 4. query: recall it.
  const found = await findSimilar({ text: 'quickstart', k: 5 });
  console.log('recall found', found);

  // 5. add a second source, then recall both.
  const note2 = await remember({
    type_name: 'note',
    properties: { body: 'Second source: ingestion extracts knowledge IR from documents.' },
  });
  const both = await findSimilar({ text: 'quickstart pipeline', k: 5 });
  console.log('two sources recall', both, 'second koid', koidOf(note2));

  // 6. a knowledge-backed agent: commit a claim under the agent's own
  // subject, then recall its own knowledge.
  await remember({
    subject: 'hello-agent',
    type_name: 'claim',
    properties: { body: 'agent believes the quickstart works' },
  });
  const agentRecall = await findSimilar({
    subject: 'hello-agent',
    text: 'agent believes',
    k: 5,
  });
  console.log('agent recall', agentRecall);

  // 7. debug: why does this object say what it says, and what is its
  // lineage?
  const why = await explain(koidOf(note));
  const lineage = await trace(koidOf(note));
  console.log('explain:', JSON.stringify(why));
  console.log('lineage versions:', JSON.stringify(lineage));
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
