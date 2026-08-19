// v0.3 K2 acceptance: temporal operators over a real server.
// Mirrors mcp_stdio::k2_temporal_operators_end_to_end: valid-time BETWEEN,
// transaction-time AS_OF/HISTORICAL, supersession validity ending + SUPERSEDES
// edge, and the EPISTEMIC protocol filter — all through tools/call.
// Usage: node scripts/e2e-k2-temporal.js <binary> <db.redb>
const { spawn } = require('child_process');

const [bin, db] = process.argv.slice(2);
if (!bin || !db) { console.error('usage: e2e-k2-temporal.js <binary> <db.redb>'); process.exit(2); }

setTimeout(() => { console.error('e2e-k2: watchdog timeout (60s) — aborting'); process.exit(1); }, 60000).unref();

function client() {
  const child = spawn(bin, [db], { stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.on('data', (c) => process.stderr.write('[serve] ' + c));
  child.on('error', (e) => { console.error('e2e-k2: spawn failed:', e.message); process.exit(1); });
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
    end: () => child.stdin.end(),
  };
}

function assert(cond, msg) {
  if (!cond) { console.error('e2e-k2: FAIL ' + msg); process.exit(1); }
}

(async () => {
  const c = client();
  await c.call('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e-k2', version: '0' } }).catch(() => {});
  const nowMs = Date.now();

  // 1. Two generations of a claim: old valid since epoch, new since Nov 2023.
  const old = await c.call('remember', {
    subject: 'alice', type_name: 'claim',
    properties: { text: 'we use kafka' }, extensions: { valid_from: 0 },
  });
  const oldKoid = old.koid;
  const fresh = await c.call('remember', {
    subject: 'alice', type_name: 'claim',
    properties: { text: 'we use rabbitmq' }, extensions: { valid_from: 1700000000000 },
  });
  const newKoid = fresh.koid;

  // call() already unwrapped content[0].text; aikoql's payload has .results.
  const ql = async (query) => (await c.call('aikoql', { subject: 'alice', query })).results;

  // 2. Default MATCH = current truth: both generations valid now.
  assert((await ql('MATCH claim RETURN *')).length === 2, 'default MATCH must see both generations');

  // 3. BETWEEN = valid-time overlap: only the old generation held [1000, 2000).
  const between = await ql('MATCH claim BETWEEN 1000 AND 2000 RETURN *');
  assert(between.length === 1 && between[0].properties.text === 'we use kafka',
    'BETWEEN must narrow to the old generation');

  // 4. AS_OF = transaction-time reconstruction: nothing existed at epoch 0.
  assert((await ql('MATCH claim AS_OF 0 RETURN *')).length === 0, 'AS_OF 0 must be empty');
  assert((await ql(`MATCH claim AS_OF ${nowMs + 60000} RETURN *`)).length === 2,
    'AS_OF now must see both generations');

  // 5. Supersession through the semantic op (review P0-1): validity ends
  // ~now, edge old -> new, evidence stamped on the old claim.
  const t = await c.call('supersede', {
    subject: 'alice', old: oldKoid, superseded_by: newKoid,
    evidence: [{ source_artifact: 'migration-runbook.md', method: 'runtime_observation', confidence: 0.95 }],
    reason: 'migrated to rabbitmq',
  });
  assert(t.old === oldKoid && t.new === newKoid,
    'supersede must supersede old onto the existing successor');
  const ko = await c.call('get', { subject: 'alice', koid: oldKoid });
  assert(ko.extensions.valid_to >= nowMs - 60000, 'supersession must end validity at ~now');
  const hits = await c.call('traverse', { subject: 'alice', koid: oldKoid, rel_type: 'supersedes' });
  assert(hits.hits.length === 1 && hits.hits[0].koid === newKoid, 'SUPERSEDES edge must point old -> new');

  // 6. Current truth excludes the superseded generation (runtime enforces).
  const current = await ql('MATCH claim RETURN *');
  assert(current.length === 1 && current[0].properties.text === 'we use rabbitmq',
    'superseded generation must drop out of current truth');

  // 7. HISTORICAL reconstructs every committed version: old appears three
  // times (created + superseded + evidence stamp), new once.
  const hist = await ql('MATCH claim HISTORICAL RETURN *');
  assert(hist.length === 4, `HISTORICAL must return 4 versions, got ${hist.length}`);
  const oldVersions = hist.filter((r) => r.koid === oldKoid).map((r) => r.version);
  assert(JSON.stringify(oldVersions) === '[1,2,3]', 'old versions must ascend in commit order');

  // 8. EPISTEMIC filter: the successor passes review (semantic verification),
  // verified only.
  await c.call('verify_knowledge', {
    subject: 'alice', koid: newKoid,
    evidence: [{ source_artifact: 'ops-review.md', method: 'human_provided', confidence: 0.9 }],
    note: 'ops review',
  });
  const verified = await ql('MATCH claim EPISTEMIC verified RETURN *');
  assert(verified.length === 1 && verified[0].koid === newKoid, 'EPISTEMIC verified must return only the successor');

  c.end();
  console.log('e2e-k2: PASS (BETWEEN/AS_OF/HISTORICAL + supersession + EPISTEMIC filter)');
  process.exit(0);
})().catch((e) => { console.error('e2e-k2: FAIL', e.message); process.exit(1); });
