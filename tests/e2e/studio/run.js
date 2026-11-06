#!/usr/bin/env node
// Studio browser e2e: drives the real page in Chrome against a server it
// starts itself (temp database, free port, bootstrap admin). Not run in CI —
// scripts/prepush.sh runs it when a browser is available.
//
//   node tests/e2e/studio/run.js
//
// Env: AIKOQL_BIN (default target/debug/aikoql-mcp), CHROME_BIN (default: the
// usual macOS/Linux locations). Exit 0 = pass, 1 = failure, 77 = skipped (no
// binary or no browser — the reason is printed).
//
// What it proves that unit tests cannot: with CSP `script-src 'self'` an
// inline handler that was missed does not error — the control just does
// nothing. So every delegated control is CLICKED and must react, the named
// flows are checked end to end, and any CSP violation fails the run.
'use strict';
const { spawn } = require('child_process');
const fs = require('fs');
const net = require('net');
const os = require('os');
const path = require('path');

const ROOT = path.resolve(__dirname, '../../..');
const BIN = process.env.AIKOQL_BIN || path.join(ROOT, 'target/debug/aikoql-mcp');
const PW = 'e2e-pw-12345';
const HOSTILE = '<img src=x onerror=window.__pwned=1>';

function findChrome() {
  return [
    process.env.CHROME_BIN,
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
  ].filter(Boolean).find(p => fs.existsSync(p));
}
if (!fs.existsSync(BIN)) { console.log(`SKIP: no server binary at ${BIN} (cargo build -p aikoql-mcp)`); process.exit(77); }
const CHROME = findChrome();
if (!CHROME) { console.log('SKIP: no Chrome/Chromium found (set CHROME_BIN)'); process.exit(77); }
let chromium;
try { ({ chromium } = require('playwright-core')); } catch (_) {
  console.log('SKIP: playwright-core not installed (cd tests/e2e/studio && npm ci)'); process.exit(77);
}

const results = [];
const ok = (name, cond, detail) => results.push([cond ? 'PASS' : 'FAIL', name, cond ? '' : String(detail === undefined ? '' : detail)]);
const freePort = () => new Promise((res, rej) => {
  const s = net.createServer(); s.listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => res(p)); }); s.on('error', rej);
});
const sleep = ms => new Promise(r => setTimeout(r, ms));

async function main() {
  const port = await freePort();
  const BASE = `http://127.0.0.1:${port}`;
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aikoql-studio-e2e-'));
  const server = spawn(BIN, ['serve', '--metrics-addr', `127.0.0.1:${port}`, path.join(dir, 'db')], {
    env: { ...process.env, AIKOQL_ADMIN_PASSWORD: PW }, stdio: ['ignore', 'ignore', 'pipe'],
  });
  let serverLog = ''; server.stderr.on('data', d => { serverLog += d; });
  let browser;
  const cleanup = async () => {
    try { if (browser) await browser.close(); } catch (_) { /* best effort */ }
    server.kill();
    fs.rmSync(dir, { recursive: true, force: true });
  };
  try {
    for (let i = 0; i < 120; i++) { try { if ((await fetch(BASE + '/health')).ok) break; } catch (_) { /* not up yet */ } await sleep(500); if (i === 119) throw new Error('server never became healthy:\n' + serverLog); }

    // ---- seed over REST with the header (as Studio authenticates) --------
    const tok = (await (await fetch(BASE + '/api/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username: 'admin', password: PW }) })).json()).token;
    const H = { 'Content-Type': 'application/json', Authorization: 'Bearer ' + tok };
    const rem = async b => (await fetch(BASE + '/api/v1/remember', { method: 'POST', headers: H, body: JSON.stringify(b) })).json();
    await rem({ type_name: 'Person', properties: { name: HOSTILE, city: 'Paris' }, tenant: 't1' });
    await rem({ type_name: 'Company', properties: { name: 'Acme' }, tenant: 't1' });
    await rem({ type_name: 'Person', properties: { name: 'Solo' } });

    browser = await chromium.launch({ executablePath: CHROME, headless: true });
    const page = await (await browser.newContext()).newPage();
    const consoleErrors = [], requests = [], expected404 = new Set();
    let dialogs = 0;
    page.on('console', m => { if (m.type() === 'error') consoleErrors.push(m.text()); });
    page.on('pageerror', e => consoleErrors.push('pageerror: ' + e.message));
    page.on('request', r => requests.push(r.url()));
    page.on('dialog', d => { dialogs++; d.dismiss().catch(() => {}); });
    await page.addInitScript(() => {
      window.__csp = []; window.__mut = 0;
      document.addEventListener('securitypolicyviolation', e => window.__csp.push(e.violatedDirective + ' ' + e.blockedURI));
      new MutationObserver(() => { window.__mut++; }).observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    });

    const resp = await page.goto(BASE + '/studio');
    const csp = resp.headers()['content-security-policy'] || '';
    ok('CSP: scripts only from self, no unsafe-inline for scripts', /script-src 'self';/.test(csp) && !/script-src[^;]*unsafe-inline/.test(csp), csp);

    // ---- login through the real button -------------------------------------
    await page.fill('#login-user', 'admin'); await page.fill('#login-pass', PW);
    await page.click('[data-click="doLogin"]');
    await page.waitForSelector('#login-overlay', { state: 'hidden', timeout: 10000 }).catch(() => {});
    await page.waitForTimeout(1500);
    ok('login button works (delegated click)', await page.isHidden('#login-overlay'));
    const jar = await page.context().cookies();
    const sess = jar.find(c => c.name === 'aikoql_session');
    ok('session is an HttpOnly SameSite=Strict cookie', !!sess && sess.httpOnly && sess.sameSite === 'Strict', JSON.stringify(sess));
    ok('script cannot read the session', !(await page.evaluate(() => document.cookie)).includes('aikoql_session') && (await page.evaluate(() => typeof authToken === 'undefined' || authToken === null)));
    ok('vendored scripts loaded', await page.evaluate(() => typeof vis !== 'undefined' && !!document.querySelector('#query-editor .cm-editor')));

    // ---- query panel -------------------------------------------------------
    await page.click('#query-editor .cm-content');
    await page.keyboard.type('MATCH Person RETURN *');
    await page.click('[data-click="runQuery"]'); await page.waitForTimeout(1200);
    ok('Run: results table rendered', await page.evaluate(() => /Person|name/.test(document.getElementById('query-results').innerText)));
    await page.click('[data-click="explainQuery"]'); await page.waitForTimeout(800);
    ok('Explain reacts', await page.evaluate(() => document.getElementById('query-results').innerText.length > 0));
    await page.click('[data-click="streamQuery"]'); await page.waitForTimeout(1200);
    ok('Chunked view renders chunks', await page.evaluate(() => /Chunk/.test(document.getElementById('query-results').innerText)));
    await page.click('#query-editor .cm-content');
    await page.keyboard.press('Control+A'); await page.keyboard.type('MAT'); await page.keyboard.press('Control+Space');
    await page.waitForSelector('.cm-tooltip-autocomplete li', { timeout: 3000 }).catch(() => {});
    ok('editor completes keywords', (await page.$$eval('.cm-tooltip-autocomplete li', l => l.map(x => x.textContent))).some(t => t.includes('MATCH')));
    await page.keyboard.press('Escape');

    // ---- explorer: input filter, tree item click, KOID link ---------------
    await page.click('button[data-panel="explorer"]'); await page.waitForTimeout(900);
    const before = await page.$$eval('#explorer-tree-content .tree-item', e => e.length);
    await page.fill('#explorer-filter', 'Compan'); await page.waitForTimeout(400);
    const filtered = await page.$$eval('#explorer-tree-content .tree-item', e => e.length);
    ok('explorer filter (data-input) re-renders the tree', filtered < before, `before=${before} after=${filtered}`);
    await page.fill('#explorer-filter', '');
    await page.waitForTimeout(300);
    await page.click('#explorer-tree-content .tree-item.type-node[data-type="Person"]'); await page.waitForTimeout(900);
    ok('tree item click loads the type', await page.evaluate(() => /Person/.test(document.getElementById('explorer-detail').innerText)));
    await page.click('#explorer-detail a[data-click="openInInspector"]'); await page.waitForTimeout(600);
    ok('KOID link opens the Inspector with that KOID (and does not navigate)',
      await page.evaluate(() => document.getElementById('panel-inspector').classList.contains('active') && document.getElementById('inspector-search-input').value.length > 8 && !location.hash));

    // ---- inspector: input search, suggestion click -------------------------
    await page.fill('#inspector-search-input', 'Person'); await page.waitForTimeout(900);
    await page.click('#inspector-suggestions .suggest-item[data-click="inspectorLoadKOID"]'); await page.waitForTimeout(900);
    ok('inspector suggestion click loads the KO', await page.evaluate(() => /KOID/.test(document.getElementById('inspector-detail').innerText)));

    // ---- graph: select change, buttons, tooltip, detail close -------------
    await page.click('button[data-panel="graph"]'); await page.waitForTimeout(1500);
    const reqBefore = requests.filter(u => u.includes('/api/graph')).length;
    await page.selectOption('#graph-tenant', 't1'); await page.waitForTimeout(1200);
    ok('graph tenant select (data-change) reloads the graph', requests.filter(u => u.includes('/api/graph')).length > reqBefore);
    await page.click('[data-click="loadGraphAll"]'); await page.waitForTimeout(1200);
    const pos = await page.evaluate(() => { const id = graphNetwork.body.nodeIndices[0]; const p = graphNetwork.canvasToDOM(graphNetwork.getPositions([id])[id]); const r = document.querySelector('#graph-area canvas').getBoundingClientRect(); return { x: r.left + p.x, y: r.top + p.y }; });
    await page.mouse.move(pos.x - 30, pos.y - 30); await page.mouse.move(pos.x, pos.y); await page.waitForTimeout(900);
    const tip = await page.evaluate(() => { const t = document.querySelector('.vis-tooltip'); return t ? { imgs: t.querySelectorAll('img').length, bold: t.querySelectorAll('b').length } : null; });
    ok('graph tooltip is text with a bold type', !!tip && tip.imgs === 0 && tip.bold === 1, JSON.stringify(tip));
    await page.mouse.click(pos.x, pos.y); await page.waitForTimeout(900);
    if (await page.$('[data-click="closeDetail"]')) {
      await page.click('[data-click="closeDetail"]'); await page.waitForTimeout(300);
      ok('node-detail close button removes the detail', (await page.$('[data-click="closeDetail"]')) === null);
    }

    // ---- ontology: preview -> explicit save -> CAS save --------------------
    await page.click('button[data-panel="ontology"]'); await page.waitForTimeout(700);
    await page.click('[data-click="discoverOntology"]'); await page.waitForTimeout(1200);
    ok('Auto-Discover shows a proposal, nothing saved', /Proposal/.test(await page.innerText('#ontology-save-msg')) && !((await (await fetch(BASE + '/api/v1/ontology', { headers: H })).json()).data || {}).properties);
    await page.click('[data-click="discoverOntologySave"]'); await page.waitForTimeout(1500);
    ok('Save discovered ontology persists it', !!((await (await fetch(BASE + '/api/v1/ontology', { headers: H })).json()).data || {}).properties);
    const def = v => JSON.stringify({ namespace: 'e2e-ns', version: v, classes: [{ name: 'Widget' }], relationships: [], mappings: [] });
    await page.fill('#ontology-yaml-input', def('1')); await page.click('[data-click="saveOntology"]'); await page.waitForTimeout(1200);
    ok('first manual save', /saved/i.test(await page.innerText('#ontology-save-msg')), await page.innerText('#ontology-save-msg'));
    await page.fill('#ontology-yaml-input', def('2')); await page.click('[data-click="saveOntology"]'); await page.waitForTimeout(1200);
    ok('second save updates in place (CAS)', /revision 2/.test(await page.innerText('#ontology-save-msg')), await page.innerText('#ontology-save-msg'));
    const stale = await fetch(BASE + '/api/v1/ontology', { method: 'POST', headers: H, body: JSON.stringify({ definition: JSON.parse(def('9')), expected_version: 1 }) });
    ok('stale expected_version -> 409', stale.status === 409, stale.status);
    // switching tenant must swap the editor to that tenant's ontology (empty if
    // none), never leave another scope's text to be saved over it
    await page.evaluate(() => { const s = document.getElementById('ontology-tenant'); s.add(new Option('acme', 'acme')); });
    await page.selectOption('#ontology-tenant', 'acme'); await page.waitForTimeout(700);
    ok('tenant switch empties the editor when that tenant has no ontology', (await page.inputValue('#ontology-yaml-input')) === '', await page.inputValue('#ontology-yaml-input'));
    await page.fill('#ontology-yaml-input', def('t1').replace('e2e-ns', 'acme-ns')); await page.click('[data-click="saveOntology"]'); await page.waitForTimeout(1200);
    ok('tenant save loads its own definition back', /acme-ns/.test(await page.inputValue('#ontology-yaml-input')), await page.inputValue('#ontology-yaml-input'));
    await page.selectOption('#ontology-tenant', ''); await page.waitForTimeout(700);
    ok('switching back to Global shows the global ontology', /e2e-ns/.test(await page.inputValue('#ontology-yaml-input')), await page.inputValue('#ontology-yaml-input'));

    // ---- sweep: every delegated control, in every panel, must REACT -------
    const SKIP = new Set(['logout', 'adminRestoreBackup', 'signInSso', 'downloadAuditExport']);
    const panels = await page.$$eval('#sidebar nav button[data-panel]', b => b.map(x => x.dataset.panel));
    const reactions = [], unclickable = [];
    for (const name of panels) {
      await page.click(`button[data-panel="${name}"]`); await page.waitForTimeout(1800); // panels load their data on activation
      const actions = await page.$$eval(`#panel-${name} [data-click]`, els => els.filter(e => e.offsetParent !== null).map(e => e.getAttribute('data-click')));
      for (const a of [...new Set(actions)].filter(a => !SKIP.has(a))) {
        const mut0 = await page.evaluate(() => window.__mut), req0 = requests.length, dlg0 = dialogs;
        // an earlier click may have re-rendered this panel (e.g. a tenant drill-down
        // swaps the KOID-link table): a control that no longer exists is not dead
        if ((await page.locator(`#panel-${name} [data-click="${a}"]:visible`).count()) === 0) continue;
        let clickError = null;
        await page.locator(`#panel-${name} [data-click="${a}"]:visible`).first().click({ timeout: 3000 }).catch(e => { clickError = String(e.message).split('\n').filter(l => /intercepts|not visible|outside|detached|covered|waiting/.test(l)).slice(0, 3).join(' | ').trim() || String(e.message).split('\n')[0]; });
        await page.waitForTimeout(500);
        const mut1 = await page.evaluate(() => window.__mut);
        // an unclickable element is not a dead handler: report them apart
        if (clickError) unclickable.push(name + ':' + a + ' (' + clickError + ')');
        else reactions.push([name + ':' + a, mut1 > mut0 || requests.length > req0 || dialogs > dlg0]);
      }
    }
    ok(`every visible delegated control reacts (${reactions.length} clicked)`, reactions.length >= 15 && reactions.every(r => r[1]), 'dead: ' + reactions.filter(r => !r[1]).map(r => r[0]).join(', ') + ` (clicked ${reactions.length})`);
    ok('no control was visible but unclickable', unclickable.length === 0, unclickable.join(' ; '));

    // ---- documents: upload, a real status badge, a clean compile error -----
    await page.click('button[data-panel="documents"]'); await page.waitForTimeout(700);
    await page.setInputFiles('#doc-file-input', { name: 'shot.png', mimeType: 'image/png', buffer: Buffer.from('iVBORw0KGgo=', 'base64') });
    await page.click('[data-click="ingestDocument"]'); await page.waitForTimeout(2000);
    ok('ingested document is listed with a rendered status badge', await page.evaluate(() => !!document.querySelector('#documents-list td .badge') && !/<span/.test(document.getElementById('documents-list').innerText)), await page.innerText('#documents-list'));
    await page.click('[data-click="compileDocument"]'); await page.waitForTimeout(1500);
    ok('an uploaded image compiles', /Compilation Results/.test(await page.innerText('#doc-compile-result')) && !/Compile failed/.test(await page.innerText('#doc-compile-result')), await page.innerText('#doc-compile-result'));

    // ---- admin buttons that need a licence/side effect ---------------------
    await page.click('button[data-panel="admin"]'); await page.waitForTimeout(900);
    await page.click('[data-click="adminCreateBackup"]'); await page.waitForTimeout(900);
    ok('Create Backup shows a result', /Backup created|backup/i.test(await page.innerText('#admin-backup-result')), await page.innerText('#admin-backup-result'));
    await page.waitForTimeout(2200); // past the panel's own 1.5s reload
    ok('Create Backup message survives the panel reload', /Backup created/.test(await page.innerText('#admin-backup-result')), await page.innerText('#admin-backup-result'));
    const verify = await page.$('[data-click="adminVerifyBackup"]');
    if (verify) {
      await verify.click(); await page.waitForTimeout(1500);
      ok('Verify backup (data-name) reacts', /Backup .*:/.test(await page.innerText('#admin-backup-result')), await page.innerText('#admin-backup-result'));
    }

    // ---- security properties of the whole run ------------------------------
    ok('hostile record rendered inertly everywhere', (await page.evaluate(() => document.querySelectorAll('[onerror]').length)) === 0 && (await page.evaluate(() => window.__pwned || 0)) === 0);
    ok('no request carried ?token=', !requests.some(u => /[?&]token=/.test(u)), requests.filter(u => /token=/.test(u)).join(','));
    ok('no third-party request', requests.every(u => u.startsWith(BASE) || /^(data|blob):/.test(u)), requests.filter(u => !u.startsWith(BASE) && !/^(data|blob):/.test(u)).join(','));
    const violations = await page.evaluate(() => window.__csp);
    ok('no CSP violations (a violation = a handler/script CSP blocked)', violations.length === 0, violations.join(' ; '));
    const realErrors = consoleErrors.filter(e => !/Failed to load resource/.test(e));
    ok('no page/console errors', realErrors.length === 0, realErrors.slice(0, 4).join(' || '));

    // ---- a reload resumes the session from the cookie ----------------------
    await page.reload(); await page.waitForTimeout(1500);
    ok('reload resumes the signed-in session', await page.isHidden('#login-overlay') && /admin/.test(await page.innerText('#user-badge')), await page.innerText('#login-error').catch(() => ''));
    // ---- a dead cookie falls back to the form; signing in must still work ---
    await page.context().addCookies([{ name: 'aikoql_session', value: 'dead', url: BASE, httpOnly: true, sameSite: 'Strict' }]);
    await page.reload(); await page.waitForSelector('#login-overlay', { state: 'visible', timeout: 10000 });
    await page.fill('#login-user', 'admin'); await page.fill('#login-pass', PW);
    await page.click('[data-click="doLogin"]'); await page.waitForSelector('#login-overlay', { state: 'hidden', timeout: 10000 }).catch(() => {});
    ok('sign in again over a stale session cookie', await page.isHidden('#login-overlay'), await page.innerText('#login-error').catch(() => ''));
  } finally {
    await cleanup();
  }
}

main().then(() => {
  const failed = results.filter(r => r[0] === 'FAIL');
  for (const r of results) console.log(r[0], '-', r[1], r[2] ? `  [${r[2].slice(0, 220)}]` : '');
  console.log(`\n${results.length - failed.length}/${results.length} passed`);
  process.exit(failed.length ? 1 : 0);
}).catch(e => { console.error('HARNESS ERROR:', e); process.exit(1); });
