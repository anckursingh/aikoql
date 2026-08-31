// v0.3 dogfood gate (reviewer §17): aikoql's own repository as the knowledge
// universe. The repo (crates/+docs/+scripts) was ingested with `ingest-dir`;
// this script answers the 10 continuity questions over the real server and
// asserts each answer carries knowledge lineage — evidence, derivation,
// invalidation stamps, versions — not just retrieved snippets.
// Usage: node scripts/e2e-dogfood.js <binary> <db.redb> <doc-koid>
// <doc-koid> is the aikoql:ingested-directory KOID printed by ingest-dir.
const { spawn } = require('child_process');

const [bin, db, docKoid] = process.argv.slice(2);
if (!bin || !db || !docKoid) { console.error('usage: e2e-dogfood.js <binary> <db.redb> <doc-koid>'); process.exit(2); }

setTimeout(() => { console.error('e2e-dogfood: watchdog timeout (120s) — aborting'); process.exit(1); }, 120000).unref();

function client() {
  const child = spawn(bin, [db], { stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.on('data', (c) => process.stderr.write('[serve] ' + c));
  child.on('error', (e) => { console.error('e2e-dogfood: spawn failed:', e.message); process.exit(1); });
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
  if (!cond) { console.error('e2e-dogfood: FAIL ' + msg); process.exit(1); }
}

(async () => {
  const c = client();
  await c.call('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e-dogfood', version: '0' } }).catch(() => {});
  const subject = 'ingest-dir'; // the repo ingest owns its KOs
  const ql = async (query) => (await c.call('aikoql', { subject, query })).results;
  const nowMs = Date.now();

  // ---- Q1. What is currently true? (default MATCH = valid-at-now, stamped) --
  const structs = await ql('MATCH Struct RETURN *');
  assert(structs.length > 0, 'ingest must have produced Struct entities');
  const stamped = structs.filter((r) => r.extensions && r.extensions.epistemic_status);
  assert(stamped.length === structs.length, 'every ingested entity must carry epistemic_status');
  const entity = structs[0];
  const entityKoid = entity.koid;
  console.log(`dogfood Q1: ${structs.length} Struct entities, all stamped (e.g. ${entity.properties.name || entityKoid})`);

  // ---- Q2/Q3. What existed before / when did it change? (HISTORICAL, AS_OF) -
  const histAll = await ql('MATCH Struct HISTORICAL RETURN *');
  const hist = histAll.filter((r) => r.koid === entityKoid);
  assert(hist.length >= 1, 'HISTORICAL must reconstruct the committed versions');
  const versions = hist.map((r) => r.version);
  assert(JSON.stringify(versions) === JSON.stringify([...versions].sort((a, b) => a - b)),
    'HISTORICAL versions must ascend in commit order');
  assert((await ql('MATCH Struct AS_OF 1 RETURN *')).every((r) => r.koid !== entityKoid),
    'AS_OF 1 must not see entities committed later');
  assert((await ql(`MATCH Struct AS_OF ${nowMs + 60000} RETURN *`)).some((r) => r.koid === entityKoid),
    'AS_OF now must see the committed entity');
  console.log(`dogfood Q2/Q3: HISTORICAL ${hist.length} version(s), AS_OF reconstructs both sides`);

  // Real change story (Agent 2 changes Kafka -> Pulsar, dogfood edition):
  // two generations of a claim, then supersession ends generation 1, then a
  // versioned update of generation 2 exercises the multi-version history.
  const claimV1 = await c.call('remember', {
    subject, type_name: 'DogfoodClaim',
    properties: { text: 'the aikoql kernel commits under one pipe lock' },
    extensions: { valid_from: 0 },
  });
  const claimV2 = await c.call('remember', {
    subject, type_name: 'DogfoodClaim',
    properties: { text: 'the aikoql kernel commits under one pipe lock and captures agent experiences' },
  });
  const ts = await c.call('supersede', {
    subject, old: claimV1.koid, superseded_by: claimV2.koid,
    evidence: [{ source_artifact: 'k5-pr.md', method: 'human_provided', confidence: 0.9 }],
    reason: 'dogfood: experience capture landed in K5',
  });
  assert(ts.old === claimV1.koid && ts.new === claimV2.koid,
    'supersession must supersede generation 1 onto generation 2');
  const supersededKo = await c.call('get', { subject, koid: claimV1.koid });
  assert(typeof supersededKo.extensions.valid_to === 'number', 'supersession must stamp valid_to (when the change happened)');
  const claimV2b = await c.call('remember', {
    subject, koid: claimV2.koid, expected_version: 1, type_name: 'DogfoodClaim',
    properties: { text: 'the aikoql kernel commits under one pipe lock and captures agent experiences (v2)' },
  });
  assert(claimV2b.version === 2, 'the successor update must produce version 2');
  const claimHist = (await ql('MATCH DogfoodClaim HISTORICAL RETURN *')).filter((r) => r.koid === claimV2.koid);
  assert(JSON.stringify(claimHist.map((r) => r.version)) === '[1,2]',
    'the successor must keep both committed versions');
  assert(!(await ql('MATCH DogfoodClaim RETURN *')).some((r) => r.koid === claimV1.koid),
    'the superseded generation must drop out of current truth');
  console.log('dogfood Q3: change stamped at the transition instant; generation 1 superseded, generation 2 current (v1+v2)');

  // ---- Q4/Q5/Q6. Why / by whom / with which evidence? (trace lineage) -------
  const traceEntity = await c.call('trace', { subject, koid: entityKoid });
  assert(Array.isArray(traceEntity.evidence) && traceEntity.evidence.length > 0,
    'ingested entity must carry its evidence trail');
  const ev0 = traceEntity.evidence[0];
  assert(ev0.source_artifact && ev0.method && typeof ev0.confidence === 'number',
    'evidence must carry source_artifact/method/confidence — lineage, not a snippet');
  assert(traceEntity.versions.length >= 1, 'trace must expose the version history');
  assert(traceEntity.versions.every((v) => typeof v.commit_ts === 'number'),
    'versions must carry commit timestamps');
  console.log(`dogfood Q4-Q6: trace(evidence): ${ev0.source_artifact} via ${ev0.method} @ ${ev0.confidence}`);

  const derived = await c.call('derive', {
    subject, type_name: 'Finding',
    properties: { text: `ingested entity ${entityKoid} carries canonical evidence` },
    sources: [entityKoid],
    operation: 'dogfood_derivation', actor: 'dogfood-runner',
    reason: 'dogfood gate: verify derivation lineage end-to-end',
    evidence: [{ source_artifact: 'scripts/e2e-dogfood.js', method: 'agent_analysis', confidence: 0.8 }],
  });
  const traceDerived = await c.call('trace', { subject, koid: derived.koid });
  assert(traceDerived.derivation && traceDerived.derivation.sources.length === 1
    && traceDerived.derivation.sources[0].koid === entityKoid,
    'derivation must record FROM WHAT (sources)');
  assert(traceDerived.derivation.reason && traceDerived.derivation.actor,
    'derivation must answer WHY (reason) and BY WHOM (actor)');
  assert(typeof traceDerived.confidence.score === 'number', 'derivation must carry a confidence context');
  console.log(`dogfood Q4-Q6: derivation reason/actor/sources + confidence ${traceDerived.confidence.score}`);

  // ---- Q7/Q8. What is affected / what became stale? (invalidation sweep) ----
  const inv = await c.call('invalidate', {
    subject, koid: entityKoid,
    evidence: [{ source_artifact: 'scripts/e2e-dogfood.js', method: 'agent_analysis', confidence: 0.8 }],
    reason: 'dogfood gate: exercise the dependent sweep',
  });
  const swept = inv.invalidated_dependents || inv.invalidated || [];
  assert(swept.includes(derived.koid),
    `the derived dependent must be swept, got ${JSON.stringify(swept)}`);
  const traceStale = await c.call('trace', { subject, koid: derived.koid });
  assert(traceStale.invalidation && traceStale.invalidation.actor && traceStale.invalidation.reason,
    'the stale dependent must be stamped WHEN / BY WHOM / WHY');
  console.log(`dogfood Q7/Q8: ${swept.length} dependent(s) swept, invalidation stamp on the derived KO`);

  // ---- Q9/Q10. What experience applies / what to be careful about? ----------
  const exp = await c.call('record_experience', {
    subject, goal: 'answer the v0.3 continuity questions over the ingested aikoql repo',
    action: 'queried entities, traced lineage, derived and invalidated',
    outcome: 'all 10 questions answered with evidence chains',
    lesson: 'trust trace, not snippets',
    reuse_conditions: ['dogfood', 'repo'],
    evidence: [{ source_artifact: 'scripts/e2e-dogfood.js', method: 'agent_analysis', confidence: 0.8 }],
  });
  const found = await c.call('find_experiences', {
    subject, task: 'run the dogfood gate on the aikoql repo again', limit: 5,
  });
  assert(found.matches.length >= 1 && found.matches[0].koid === exp.koid,
    'the dogfood experience must match a reuse task');
  assert(found.matches[0].lesson === 'trust trace, not snippets',
    'the lesson must travel with the match');
  console.log(`dogfood Q9: experience matched with score ${found.matches[0].score}`);

  // The real ingested-directory KO (crates/) is the knowledge document — the
  // context compiler ranks the repo IR AND injects the matched experience.
  const ctx = await c.call('compile_context', {
    subject, koid: docKoid,
    task: 'run the dogfood gate on the aikoql repo again', token_budget: 2000,
  });
  const content = typeof ctx.content === 'string' ? ctx.content : JSON.stringify(ctx);
  assert(content.includes('Previous Agent Experience'),
    'compile_context must inject the experience section');
  assert(content.includes('trust trace, not snippets'),
    'the lesson must reach the next agent in the context package');
  assert(Array.isArray(ctx.experiences) && ctx.experiences.length >= 1,
    'compile_context must expose the matched experiences');
  console.log('dogfood Q10: experience injected into the next agent context package');

  c.end();
  console.log('e2e-dogfood: PASS — 10 continuity questions answered with knowledge lineage over the ingested repo');
  process.exit(0);
})().catch((e) => { console.error('e2e-dogfood: FAIL', e.message); process.exit(1); });
