// MVP-QA-001 MVP-DEP-003 — volume-backed container restart:
// container A remembers a KO on a named volume, container A is removed,
// container B (fresh container, SAME volume) must resolve the same KOID
// with the same content. Proves the container contract in the Dockerfile:
// everything mutable lives under /data (VOLUME /data), so a restart loses
// nothing.
// Usage: node scripts/e2e-volume-restart.js [image]
// The image must be built already (CI: `docker build -t aikoql:test .`).
const { spawn, execSync } = require('child_process');

const image = process.argv[2] || 'aikoql:test';
const VOLUME = 'aikoql-vol';

setTimeout(() => { console.error('e2e-volume-restart: watchdog timeout (90s) — aborting'); process.exit(1); }, 90000).unref();

function sh(cmd) { try { execSync(cmd, { stdio: 'ignore' }); } catch (_) { /* best-effort cleanup */ } }

function client(name) {
  const child = spawn('docker', [
    'run', '-i', '--rm', '--name', name,
    '-v', `${VOLUME}:/data`,
    image, '/data/aikoql.redb',
  ], { stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.on('data', (c) => process.stderr.write(`[${name}] ` + c));
  child.on('error', (e) => { console.error(`e2e-volume-restart: spawn ${name} failed:`, e.message); process.exit(1); });
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
    call: async (name_, arguments_) => {
      const r = await req('tools/call', { name: name_, arguments: arguments_ });
      return JSON.parse(r.content[0].text);
    },
    end: () => child.stdin.end(),
  };
}

function assert(cond, msg) {
  if (!cond) { console.error('e2e-volume-restart: FAIL ' + msg); process.exit(1); }
}

(async () => {
  sh(`docker rm -f aikoql-vol-a aikoql-vol-b`);
  sh(`docker volume rm -f ${VOLUME}`);

  // Phase 1: container A writes.
  const a = client('aikoql-vol-a');
  await a.call('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e-volume-restart', version: '0' } }).catch(() => {});
  const note = await a.call('remember', {
    subject: 'admin', type_name: 'note',
    properties: { body: 'volume restart proof', memo: 'dep003' },
  });
  const koid = note.koid;
  assert(!!koid, `remember must return a koid, got ${JSON.stringify(note)}`);
  a.end();
  sh('docker stop -t 1 aikoql-vol-a');

  // Phase 2: container A is gone; container B on the same volume reads.
  const b = client('aikoql-vol-b');
  await b.call('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e-volume-restart', version: '0' } }).catch(() => {});
  const fetched = await b.call('get', { koid, subject: 'admin' });
  assert(fetched.type_name === 'note', `restored KO must be a note, got ${JSON.stringify(fetched)}`);
  assert(fetched.properties.body === 'volume restart proof',
    `restored KO content must match, got ${fetched.properties.body}`);
  b.end();
  sh('docker stop -t 1 aikoql-vol-b');
  sh(`docker volume rm -f ${VOLUME}`);

  console.log('e2e-volume-restart: PASS — knowledge survives a volume-backed container restart');
  process.exit(0);
})().catch((e) => { console.error('e2e-volume-restart: FAIL', e.message); process.exit(1); });
