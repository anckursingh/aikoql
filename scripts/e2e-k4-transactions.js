// v0.3 K4 acceptance: knowledge transactions over a real server.
// Mirrors mcp_stdio::k4_knowledge_transactions_end_to_end: observe / assert /
// verify / contradict / supersede / merge / invalidate / resolve_conflict as
// first-class kernel ops with evidence mandates, conflict persistence,
// authority-ranked resolution, and DERIVED_FROM dependent sweeps.
// Usage: node scripts/e2e-k4-transactions.js <binary> <db.redb>
const { spawn } = require('child_process');

const [bin, db] = process.argv.slice(2);
if (!bin || !db) { console.error('usage: e2e-k4-transactions.js <binary> <db.redb>'); process.exit(2); }

setTimeout(() => { console.error('e2e-k4: watchdog timeout (60s) — aborting'); process.exit(1); }, 60000).unref();

function client() {
  const child = spawn(bin, [db], { stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.on('data', (c) => process.stderr.write('[serve] ' + c));
  child.on('error', (e) => { console.error('e2e-k4: spawn failed:', e.message); process.exit(1); });
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
  if (!cond) { console.error('e2e-k4: FAIL ' + msg); process.exit(1); }
}

(async () => {
  const c = client();
  await c.call('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e-k4', version: '0' } }).catch(() => {});

  // 1. assert_knowledge: authority + evidence mandatory, stamped on the KO.
  const a = await c.call('assert_knowledge', {
    subject: 'alice', type_name: 'claim',
    properties: { env: 'prod', cpu: 41 },
    authority: 'source_code',
    evidence: [{ source_artifact: 'src/main.rs', method: 'ast_extraction' }],
  });
  const ko = await c.call('get', { subject: 'alice', koid: a.koid });
  assert(ko.extensions.epistemic_status === 'asserted' && ko.extensions.authority === 'source_code',
    'assertion stamped');

  // 2. observe + verify_knowledge: verification bumps the confidence context.
  const o = await c.call('observe', {
    subject: 'alice', type_name: 'sighting',
    properties: { temp: 21 },
    evidence: [{ source_artifact: 'thermometer-1', method: 'runtime_observation' }],
  });
  const v = await c.call('verify_knowledge', {
    subject: 'alice', koid: o.koid,
    evidence: [{ source_artifact: 'ci-run-1', method: 'ci_observation' }],
  });
  assert(v.status === 'verified' && v.confirmations === 1, 'verify bumps confirmations');

  // 3. Derive a dependent from the claim (the invalidation input).
  const d = await c.call('derive', {
    subject: 'alice', type_name: 'conclusion',
    properties: { cpu_is_high: true },
    sources: [a.koid], operation: 'inference', reason: 'elevated cpu',
  });

  // 4. contradict: counter + persisted Conflict KO; original untouched.
  const cc = await c.call('contradict', {
    subject: 'alice', claim: a.koid,
    properties: { env: 'prod', cpu: 87 },
    authority: 'documentation',
    evidence: [{ source_artifact: 'ops-runbook', method: 'doc_extraction' }],
  });
  const ako = await c.call('get', { subject: 'alice', koid: a.koid });
  assert(ako.extensions.epistemic_status === 'asserted', 'original claim untouched by contradict');
  const cko = await c.call('get', { subject: 'alice', koid: cc.conflict });
  assert(cko.type_name === 'aikoql:conflict', 'Conflict KO type');
  assert(cko.extensions.resolution === 'unresolved', 'conflict starts unresolved');
  assert(cko.properties.claim_a === a.koid && cko.properties.claim_b === cc.counter,
    'conflict records both claims');
  assert(cko.extensions.assertions.a.authority === 'source_code'
    && cko.extensions.assertions.b.authority === 'documentation',
    'per-assertion authority snapshots');

  // 5. resolve_conflict_by_authority: source_code (7) beats documentation (3).
  const res = await c.call('resolve_conflict_by_authority', {
    subject: 'alice', koid: cc.conflict, rationale: 'code is ground truth',
  });
  assert(res.decision === 'resolved_a_preferred', 'authority-ranked decision');
  assert(res.effects.length === 1 && res.effects[0].koid === cc.counter
    && res.effects[0].status === 'contradicted', 'loser contradicted');

  // 6. supersede: old preserved + Superseded, dependent swept for staleness.
  const s = await c.call('supersede', {
    subject: 'alice', old: a.koid, type_name: 'claim',
    properties: { env: 'prod', cpu: 55 },
    evidence: [{ source_artifact: 're-measure', method: 'runtime_observation' }],
    reason: 'new measurement',
  });
  assert(s.invalidated_dependents.length === 1 && s.invalidated_dependents[0] === d.koid,
    'dependent swept by supersede');
  const ako2 = await c.call('get', { subject: 'alice', koid: a.koid });
  assert(ako2.extensions.epistemic_status === 'superseded' && typeof ako2.extensions.valid_to === 'number',
    'old claim superseded with valid_to');
  const dko = await c.call('get', { subject: 'alice', koid: d.koid });
  assert(dko.extensions.epistemic_status === 'inferred' && dko.extensions.invalidation,
    'dependent stamped invalidated, epistemic status untouched');

  // 7. trace answers INVALIDATED WHEN / BY WHOM / WHY.
  const t = await c.call('trace', { subject: 'alice', koid: d.koid });
  assert(t.invalidation && t.invalidation.actor === 'alice'
    && typeof t.invalidation.at === 'number' && t.invalidation.reason.length > 0,
    'trace invalidation section');

  // 8. merge: first-class derivation with operation "merge".
  const x = await c.call('assert_knowledge', {
    subject: 'alice', type_name: 'claim',
    properties: { region: 'us' },
    authority: 'ci_verified',
    evidence: [{ source_artifact: 'ci-log', method: 'ci_observation' }],
  });
  const m = await c.call('merge', {
    subject: 'alice', type_name: 'merged',
    sources: [s.new, x.koid], strategy: 'newest_wins',
    evidence: [{ source_artifact: 'merge-run', method: 'agent_analysis' }],
  });
  const mko = await c.call('get', { subject: 'alice', koid: m.koid });
  assert(mko.extensions.derivation.operation === 'merge', 'merge is a first-class derivation');
  assert(mko.properties.env === 'prod' && mko.properties.region === 'us', 'properties folded');

  // 9. invalidate: target Contradicted + chain sweep in BFS order (y and the
  // merged KO both derive from x).
  const y = await c.call('derive', {
    subject: 'alice', type_name: 'conclusion',
    properties: { region_is: 'us' },
    sources: [x.koid], operation: 'inference',
  });
  const inv = await c.call('invalidate', {
    subject: 'alice', koid: x.koid,
    evidence: [{ source_artifact: 'refuting-observation', method: 'runtime_observation' }],
    reason: 'premise refuted',
  });
  assert(inv.invalidated.length === 3 && inv.invalidated[0] === x.koid
    && inv.invalidated.includes(y.koid) && inv.invalidated.includes(m.koid),
    'target + both dependents invalidated');
  const xko = await c.call('get', { subject: 'alice', koid: x.koid });
  assert(xko.extensions.epistemic_status === 'contradicted'
    && xko.extensions.invalidation.reason === 'premise refuted', 'target contradicted + stamped');
  const yko = await c.call('get', { subject: 'alice', koid: y.koid });
  assert(yko.extensions.epistemic_status === 'inferred' && yko.extensions.invalidation,
    'dependent stamped, epistemic status untouched');

  // 10. Anti-CRUD-cosplay at the protocol boundary: unbacked ops fail.
  for (const [name, args] of [
    ['observe', { subject: 'alice', type_name: 'sighting', properties: { temp: 1 } }],
    ['assert_knowledge', { subject: 'alice', type_name: 'claim', properties: { x: 1 }, authority: 'source_code' }],
    ['verify_knowledge', { subject: 'alice', koid: o.koid }],
    // s.new is still current here — the failure must come from the evidence
    // mandate, not from the already-invalidated guard.
    ['invalidate', { subject: 'alice', koid: s.new }],
  ]) {
    const bad = await c.rawCall(name, args);
    assert(bad.isError === true, name + ' without evidence must fail');
  }

  // 11. Authority tie: an explicit decision is required — never a silent pick.
  const t1 = await c.call('assert_knowledge', {
    subject: 'alice', type_name: 'claim',
    properties: { p: 1 },
    authority: 'documentation',
    evidence: [{ source_artifact: 'doc-a', method: 'doc_extraction' }],
  });
  const tc = await c.call('contradict', {
    subject: 'alice', claim: t1.koid,
    properties: { p: 2 },
    authority: 'documentation',
    evidence: [{ source_artifact: 'doc-b', method: 'doc_extraction' }],
  });
  const tie = await c.rawCall('resolve_conflict_by_authority', {
    subject: 'alice', koid: tc.conflict, rationale: 'rank',
  });
  assert(tie.isError === true, 'authority tie must error');

  c.end();
  console.log('e2e-k4: PASS (observe/assert/verify/contradict/supersede/merge/invalidate/resolve + authority tie)');
  process.exit(0);
})().catch((e) => { console.error('e2e-k4: FAIL', e.message); process.exit(1); });
