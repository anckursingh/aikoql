// §23 certification artifacts generator (MVP-QA-001).
//
// Reads the §9.1 per-ID registry in docs/TESTING-PLAN.md (single source of
// truth — the QA lead verifies each row), computes the §24 gate verdicts,
// and writes the five required artifacts:
//
//   artifacts/mvp-test-report.md      human-readable rollup
//   artifacts/mvp-test-results.json   machine-readable per-test results
//   artifacts/failed-tests.md         non-pass rows with root-cause notes
//   artifacts/benchmark-results.md    pinned baselines (cross-checked vs §3)
//   artifacts/release-gate.md         the exact §23 gate block
//
// Statuses are never invented: ✅ pass, 🟡 open, ❌ not-implemented/blocked
// come straight from the registry. The spec's execution rules forbid marking
// unimplemented functionality PASS, so connector rows keep the verdict NO-GO
// until the §9.3 scope decision or TP-5 lands.
//
// Usage:
//   node scripts/certify.js            generate artifacts/ (deterministic)
//   node scripts/certify.js --self-test  pin the decision logic on fixtures
//   node scripts/certify.js --check    regenerate + git-diff: stale artifacts
//                                      in the repo fail the check
const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');

const ROOT = path.join(__dirname, '..');
const PLAN = path.join(ROOT, 'docs', 'TESTING-PLAN.md');
const OUT = path.join(ROOT, 'artifacts');

// Pinned §3 baseline headlines. Each `check` substring must still be present
// in TESTING-PLAN.md — if the plan's pinned numbers are edited away, the
// bench gate flips to FAIL instead of silently citing a stale baseline.
const BENCH_HEADLINES = [
  { name: 'Track-B knowledge bench (§30)', headline: 'AikoQL 13/14 vs RAG 9/14 (measured 2026-08-22)', check: 'AikoQL 13/14 vs RAG 9/14' },
  { name: 'Agent efficacy G10 (§31, 51 tasks)', headline: 'canonical A 0.059 / B 0.569 / C 0.510 / D 1.000 — 51/51 at 0 LLM calls (measured 2026-08-24)', check: 'D 1.000 — 51/51' },
  { name: 'Chatbot comparative G11 (§52)', headline: 'accuracy A 0/16, B 10/16, C 13/16, D 15/16 (measured 2026-08-22)', check: 'D 15/16' },
  { name: 'Agent memory §32', headline: 'D 20/20 vs conventional B 12/20 (measured 2026-08-24)', check: 'D 20/20 vs conventional' },
];

// Sev-1/Sev-2 come from the GATE-10/11 readout in TESTING-PLAN §9.2 — no
// open severity-1/2 bugs today. Flip these when the readout changes.
const SEV = { sev1: 0, sev2: 0 };

function parseRegistry(text) {
  const m = text.match(/### 9\.1 Per-ID mapping\s*\n([\s\S]*?)\n### 9\.2/);
  if (!m) throw new Error('TESTING-PLAN §9.1 table not found');
  const glyphs = { '✅': 'pass', '🟡': 'open', '❌': 'blocked' };
  const rows = [];
  for (const line of m[1].split('\n')) {
    if (!line.trim().startsWith('|')) continue;
    const c = line.split('|').map((s) => s.trim());
    if (c.length < 6) continue;
    const [id, pri, gate, glyph, evidence] = [c[1], c[2], c[3], c[4], c[5]];
    if (id === 'MVP ID' || /^-+$/.test(id)) continue;
    let status = null;
    for (const [g, s] of Object.entries(glyphs)) if (glyph.startsWith(g)) status = s;
    if (!status) throw new Error(`unknown status glyph in row ${id}: ${glyph}`);
    if (status === 'pass' && glyph !== '✅') status = 'open'; // "✅ except …" = partial → never counts as a full pass
    if (status === 'blocked' && evidence.includes('NOT_IMPLEMENTED')) status = 'not_implemented';
    rows.push({ id, pri, gate: gate === '—' ? '' : gate, status, evidence });
  }
  if (rows.length < 10) throw new Error(`§9.1 parsed only ${rows.length} rows — table broken?`);
  return rows;
}

function areaOf(row) {
  if (row.id.startsWith('MVP-CON')) return 'Connectors';
  if (row.id.startsWith('MVP-EXT')) return 'Evidence';
  if (row.id.startsWith('MVP-EVO')) return 'Evolution';
  if (row.id.startsWith('MVP-TEMP')) return 'Temporal';
  if (row.id.startsWith('MVP-REC')) return 'Recovery';
  if (row.id.startsWith('MVP-DEP')) return 'Docker';
  if (row.id.startsWith('MVP-E2E')) return 'E2E';
  if (row.gate === 'G3') return 'Security';
  if (row.gate === 'G4') return 'Connectors';
  return null;
}

function verdict(rows, benchOk, sev) {
  const inPri = (pri, p) => pri.split('/').includes(p);
  const pct = (rs) =>
    rs.length ? Math.round((100 * rs.filter((r) => r.status === 'pass').length) / rs.length) : 100;
  const p0 = rows.filter((r) => inPri(r.pri, 'P0'));
  const p1 = rows.filter((r) => inPri(r.pri, 'P1'));
  const p0Pct = pct(p0);
  const p1Pct = pct(p1);

  const areas = {};
  for (const row of rows) {
    const a = areaOf(row);
    if (!a) continue;
    if (!(a in areas)) areas[a] = true;
    if (row.status !== 'pass') areas[a] = false;
  }
  const areaLine = (a) => (a in areas && areas[a] ? 'PASS' : 'FAIL');

  const blocking = [];
  for (const r of rows) if (r.status !== 'pass') blocking.push(`${r.id} [${r.status}] — ${r.evidence.slice(0, 160)}`);
  if (p0Pct !== 100) blocking.push(`P0 at ${p0Pct}% (${p0.filter((r) => r.status === 'pass').length}/${p0.length} pass)`);
  if (p1Pct < 98) blocking.push(`P1 at ${p1Pct}% (${p1.filter((r) => r.status === 'pass').length}/${p1.length} pass)`);
  for (const a of Object.keys(areas)) if (!areas[a]) blocking.push(`${a} gate not fully passing`);
  if (sev.sev1 > 0) blocking.push(`Sev-1 open: ${sev.sev1}`);
  if (sev.sev2 > 0) blocking.push(`Sev-2 open: ${sev.sev2}`);
  if (!benchOk) blocking.push('pinned benchmark baseline(s) missing from TESTING-PLAN §3');

  const gate = {
    p0: p0Pct === 100 ? 'PASS' : `FAIL (${p0Pct}%)`,
    p1: p1Pct >= 98 ? 'PASS' : `FAIL (${p1Pct}%)`,
    security: areaLine('Security'),
    connectors: areaLine('Connectors'),
    evidence: areaLine('Evidence'),
    evolution: areaLine('Evolution'),
    temporal: areaLine('Temporal'),
    recovery: areaLine('Recovery'),
    docker: areaLine('Docker'),
    e2e: areaLine('E2E'),
  };
  const go =
    p0Pct === 100 &&
    p1Pct >= 98 &&
    Object.values(gate).slice(2).every((v) => v === 'PASS') &&
    sev.sev1 === 0 &&
    sev.sev2 === 0 &&
    benchOk;

  return {
    p0Pass: p0.filter((r) => r.status === 'pass').length,
    p0Total: p0.length,
    p1Pass: p1.filter((r) => r.status === 'pass').length,
    p1Total: p1.length,
    p0Pct, p1Pct, gate, go, blocking,
  };
}

function build(planText) {
  const rows = parseRegistry(planText);
  const benchOk = BENCH_HEADLINES.every((b) => planText.includes(b.check));
  return { rows, v: verdict(rows, benchOk, SEV), benchOk };
}

function render(planText) {
  const { rows, v, benchOk } = build(planText);
  // No revision stamp: artifacts must be a pure function of the plan +
  // script so `--check` can git-diff them. The repo revision they describe
  // is the commit they live in.
  const gen = 'generated from TESTING-PLAN.md §9.1 by scripts/certify.js';
  const statusName = { pass: 'PASS', open: 'OPEN', not_implemented: 'NOT_IMPLEMENTED', blocked: 'BLOCKED' };

  const report = [
    '# AikoQL MVP Test Report',
    '',
    `> ${gen}`,
    '',
    'Registry: `docs/TESTING-PLAN.md` §9.1 (MVP-QA-001, 45 test IDs + gates). Statuses are never invented — per the spec execution rules, unimplemented rows stay NOT_IMPLEMENTED/BLOCKED, never PASS.',
    '',
    '## Summary',
    '',
    `- P0: **${v.p0Pass}/${v.p0Total} pass (${v.p0Pct}%)**`,
    `- P1: **${v.p1Pass}/${v.p1Total} pass (${v.p1Pct}%)**`,
    `- Sev-1: ${SEV.sev1} · Sev-2: ${SEV.sev2}`,
    `- Benchmarks: ${benchOk ? 'pinned baselines present' : 'BASELINE MISSING — see benchmark-results.md'}`,
    `- **Final decision: ${v.go ? 'GO' : 'NO-GO'}** (${v.blocking.length} blocking item${v.blocking.length === 1 ? '' : 's'})`,
    '',
    '## Gate readout',
    '',
    '| Gate | Verdict |',
    '| --- | --- |',
    `| P0 correctness | ${v.gate.p0} |`,
    `| P1 correctness | ${v.gate.p1} |`,
    `| Security | ${v.gate.security} |`,
    `| Connectors | ${v.gate.connectors} |`,
    `| Evidence | ${v.gate.evidence} |`,
    `| Evolution | ${v.gate.evolution} |`,
    `| Temporal | ${v.gate.temporal} |`,
    `| Recovery | ${v.gate.recovery} |`,
    `| Docker | ${v.gate.docker} |`,
    `| E2E | ${v.gate.e2e} |`,
    '',
    '## Per-ID results',
    '',
    '| ID | Pri | Gate | Status | Evidence |',
    '| --- | --- | --- | --- | --- |',
    ...rows.map((r) => `| ${r.id} | ${r.pri} | ${r.gate || '—'} | ${statusName[r.status]} | ${r.evidence} |`),
    '',
    'See `failed-tests.md` for the non-pass rows and their root-cause notes.',
    '',
  ].join('\n');

  const results = {
    generated_by: 'scripts/certify.js',
    summary: {
      p0: { pass: v.p0Pass, total: v.p0Total, pct: v.p0Pct },
      p1: { pass: v.p1Pass, total: v.p1Total, pct: v.p1Pct },
      sev: SEV,
      benchmarks: benchOk ? 'pinned-baselines-present' : 'baseline-missing',
      final_decision: v.go ? 'GO' : 'NO-GO',
    },
    gates: v.gate,
    tests: rows.map((r) => ({ id: r.id, priority: r.pri, gate: r.gate || null, status: r.status, evidence: r.evidence })),
    blocking: v.blocking,
  };

  const failed = [
    '# Failed / Open Tests',
    '',
    `> ${gen}`,
    '',
    ...rows
      .filter((r) => r.status !== 'pass')
      .map((r) => `## ${r.id} — ${statusName[r.status]}\n\n- Priority: ${r.pri} · Gate: ${r.gate || '—'}\n- ${r.evidence}\n`),
    ...(v.blocking.length && v.blocking.some((b) => !rows.some((r) => b.startsWith(r.id)))
      ? ['## Gate-level blockers', '', ...v.blocking.map((b) => `- ${b}`), '']
      : []),
  ].join('\n');

  const bench = [
    '# Benchmark Results (pinned baselines)',
    '',
    `> ${gen}`,
    '',
    'G10/G11/G12/§32 canonical measurements, pinned in TESTING-PLAN §3 rows 127–130 (regression-guarded weekly in CI, >20% alert). These are the baselines the release gate compares against — not fresh measurements. Each headline below was cross-checked to still appear in the plan.',
    '',
    ...BENCH_HEADLINES.map((b) => `- **${b.name}**: ${b.headline}`),
    '',
    benchOk ? 'All pinned headlines present in TESTING-PLAN.md.' : '**FAIL: one or more pinned headlines are missing from TESTING-PLAN.md — the plan was edited without re-pinning. Update the plan or BENCH_HEADLINES.**',
    '',
  ].join('\n');

  const gate = [
    'AIKOQL MVP RELEASE CERTIFICATION',
    '',
    `P0:                 ${v.gate.p0}`,
    `P1:                 ${v.gate.p1}`,
    `Security:           ${v.gate.security}`,
    `Connectors:         ${v.gate.connectors}`,
    `Evidence:           ${v.gate.evidence}`,
    `Evolution:          ${v.gate.evolution}`,
    `Temporal:           ${v.gate.temporal}`,
    `Recovery:           ${v.gate.recovery}`,
    `Docker:             ${v.gate.docker}`,
    `E2E:                ${v.gate.e2e}`,
    '',
    `Sev-1:              ${SEV.sev1}`,
    `Sev-2:              ${SEV.sev2}`,
    '',
    'Final decision:',
    v.go ? 'GO' : 'NO-GO',
    '',
    'Blocking tests:',
    ...(v.blocking.length ? v.blocking : ['(none)']),
    '',
    `> ${gen}`,
    '',
  ].join('\n');

  return { report, results: JSON.stringify(results, null, 2) + '\n', failed, bench, gate };
}

function writeArtifacts(files) {
  // §23 requires these exact five filenames under artifacts/.
  const names = {
    report: 'mvp-test-report.md',
    results: 'mvp-test-results.json',
    failed: 'failed-tests.md',
    bench: 'benchmark-results.md',
    gate: 'release-gate.md',
  };
  fs.mkdirSync(OUT, { recursive: true });
  for (const [key, name] of Object.entries(names)) {
    if (!files[key]) throw new Error(`render() missing artifact ${key}`);
    fs.writeFileSync(path.join(OUT, name), files[key]);
  }
}

// Required §23 release-gate block: every line below must appear verbatim.
const GATE_FORMAT = [
  'AIKOQL MVP RELEASE CERTIFICATION',
  'P0:                 ',
  'P1:                 ',
  'Security:           ',
  'Connectors:         ',
  'Evidence:           ',
  'Evolution:          ',
  'Temporal:           ',
  'Recovery:           ',
  'Docker:             ',
  'E2E:                ',
  'Sev-1:              ',
  'Sev-2:              ',
  'Final decision:',
];

function validate(files) {
  for (const line of GATE_FORMAT) {
    if (!files.gate.includes(line)) throw new Error(`release-gate.md missing required line: ${JSON.stringify(line)}`);
  }
  if (!/Final decision:\nGO|Final decision:\nNO-GO/.test(files.gate)) throw new Error('release-gate.md missing GO/NO-GO verdict');
  JSON.parse(files.results); // machine-readable results must parse
  return true;
}

// ---- self-test: pin the decision logic on fixtures -------------------------

function selfTest() {
  const pass = (id, pri, gate) => ({ id, pri, gate, status: 'pass', evidence: 'ok' });
  const open = (id, pri, gate) => ({ id, pri, gate, status: 'open', evidence: 'open note' });
  const blocked = (id, pri, gate) => ({ id, pri, gate, status: 'not_implemented', evidence: 'NOT_IMPLEMENTED' });
  const cases = [
    ['all green → GO', [pass('MVP-KO-001', 'P0', 'G1'), pass('MVP-SEC-001', 'P0', 'G3'),
      pass('MVP-CON-001..004', 'P0', 'G4'), pass('MVP-EXT-001', 'P0', 'G5'), pass('MVP-EVO-001', 'P0', 'G6'),
      pass('MVP-TEMP-001..004', 'P0/P1', 'G7'), pass('MVP-REC-001', 'P0', 'G8'), pass('MVP-DEP-001', 'P1', 'G9'),
      pass('MVP-E2E-002', 'P0', 'G5'), pass('MVP-PRG-003', 'P1', '')], true, { sev1: 0, sev2: 0 }, true],
    ['blocked P0 connector → NO-GO + listed', [pass('MVP-KO-001', 'P0', 'G1'), pass('MVP-SEC-001', 'P0', 'G3'),
      blocked('MVP-CON-001..004', 'P0', 'G4')], true, { sev1: 0, sev2: 0 }, false],
    ['open P0 → NO-GO (open ≠ pass)', [pass('MVP-KO-001', 'P0', 'G1'), open('MVP-CON-006', 'P0', 'G3')],
      true, { sev1: 0, sev2: 0 }, false],
    ['P1 below 98% → NO-GO', [pass('MVP-KO-001', 'P0', 'G1'),
      ...Array.from({ length: 97 }, (_, i) => pass(`MVP-X-${i}`, 'P1', '')),
      open('MVP-X-97', 'P1', ''), open('MVP-X-98', 'P1', ''), open('MVP-X-99', 'P1', '')],
      true, { sev1: 0, sev2: 0 }, false],
    ['Sev-1 open → NO-GO', [pass('MVP-KO-001', 'P0', 'G1')], true, { sev1: 1, sev2: 0 }, false],
    ['missing baseline → NO-GO', [pass('MVP-KO-001', 'P0', 'G1')], false, { sev1: 0, sev2: 0 }, false],
  ];
  for (const [name, rows, benchOk, sev, want] of cases) {
    const v = verdict(rows, benchOk, sev);
    if (v.go !== want) {
      throw new Error(`self-test FAIL [${name}]: decision ${v.go}, expected ${want} — blocking: ${v.blocking.join('; ')}`);
    }
    if (!want && v.blocking.length === 0) throw new Error(`self-test FAIL [${name}]: NO-GO with empty blocking list`);
  }
  console.log(`certify: self-test PASS — ${cases.length} decision fixtures`);
}

// ---- main ------------------------------------------------------------------

const mode = process.argv[2] || '';
const planText = fs.readFileSync(PLAN, 'utf8');

if (mode === '--self-test') {
  selfTest();
  const files = render(planText);
  validate(files);
  console.log('certify: release-gate format validation PASS');
  process.exit(0);
}

const files = render(planText);
validate(files);
writeArtifacts(files);
console.log(`certify: wrote 5 artifacts to artifacts/ (decision: ${files.gate.includes('Final decision:\nGO') ? 'GO' : 'NO-GO'})`);

if (mode === '--check') {
  try {
    execSync(`git diff --exit-code -- artifacts/`, { cwd: ROOT, stdio: 'pipe' });
  } catch (_) {
    console.error('certify: stale artifacts — regenerate and commit');
    process.exit(1);
  }
  console.log('certify: artifacts up to date');
}
