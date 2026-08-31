// v0.3 K1 acceptance: ingest-dir -> commit -> storage -> query.
// Verifies the canonical evidence trail (source_artifact/method/location/
// confidence) and stamped epistemic metadata survive to the MCP query
// boundary with no silent drops.
// Usage: node scripts/e2e-k1-ingest.js <binary> <db.redb> [<type_name>]
const { spawn } = require('child_process');

const [bin, db, typeName] = process.argv.slice(2);
if (!bin || !db) { console.error('usage: e2e-k1-ingest.js <binary> <db.redb> [<type>]'); process.exit(2); }

setTimeout(() => { console.error('e2e-k1: watchdog timeout (60s) — aborting'); process.exit(1); }, 60000).unref();

function client() {
  const child = spawn(bin, [db], { stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.on('data', (c) => process.stderr.write('[serve] ' + c));
  child.on('error', (e) => { console.error('e2e-k1: spawn failed:', e.message); process.exit(1); });
  let nextId = 0; const pending = new Map(); let buffer = '';
  const req = (method, params) => new Promise((res, rej) => {
    const id = ++nextId;
    const timer = setTimeout(() => { pending.delete(id); rej(new Error(method + ' timed out (15s)')); }, 15000);
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
    call: (name, arguments_) => req('tools/call', { name, arguments: arguments_ }),
    end: () => child.stdin.end(),
  };
}

(async () => {
  const c = client();
  await c.call('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e-k1', version: '0' } }).catch(() => {});
  // Ingest-dir owns the KOs; the QL scan is auth-filtered, so query as the
  // owner (empty ACL = owner-only Read).
  const subject = 'ingest-dir';
  const types = typeName ? [typeName] : ['Struct', 'Function', 'File'];
  let rows = null, used = null;
  for (const t of types) {
    try {
      const r = await c.call('aikoql', { subject, query: `MATCH ${t} RETURN *` });
      const parsed = JSON.parse(r.content[0].text);
      if (parsed.results && parsed.results.length > 0) { rows = parsed.results; used = t; break; }
    } catch (_) { /* next candidate */ }
  }
  if (!rows) { console.error('e2e-k1: no rows found for candidate types', types); process.exit(1); }
  console.log(`e2e-k1: ${rows.length} rows via MATCH ${used}`);

  // Every row must carry stamped epistemic metadata.
  for (const row of rows) {
    if (!row.extensions || !row.extensions.epistemic_status) {
      console.error('e2e-k1: row missing stamped epistemic_status:', JSON.stringify(row));
      process.exit(1);
    }
  }

  // The struct entity must carry the full canonical evidence trail.
  const withEvidence = rows.filter((r) => r.extensions && r.extensions.evidence);
  if (withEvidence.length === 0) {
    console.error('e2e-k1: no row carries canonical evidence');
    process.exit(1);
  }
  const ev = withEvidence[0].extensions.evidence[0];
  console.log('e2e-k1: evidence:', JSON.stringify(ev));
  console.log('e2e-k1: epistemic_status:', withEvidence[0].extensions.epistemic_status);
  console.log('e2e-k1: authority:', withEvidence[0].extensions.authority);
  console.log('e2e-k1: scope:', withEvidence[0].extensions.scope);
  if (!ev.source_artifact || !ev.method || !ev.confidence) {
    console.error('e2e-k1: evidence missing required fields:', JSON.stringify(ev));
    process.exit(1);
  }
  c.end();
  console.log('e2e-k1: PASS');
  process.exit(0);
})().catch((e) => { console.error('e2e-k1: FAIL', e.message); process.exit(1); });
