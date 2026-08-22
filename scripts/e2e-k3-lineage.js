// v0.3 K3 acceptance: first-class derivation + lineage over a real server.
// Mirrors mcp_stdio::k3_derivation_and_lineage_end_to_end: derive through the
// protocol, all six lineage questions answerable from one trace call,
// DERIVED_FROM edges traversable from every premise, premise validation.
// Usage: node scripts/e2e-k3-lineage.js <binary> <db.redb>
const { spawn } = require('child_process');

const [bin, db] = process.argv.slice(2);
if (!bin || !db) { console.error('usage: e2e-k3-lineage.js <binary> <db.redb>'); process.exit(2); }

setTimeout(() => { console.error('e2e-k3: watchdog timeout (60s) — aborting'); process.exit(1); }, 60000).unref();

function client() {
  const child = spawn(bin, [db], { stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.on('data', (c) => process.stderr.write('[serve] ' + c));
  child.on('error', (e) => { console.error('e2e-k3: spawn failed:', e.message); process.exit(1); });
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
    // tools/call payloads are {content:[{text:"<json>"}]}; unwrap to the json.
    call: async (name, arguments_) => {
      const r = await req('tools/call', { name, arguments: arguments_ });
      return JSON.parse(r.content[0].text);
    },
    rawCall: (name, arguments_) => req('tools/call', { name, arguments: arguments_ }),
    end: () => child.stdin.end(),
  };
}

function assert(cond, msg) {
  if (!cond) { console.error('e2e-k3: FAIL ' + msg); process.exit(1); }
}

(async () => {
  const c = client();
  await c.call('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e-k3', version: '0' } }).catch(() => {});

  // 1. Two premise observations.
  const p1 = await c.call('remember', {
    subject: 'alice', type_name: 'observation',
    properties: { env: 'prod', cpu: 41 },
    extensions: { confidence: { score: 0.8, confirmations: 1 } },
  });
  const p2 = await c.call('remember', {
    subject: 'alice', type_name: 'observation',
    properties: { env: 'prod', cpu: 43 },
  });

  // 2. Derive a conclusion through the protocol.
  const d = await c.call('derive', {
    subject: 'alice', type_name: 'conclusion',
    properties: { env: 'prod', cpu_is_high: true },
    sources: [p1.koid, p2.koid],
    operation: 'inference', actor: 'agent-7', model: 'claude-sonnet-5',
    reason: 'two independent observations agree cpu is elevated',
    evidence: [{ source_artifact: 'monitoring/grafana', method: 'runtime_observation', location: 'prod cluster', confidence: 0.9 }],
  });

  // 3. The derivation record answers all six questions at the query boundary.
  const ko = await c.call('get', { subject: 'alice', koid: d.koid });
  const ext = ko.extensions;
  assert(ext.epistemic_status === 'inferred', 'Origin::Reason => Inferred baseline');
  const deriv = ext.derivation;
  assert(deriv.operation === 'inference', 'DERIVED HOW');
  assert(deriv.actor === 'agent-7', 'BY WHOM');
  assert(deriv.model === 'claude-sonnet-5', 'WITH WHICH MODEL');
  assert(deriv.reason === 'two independent observations agree cpu is elevated', 'WHY');
  assert(deriv.sources.length === 2
    && deriv.sources.includes(p1.koid) && deriv.sources.includes(p2.koid), 'FROM WHAT');
  assert(typeof deriv.timestamp === 'number', 'WHEN');
  assert(Math.abs(ext.confidence.score - 0.8) < 0.001 && ext.confidence.confirmations === 1,
    'baseline confidence from sources');

  // 4. DERIVED_FROM edges are traversable from every premise (K4 input).
  for (const p of [p1.koid, p2.koid]) {
    const hits = await c.call('traverse', { subject: 'alice', koid: p, rel_type: 'derived_from' });
    assert(hits.hits.length === 1 && hits.hits[0].koid === d.koid, 'edge from premise to conclusion');
  }

  // 5. trace answers all six questions in one call.
  const t = await c.call('trace', { subject: 'alice', koid: d.koid });
  const tr = t.derivation;
  assert(tr.operation === 'inference' && tr.actor === 'agent-7' && tr.model === 'claude-sonnet-5', 'trace derivation record');
  assert(tr.reason === 'two independent observations agree cpu is elevated', 'trace WHY');
  assert(tr.sources.length === 2 && tr.sources[0].type_name === 'observation', 'trace FROM WHAT');
  assert(t.evidence.length === 1 && t.evidence[0].source_artifact === 'monitoring/grafana', 'trace WITH WHICH EVIDENCE');
  assert(Math.abs(t.confidence.score - 0.8) < 0.001, 'trace confidence');

  // 6. Premise validation: deriving from a missing KO is a tool error.
  const bad = await c.rawCall('derive', {
    subject: 'alice', type_name: 'conclusion',
    sources: ['ffffffffffffffffffffffffffffffff'],
  });
  assert(bad.isError === true, 'missing premise must fail the derivation');

  c.end();
  console.log('e2e-k3: PASS (derive + six-question lineage + premise validation)');
  process.exit(0);
})().catch((e) => { console.error('e2e-k3: FAIL', e.message); process.exit(1); });
