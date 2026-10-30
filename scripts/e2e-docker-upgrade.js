// The /data/aikoql.redb → /data/aikoql upgrade, on real images: container A
// (the last release with the old layout) remembers a KO on a fresh volume;
// container B (the new image, its default CMD) must rename the database once
// and say so; container C (the new image, stdio on /data/aikoql) must
// resolve the same KOID with the same content. Proves docker-entrypoint.sh
// keeps the docs' promise — upgrades are pull + recreate.
// Usage: node scripts/e2e-docker-upgrade.js [new-image] [old-image]
const { spawn, execSync } = require('child_process');

const image = process.argv[2] || 'aikoql:test';
const old = process.argv[3] || 'ghcr.io/anckursingh/aikoql:0.2.3';
const VOLUME = 'aikoql-upgrade-test';

setTimeout(() => { console.error('e2e-docker-upgrade: watchdog timeout (180s) — aborting'); process.exit(1); }, 180000).unref();

function sh(cmd) { try { execSync(cmd, { stdio: 'ignore' }); } catch (_) { /* best-effort cleanup */ } }
function out(cmd) { return execSync(cmd, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }); }

function client(name, img, db) {
  const child = spawn('docker', ['run', '-i', '--rm', '--name', name, '-v', `${VOLUME}:/data`, img, db],
    { stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.on('data', (c) => process.stderr.write(`[${name}] ` + c));
  child.on('error', (e) => { console.error(`e2e-docker-upgrade: spawn ${name} failed:`, e.message); process.exit(1); });
  let nextId = 0; const pending = new Map(); let buffer = '';
  const req = (method, params) => new Promise((res, rej) => {
    const id = ++nextId;
    const timer = setTimeout(() => { pending.delete(id); rej(new Error(method + ' timed out (30s)')); }, 30000);
    pending.set(id, { res, rej, timer });
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
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
    init: () => req('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e-docker-upgrade', version: '0' } }),
    call: async (tool, args) => JSON.parse((await req('tools/call', { name: tool, arguments: args })).content[0].text),
    end: () => child.stdin.end(),
  };
}

function assert(cond, msg) {
  if (!cond) { console.error('e2e-docker-upgrade: FAIL ' + msg); cleanup(); process.exit(1); }
}
function cleanup() {
  sh('docker rm -f aikoql-up-a aikoql-up-b aikoql-up-c');
  sh(`docker volume rm -f ${VOLUME}`);
}
const ls = () => out(`docker run --rm -v ${VOLUME}:/data --entrypoint ls ${image} /data`).split(/\s+/).filter(Boolean);

(async () => {
  cleanup();

  // A: the old release writes under its layout.
  const a = client('aikoql-up-a', old, '/data/aikoql.redb');
  await a.init();
  const note = await a.call('remember', { subject: 'admin', type_name: 'note', properties: { body: 'upgrade proof' } });
  assert(!!note.koid, `remember must return a koid, got ${JSON.stringify(note)}`);
  a.end();
  sh('docker stop -t 2 aikoql-up-a');
  assert(ls().includes('aikoql.redb') && !ls().includes('aikoql'), `old layout expected, got ${ls()}`);

  // B: the new image, its default CMD, renames once.
  sh(`docker run -d --name aikoql-up-b -e AIKOQL_TCP_TOKEN=up:up:admin -v ${VOLUME}:/data ${image}`);
  let logs = '';
  for (let i = 0; i < 60 && !/server ready/.test(logs); i++) {
    execSync('sleep 1');
    logs = out('docker logs aikoql-up-b 2>&1').replace(/\x1b\[[0-9;]*m/g, ''); // tracing colours
  }
  assert(/moved the database \/data\/aikoql\.redb to \/data\/aikoql/.test(logs), `no rename log line:\n${logs}`);
  assert(/db=\/data\/aikoql\b(?!\.redb)/.test(logs), `B must serve /data/aikoql:\n${logs}`);
  sh('docker rm -f aikoql-up-b');
  assert(ls().includes('aikoql') && !ls().includes('aikoql.redb'), `renamed layout expected, got ${ls()}`);

  // C: the new image reads A's knowledge at the new path.
  const c = client('aikoql-up-c', image, '/data/aikoql');
  await c.init();
  const got = await c.call('get', { koid: note.koid, subject: 'admin' });
  assert(got.properties && got.properties.body === 'upgrade proof', `KO lost across the upgrade: ${JSON.stringify(got)}`);
  c.end();
  sh('docker stop -t 2 aikoql-up-c');

  cleanup();
  console.log('e2e-docker-upgrade: PASS — a 0.2.3 volume upgrades in place, its knowledge intact');
  process.exit(0);
})().catch((e) => { console.error('e2e-docker-upgrade: FAIL', e.message); cleanup(); process.exit(1); });
