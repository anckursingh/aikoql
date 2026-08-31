// v0.3 release gate — restart leg (reviewer MVP gate):
// Install → Start → Ingest → Search → Retrieve → RESTART → Data survives.
// Run AFTER scripts/e2e-dogfood.js on the same db: starts a FRESH server
// process and asserts the dogfood's committed writes (the supersession and
// the ingested corpus) are still queryable from disk.
// Usage: node scripts/e2e-restart.js <binary> <db.redb>
const { spawn } = require('child_process');

const [bin, db] = process.argv.slice(2);
if (!bin || !db) { console.error('usage: e2e-restart.js <binary> <db.redb>'); process.exit(2); }

setTimeout(() => { console.error('e2e-restart: watchdog timeout (60s) — aborting'); process.exit(1); }, 60000).unref();

function client() {
  const child = spawn(bin, [db], { stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.on('data', (c) => process.stderr.write('[serve] ' + c));
  child.on('error', (e) => { console.error('e2e-restart: spawn failed:', e.message); process.exit(1); });
  let nextId = 0; const pending = new Map(); let buffer = '';
  const req = (method, params) => new Promise((res, rej) => {
    const id = ++nextId;
    const timer = setTimeout(() => { pending.delete(id); rej(new Error(method + ' timed out (30s)')); }, 30000);
    pending.set(id, { res, rej, timer });
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n', (err) => {
      if (err) { clearTimeout(timer); pending.delete(id); rej(err); }
    });
  });
  child.stdout.on('data', (chunk) => {
    buffer += chunk.toString('utf8');
    let nl;
    while ((nl = buffer.indexOf('\n')) !== -1) {
      const line = buffer.slice(0, nl).trim(); buffer = buffer.slice(nl + 1);
      if (!line) continue;
      let msg; try { msg = JSON.parse(line); } catch (_) { continue; }
      if (msg.id !== undefined && pending.has(msg.id)) {
        const { res, rej, timer } = pending.get(msg.id);
        pending.delete(msg.id); clearTimeout(timer);
        msg.error ? rej(new Error(JSON.stringify(msg.error))) : res(msg.result);
      }
    }
  });
  return {
    call: async (name, arguments_) => {
      const r = await req('tools/call', { name, arguments: arguments_ });
      return JSON.parse(r.content[0].text);
    },
    end: () => child.stdin.end(),
  };
}

function assert(cond, msg) {
  if (!cond) { console.error('e2e-restart: FAIL ' + msg); process.exit(1); }
}

(async () => {
  const c = client();
  await c.call('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e-restart', version: '0' } }).catch(() => {});
  const subject = 'ingest-dir';
  const ql = async (query) => (await c.call('aikoql', { subject, query })).results;

  // The dogfood's final committed state: generation 2 current with its v2
  // text, generation 1 superseded out, and the ingested corpus intact.
  const claims = await ql('MATCH DogfoodClaim RETURN *');
  assert(claims.length === 1, `exactly 1 current DogfoodClaim after restart, got ${claims.length}`);
  assert(claims[0].properties.text.includes('(v2)'),
    'the v2 text must survive restart: ' + claims[0].properties.text);
  const structs = await ql('MATCH Struct RETURN *');
  assert(structs.length > 0, 'ingested Struct entities must survive restart');

  c.end();
  console.log('e2e-restart: PASS — dogfood writes + ingested corpus survive a fresh server process');
  process.exit(0);
})().catch((e) => { console.error('e2e-restart: FAIL', e.message); process.exit(1); });
