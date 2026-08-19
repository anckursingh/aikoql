// v0.3 K5 acceptance: agent experience over a real server.
// Mirrors mcp_stdio::k5_experience_reuse_end_to_end: record_experience /
// find_experiences with evidence mandate, agent_derived authority,
// reuse-condition gating, ACL-scoped cross-agent reuse, compile_context
// injection, agent_memory TTL enforcement, execute_agent outcome capture.
// Usage: node scripts/e2e-k5-experience.js <binary> <db.redb>
const { spawn } = require('child_process');

const [bin, db] = process.argv.slice(2);
if (!bin || !db) { console.error('usage: e2e-k5-experience.js <binary> <db.redb>'); process.exit(2); }

setTimeout(() => { console.error('e2e-k5: watchdog timeout (60s) — aborting'); process.exit(1); }, 60000).unref();

function client() {
  const child = spawn(bin, [db], { stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.on('data', (c) => process.stderr.write('[serve] ' + c));
  child.on('error', (e) => { console.error('e2e-k5: spawn failed:', e.message); process.exit(1); });
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
  if (!cond) { console.error('e2e-k5: FAIL ' + msg); process.exit(1); }
}

(async () => {
  const c = client();
  await c.call('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e-k5', version: '0' } }).catch(() => {});

  // 1. record_experience: evidence mandatory at the protocol boundary.
  const bad = await c.rawCall('record_experience', {
    subject: 'alice', goal: 'refactor the rust parser',
    action: 'split the lexer', outcome: 'tests green',
  });
  assert(bad.isError === true, 'unbacked experience must fail');

  const r = await c.call('record_experience', {
    subject: 'alice', goal: 'refactor the rust parser', action: 'split the lexer',
    outcome: 'tests green', lesson: 'smaller functions first',
    reuse_conditions: ['rust', 'parser'],
    evidence: [{ source_artifact: 'run-log', method: 'agent_analysis' }],
    shared_with: ['bob'],
  });
  const eko = await c.call('get', { subject: 'alice', koid: r.koid });
  assert(eko.type_name === 'aikoql:experience', 'experience type');
  assert(eko.extensions.epistemic_status === 'asserted'
    && eko.extensions.authority === 'agent_derived', 'agent_derived authority');
  assert(typeof eko.extensions.valid_to === 'number', 'ttl-bounded validity');
  assert(eko.extensions.confidence.score === 0.5 && eko.extensions.confidence.confirmations === 0,
    'fresh capture starts at 0.5 / 0 confirmations');

  // 2. Cross-agent reuse: bob matches only under full condition coverage.
  const m = await c.call('find_experiences', {
    subject: 'bob', task: 'please refactor the rust parser again',
  });
  assert(m.matches.length === 1 && m.matches[0].koid === r.koid
    && m.matches[0].actor === 'alice', 'bob matches the shared experience');
  const none = await c.call('find_experiences', {
    subject: 'bob', task: 'refactor something else entirely',
  });
  assert(none.matches.length === 0, 'partial condition coverage gates out');
  const stranger = await c.call('find_experiences', {
    subject: 'carol', task: 'please refactor the rust parser again',
  });
  assert(stranger.matches.length === 0, 'no ACL grant, no reuse');

  // 3. compile_context injects the experiences section for a matching task.
  const kb = await c.call('remember', {
    subject: 'bob', type_name: 'knowledge_doc',
    properties: { ir_json: '{"entities":[],"relations":[],"facts":[],"events":[],"temporal":[],"document_id":null,"page_count":0,"extractor":""}' },
  });
  const ctxPkg = await c.call('compile_context', {
    subject: 'bob', koid: kb.koid, task: 'refactor the rust parser',
  });
  assert(ctxPkg.context_markdown.includes('Previous Agent Experience')
    && ctxPkg.experiences.length === 1 && ctxPkg.experiences[0].koid === r.koid,
    'compile_context carries the experience section');
  const ctxNone = await c.call('compile_context', {
    subject: 'bob', koid: kb.koid, task: 'paint the bikeshed',
  });
  assert(ctxNone.experiences.length === 0, 'no section for a non-matching task');

  // 4. agent_memory TTL enforcement at the read path.
  await c.call('agent_memory', { subject: 'alice', agent_id: 'alice', key: 'gone', value: 'expired', ttl: 0 });
  await c.call('agent_memory', { subject: 'alice', agent_id: 'alice', key: 'live', value: 'alive', ttl: 3600 });
  const mem = await c.call('agent_memory', { subject: 'alice', agent_id: 'alice' });
  assert(mem.count === 1 && mem.expired_dropped === 1 && mem.memories[0].key === 'live',
    'ttl=0 dropped, live memory returned');

  // 5. execute_agent captures the run as an experience (non-fatal hook).
  await c.call('deploy_program', {
    name: 'FindEngPeople', body: 'MATCH Person WHERE dept == "Eng" RETURN name',
    language: 'aikoql', subject: 'tester',
  });
  const agent = await c.call('deploy_agent', {
    name: 'HRAssistant', prompt: 'You help find people in the org.',
    skills: ['FindEngPeople'], tools: [], policies: [], subject: 'tester',
  });
  const result = await c.call('execute_agent', { koid: agent.koid, subject: 'tester' });
  const logText = result.execution_log.join('\n');
  assert(logText.includes('experience captured:'), 'run outcome captured');
  const own = await c.call('find_experiences', {
    subject: 'tester', task: 'find people in the org',
  });
  assert(own.matches.length === 1 && own.matches[0].actor === 'tester',
    'captured run is reusable by the executor');

  console.log('e2e-k5: PASS');
  c.end();
})().catch((e) => { console.error('e2e-k5: FAIL ' + e.message); process.exit(1); });
