#!/usr/bin/env node
// PRR-5 MCP smoke client: spawn a command (binary path or "npx aikoql-mcp"),
// speak stdio JSON-RPC — initialize -> tools/list -> one tools/call.
// Exits 0 only if the full round-trip succeeds. Not shipped in the npm tarball
// (package.json "files" lists run.js only).
const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');

const cmdline = process.argv.slice(2);
if (cmdline.length === 0) {
  console.error('usage: node smoke-mcp.js <command...>  (e.g. node smoke-mcp.js npx aikoql-mcp serve <db>)');
  process.exit(2);
}

// If the command looks like a binary path and no db was given, serve a temp db
// FILE under cwd (the db path is a file, not a dir — redb on a directory path
// fails with Access denied on Windows; cleaned up on exit).
let args = cmdline;
let smokeDb = null;
if (cmdline.length === 1 && !cmdline[0].startsWith('npx')) {
  // cmd.exe can't run relative paths without .\ — resolve to a drive-letter path.
  const bin = process.platform === 'win32' ? path.resolve(cmdline[0]) : cmdline[0];
  smokeDb = path.join(process.cwd(), `.aikoql-smoke-${process.pid}.redb`);
  args = [bin, 'serve', smokeDb];
}
const done = (code) => {
  if (smokeDb) { try { fs.rmSync(smokeDb, { force: true }); } catch (_) {} }
  process.exit(code);
};

const child = spawn(args[0], args.slice(1), { shell: true, stdio: ['pipe', 'pipe', 'inherit'] });
let nextId = 0;
const pending = new Map();
let buffer = '';
let exited = false;
const log = (msg) => console.error(`smoke: ${msg}`);

function send(method, params, id) {
  child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
  return id;
}

function request(method, params) {
  const id = ++nextId;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject, method });
    send(method, params, id);
  });
}

const timeout = setTimeout(() => {
  log('TIMEOUT after 120s — server did not complete the smoke round-trip');
  done(1);
}, 120000);

child.stdout.on('data', (chunk) => {
  buffer += chunk.toString('utf8');
  let nl;
  while ((nl = buffer.indexOf('\n')) !== -1) {
    const line = buffer.slice(0, nl).trim();
    buffer = buffer.slice(nl + 1);
    if (!line) continue;
    let msg;
    try { msg = JSON.parse(line); } catch (_) { log(`non-JSON line ignored: ${line.slice(0, 80)}`); continue; }
    if (msg.id !== undefined && pending.has(msg.id)) {
      const { resolve, reject, method } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(`${method}: ${JSON.stringify(msg.error)}`));
      else resolve(msg.result);
    } else if (msg.method) {
      log(`ignored server->client request/notification: ${msg.method}`);
    }
  }
});

let passed = false;
child.on('exit', (code) => {
  exited = true;
  clearTimeout(timeout);
  if (passed) done(0);
  else {
    log(`server exited early (code ${code}) — smoke failed`);
    done(code || 1);
  }
});

async function main() {
  log(`spawning: ${args.join(' ')}`);
  const init = await request('initialize', {
    protocolVersion: '2024-11-05',
    capabilities: {},
    clientInfo: { name: 'aikoql-packaging-smoke', version: '0' },
  });
  log(`initialize OK — server ${init.serverInfo?.name} ${init.serverInfo?.version}`);
  send('notifications/initialized', {}, null);

  const tools = await request('tools/list', {});
  const names = (tools.tools || []).map((t) => t.name);
  log(`tools/list OK — ${names.length} tools`);

  const call = names.includes('metrics') ? 'metrics'
             : names.includes('tool_health') ? 'tool_health'
             : names[0];
  if (!call) throw new Error('tools/list returned zero tools');
  const result = await request('tools/call', { name: call, arguments: {} });
  const err = result?.isError === true;
  log(`tools/call "${call}" ${err ? 'FAILED' : 'OK'}`);
  if (err) throw new Error(`tools/call ${call} returned isError with content: ${JSON.stringify(result.content).slice(0, 200)}`);

  passed = true;
  child.stdin.end();
  // Server closes stdio after stdin EOF; exit handler sees passed=true and
  // declares success. Timer is a fallback if the exit event never fires.
  setTimeout(() => done(0), 5000);
}

main().catch((e) => {
  clearTimeout(timeout);
  if (!exited) { try { child.kill(); } catch (_) {} }
  log(`SMOKE FAILED: ${e.message}`);
  done(1);
});
