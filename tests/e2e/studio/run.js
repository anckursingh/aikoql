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
const { spawn, execFileSync } = require('child_process');
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
const ok = (name, cond, detail) => {
  results.push([cond ? 'PASS' : 'FAIL', name, cond ? '' : String(detail === undefined ? '' : detail)]);
  if (process.env.E2E_TRACE) console.error((cond ? 'PASS' : 'FAIL') + ' - ' + name); // live, for finding a stuck step
};
const freePort = () => new Promise((res, rej) => {
  const s = net.createServer(); s.listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => res(p)); }); s.on('error', rej);
});
const sleep = ms => new Promise(r => setTimeout(r, ms));

// A scripted OpenAI-style model. It answers the query skill with a fixed
// MATCH (or a refusal for "unknowable") and the answer skill with a claim that
// quotes the one Company fact; the evidence id it cites is made up, because
// a real model is shown statements, not koids (the server attributes them).
function startChatStub(state) {
  const http = require('http');
  const queryFor = q => /unknowable/.test(q) ? { query: null, refusal_reason: 'cannot be answered' } : { query: 'MATCH Company RETURN *', refusal_reason: null };
  const answerFor = q => ({
    answer: /hostile/.test(q) ? HOSTILE : 'Acme is a company.', grounded: true,
    claims: [{ statement: 'Company Acme: name=Acme', evidence_ids: ['model-cannot-know-koids'] }], refusal_reason: null,
  });
  return new Promise(resolve => {
    const srv = http.createServer((req, resp) => {
      let body = '';
      req.on('data', d => { body += d; });
      req.on('end', async () => {
        let prompt = ''; try { prompt = JSON.parse(body).messages[0].content; } catch (_) { /* not our shape */ }
        const q = (/^Question: (.*)$/m.exec(prompt) || [])[1] || '';
        const reply = prompt.includes('Knowledge context:') ? answerFor(q) : queryFor(q);
        if (/slow/.test(q)) await sleep(900);
        resp.setHeader('Content-Type', 'application/json');
        resp.end(JSON.stringify({ choices: [{ message: { role: 'assistant', content: JSON.stringify(reply) } }] }));
      });
    });
    srv.listen(0, '127.0.0.1', () => resolve(srv));
  });
}

// A second server WITHOUT [chat]: the Ask panel must say so instead of offering a box.
async function checkChatDisabledNotice(browser) {
  const port = await freePort();
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aikoql-studio-e2e-nochat-'));
  const server = spawn(BIN, ['serve', '--metrics-addr', `127.0.0.1:${port}`, path.join(dir, 'db')], {
    env: { ...process.env, AIKOQL_ADMIN_PASSWORD: PW }, stdio: 'ignore',
  });
  try {
    const base = `http://127.0.0.1:${port}`;
    for (let i = 0; i < 120; i++) { try { if ((await fetch(base + '/health')).ok) break; } catch (_) { /* not up yet */ } await sleep(500); }
    const pg = await (await browser.newContext()).newPage();
    await pg.goto(base + '/studio');
    await pg.fill('#login-user', 'admin'); await pg.fill('#login-pass', PW);
    await pg.click('[data-click="doLogin"]'); await pg.waitForTimeout(1500);
    await pg.locator('button[data-panel="ask"]').click({ timeout: 2000 }).catch(() => {});
    await pg.waitForTimeout(800);
    const notice = await pg.locator('#ask-disabled:visible').count() ? await pg.innerText('#ask-disabled') : '';
    ok('Ask panel shows the not-configured notice when chat is disabled',
      /No model configured/.test(notice) && /docs\/CHAT\.md/.test(notice) && (await pg.locator('#ask-form:visible').count()) === 0, notice);
  } finally { server.kill(); fs.rmSync(dir, { recursive: true, force: true }); }
}

async function main() {
  const port = await freePort();
  const BASE = `http://127.0.0.1:${port}`;
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aikoql-studio-e2e-'));
  // max_pages = 2: the 4-slide deck below is read in two batches
  // Source-discovery fixtures: three SQLite databases built with python's stdlib.
  const mkdb = (file, sql) => execFileSync('python3', ['-c', 'import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript(sys.argv[2]); c.commit(); c.close()', file, sql]);
  const crmDb = path.join(dir, 'crm.sqlite'), shopDb = path.join(dir, 'shop.sqlite'), weirdDb = path.join(dir, 'weird.sqlite');
  mkdb(crmDb, "CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL); CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id), total REAL); INSERT INTO customers VALUES (1, 'a@x.test');");
  mkdb(shopDb, "CREATE TABLE clients (id INTEGER PRIMARY KEY, email_address TEXT NOT NULL); INSERT INTO clients VALUES (7, 'b@y.test'), (1, 'changed@y.test');");
  mkdb(weirdDb, 'CREATE TABLE "<img src=x onerror=window.__pwned=1>" (id INTEGER PRIMARY KEY, "<b>col</b>" TEXT);');
  const sourcesToml = ['crm', 'shop', 'weird'].map(n => `[[sources]]\nname = "${n}"\nkind = "sqlite"\nurl_env = "E2E_${n.toUpperCase()}_DB"\n`).join('\n');
  const chatState = { companyKoid: '' };
  const chatStub = await startChatStub(chatState);
  fs.writeFileSync(path.join(dir, 'aikoql.toml'), `[ingestion]\nmax_pages = 2\n[chat]\nurl = "http://127.0.0.1:${chatStub.address().port}"\nmodel = "e2e-stub"\n\n` + sourcesToml);
  const server = spawn(BIN, ['serve', '--config', path.join(dir, 'aikoql.toml'), '--metrics-addr', `127.0.0.1:${port}`, path.join(dir, 'db')], {
    env: { ...process.env, AIKOQL_ADMIN_PASSWORD: PW, E2E_CRM_DB: crmDb, E2E_SHOP_DB: shopDb, E2E_WEIRD_DB: weirdDb }, stdio: ['ignore', 'ignore', 'pipe'],
  });
  let serverLog = ''; server.stderr.on('data', d => { serverLog += d; });
  let browser;
  const cleanup = async () => {
    try { if (browser) await browser.close(); } catch (_) { /* best effort */ }
    server.kill();
    chatStub.close();
    fs.rmSync(dir, { recursive: true, force: true });
  };
  try {
    for (let i = 0; i < 120; i++) { try { if ((await fetch(BASE + '/health')).ok) break; } catch (_) { /* not up yet */ } await sleep(500); if (i === 119) throw new Error('server never became healthy:\n' + serverLog); }

    // ---- seed over REST with the header (as Studio authenticates) --------
    const tok = (await (await fetch(BASE + '/api/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username: 'admin', password: PW }) })).json()).token;
    const H = { 'Content-Type': 'application/json', Authorization: 'Bearer ' + tok };
    const rem = async b => (await fetch(BASE + '/api/v1/remember', { method: 'POST', headers: H, body: JSON.stringify(b) })).json();
    await rem({ type_name: 'Person', properties: { name: HOSTILE, city: 'Paris' }, tenant: 't1' });
    chatState.companyKoid = (await rem({ type_name: 'Company', properties: { name: 'Acme' }, tenant: 't1' })).data.koid;
    await rem({ type_name: 'Person', properties: { name: 'Solo' } });

    browser = await chromium.launch({ executablePath: CHROME, headless: true });
    await checkChatDisabledNotice(browser);
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
    ok('KOID link opens the Inspector with that KOID (at the panel address, no page navigation)',
      await page.evaluate(() => document.getElementById('panel-inspector').classList.contains('active') && document.getElementById('inspector-search-input').value.length > 8 && location.hash === '#/inspector'));

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
    ok('the raw definition editor is an advanced, collapsed section', await page.evaluate(() => { const d = document.querySelector('#panel-ontology details'); return !!d && !d.open && d.contains(document.getElementById('ontology-yaml-input')); }));
    await page.evaluate(() => { document.querySelector('#panel-ontology details').open = true; });
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

    // ---- Data Sources: discover, preview, merge, publish, import, query ------
    const tryClick = async sel => { await page.locator(sel).first().click({ timeout: 2000 }).catch(() => {}); await page.waitForTimeout(500); };
    await tryClick('button[data-panel="sources"]');
    await page.waitForTimeout(700);
    const listText = await page.innerText('#sources-list').catch(() => '');
    ok('Data Sources lists the configured sources', /crm/.test(listText) && /shop/.test(listText) && /weird/.test(listText), listText.slice(0, 200));
    await tryClick('[data-click="discoverSource"][data-name="crm"]');
    ok('Discover reports the container count', /2 containers/.test(await page.innerText('#sources-result').catch(() => '')), await page.innerText('#sources-result').catch(() => 'no panel'));
    await tryClick('[data-click="discoverSource"][data-name="shop"]');
    await tryClick('[data-click="discoverSource"][data-name="weird"]');
    await page.locator('.src-pick[data-name="crm"]').check({ timeout: 2000 }).catch(() => {});
    await page.locator('.src-pick[data-name="shop"]').check({ timeout: 2000 }).catch(() => {});
    await tryClick('[data-click="previewSourceOntology"]');
    const preview1 = await page.innerText('#sources-preview').catch(() => '');
    ok('Preview proposes one class per table, as new', /Customer/.test(preview1) && /Order/.test(preview1) && /Client/.test(preview1) && /new/.test(preview1), preview1.slice(0, 200));
    ok('Preview suggests merging the two customer tables, as advice only', /Suggested merges/.test(preview1) && /crm\.customers/.test(preview1) && /shop\.clients/.test(preview1) && await page.locator('[data-click="applySuggestion"]').count() >= 1, preview1.slice(0, 300));
    await page.locator('.cls-pick[data-class="Customer"]').check({ timeout: 2000 }).catch(() => {});
    await page.locator('.cls-pick[data-class="Client"]').check({ timeout: 2000 }).catch(() => {});
    await tryClick('[data-click="mergeSelectedClasses"]');
    const preview2 = await page.innerText('#sources-preview').catch(() => '');
    ok('Merging two classes leaves one class with both tables', /Customer/.test(preview2) && !/Client/.test(preview2.replace(/clients/g, '')), preview2.slice(0, 300));
    // ---- the workbench: accept a key, set authority, then publish them with the classes ----
    ok('Preview offers keys, links and authority', /Keys — which rows are the same row/.test(preview2) && /Links — which rows relate/.test(preview2) && /Authority — who is believed/.test(preview2), preview2.slice(0, 200));
    const keyBtn = page.locator('[data-click="toggleIdentity"][data-container="crm.customers"]');
    ok('a proposed key explains itself and can be accepted', await keyBtn.count() === 1 && /declared by the source|%/.test(await page.innerText('#sources-preview')), '');
    await keyBtn.click({ timeout: 2000 }).catch(() => {}); await page.waitForTimeout(500);
    ok('an accepted key turns into a Remove button and survives the re-preview', (await page.locator('[data-click="toggleIdentity"][data-container="crm.customers"]').innerText().catch(() => '')) === 'Remove');
    const addAssertion = async (from, cls, props, rank) => {
      await page.fill('#as-from', from); await page.fill('#as-class', cls); await page.fill('#as-props', props); await page.fill('#as-rank', rank);
      await tryClick('[data-click="addAssertion"]');
    };
    await addAssertion('crm.customers.id', 'Customer', 'email=email', '10');
    await addAssertion('shop.clients.id', 'Customer', 'email_address=email', '20');
    const withAuth = await page.innerText('#sources-preview');
    ok('declared authority is listed with its rank', /rank 10/.test(withAuth) && /rank 20/.test(withAuth) && await page.locator('[data-click="removeAssertion"]').count() === 2, withAuth.slice(-300));
    await addAssertion('crm.customers.nope', 'Customer', 'email=email', '5');
    ok('an assertion naming a missing column is refused with the reason, not published', /assertion ignored/.test(await page.innerText('#sources-msg')) && /no column nope/.test(await page.innerText('#sources-msg')), await page.innerText('#sources-msg'));
    ok('a refused assertion is not left listed as declared', await page.locator('[data-click="removeAssertion"]').count() === 2);
    await addAssertion('crm.customers.id', 'Temp', 'email=email', '50');
    ok('a third assertion is listed', await page.locator('[data-click="removeAssertion"]').count() === 3);
    await page.locator('[data-click="removeAssertion"]').last().click({ timeout: 2000 }).catch(() => {}); await page.waitForTimeout(500);
    ok('a pending assertion can be removed', await page.locator('[data-click="removeAssertion"]').count() === 2);
    await addAssertion('crm.customers.id', 'Customer', 'email=email', '');
    ok('a blank rank is refused, not read as the highest authority (0)', /whole-number rank/.test(await page.innerText('#sources-msg')) && await page.locator('[data-click="removeAssertion"]').count() === 2, await page.innerText('#sources-msg'));
    await tryClick('[data-click="applySourceOntology"]');
    ok('Publish saves the ontology', /Published/.test(await page.innerText('#sources-msg').catch(() => '')), await page.innerText('#sources-msg').catch(() => ''));
    const onto = ((await (await fetch(BASE + '/api/v1/ontology', { headers: H })).json()).data || {}).properties || {};
    const classNames = (onto.classes || []).map(c => c.name);
    ok('the published ontology has Customer (not Client) mapped to both tables', classNames.includes('Customer') && !classNames.includes('Client') && (onto.mappings || []).filter(m => m.class === 'Customer').length === 2, JSON.stringify(classNames));
    ok('the published mapping carries the key and the authority', (onto.mappings || []).some(m => m.physical_type === 'customers' && (m.identity || []).join() === 'id' && (m.asserts || []).some(a => a.class === 'Customer' && a.rank === 10)), JSON.stringify(onto.mappings));
    await page.locator('.imp-pick[data-source="crm"][data-name="customers"]').check({ timeout: 2000 }).catch(() => {});
    await tryClick('[data-click="importSource"][data-name="crm"]');
    await page.locator('.imp-pick[data-source="shop"][data-name="clients"]').check({ timeout: 2000 }).catch(() => {});
    await tryClick('[data-click="importSource"][data-name="shop"]');
    const matched = await (await fetch(BASE + '/api/aikoql?query=' + encodeURIComponent('MATCH Customer RETURN *'), { headers: H })).text();
    ok('MATCH Customer returns the rows of both sources', (matched.match(/"koid"/g) || []).length >= 2, matched.slice(0, 200));
    // ---- Ontology panel: the class cards show keys and authority; Resolve reconciles ----
    await tryClick('button[data-panel="ontology"]'); await page.waitForTimeout(700);
    const ontoText = await page.innerText('#ontology-mappings').catch(() => '');
    ok('the mapping table shows key, links and authority', /Key/.test(ontoText) && /Authority/.test(ontoText) && /Customer \(rank 10\)/.test(ontoText), ontoText.slice(0, 300));
    ok('a class card shows its source key', /key: id/.test(await page.innerText('#ontology-classes').catch(() => '')));
    ok('Resolve is offered for the class that has authority', (await page.locator('#reconcile-class option').allInnerTexts()).includes('Customer') && await page.locator('#reconcile-run').isEnabled());
    await tryClick('#reconcile-run'); await page.waitForTimeout(800);
    const rec = await page.innerText('#reconcile-result').catch(() => '');
    ok('Resolve reports entities and claims', /2 entities from 3 claims/.test(rec) && /1 disagreements settled by authority, 0 open/.test(rec), rec.slice(0, 300));
    const row = page.locator('#reconcile-result tbody tr');
    ok('the disagreement is a row naming both values, the winner and the rule', await row.count() === 1 && /a@x\.test/.test(await row.innerText()) && /changed@y\.test/.test(await row.innerText()) && /resolved/.test(await row.innerText()) && /sqlite\.customers/.test(await row.innerText()) && /rank/.test(await row.innerText()), await row.first().innerText().catch(() => 'no row'));
    await tryClick('#reconcile-result [data-click="openInInspector"]'); await page.waitForTimeout(800);
    ok('Inspect opens the contradiction in the Inspector', /aikoql:contradiction/.test(await page.innerText('#inspector-detail').catch(() => '')) || /KOID/.test(await page.innerText('#inspector-detail').catch(() => '')), (await page.innerText('#inspector-detail').catch(() => '')).slice(0, 200));
    ok('a reload of the preview reports published keys and authority as published, with no Remove', await (async () => {
      await tryClick('button[data-panel="sources"]');
      await page.locator('.src-pick[data-name="crm"]').check({ timeout: 2000 }).catch(() => {});
      await page.locator('.src-pick[data-name="shop"]').check({ timeout: 2000 }).catch(() => {});
      await tryClick('[data-click="previewSourceOntology"]');
      const t = await page.innerText('#sources-preview');
      return /published/.test(t) && /rank 10/.test(t) && await page.locator('[data-click="removeAssertion"]').count() === 0 && await page.locator('[data-click="toggleIdentity"][data-container="crm.customers"]').count() === 0;
    })());
    await tryClick('button[data-panel="sources"]');
    ok('hostile table and column names are rendered inertly',
      await page.evaluate(() => document.querySelectorAll('#panel-sources [onerror], #panel-sources img').length === 0 && !window.__pwned) && /<img src=x onerror/.test(await page.textContent('#sources-list').catch(() => '')));
    ok('no CSP violation and no console error on the Data Sources panel',
      (await page.evaluate(() => window.__csp)).length === 0 && consoleErrors.filter(e => !/Failed to load resource/.test(e)).length === 0, consoleErrors.join(' || '));
    // ---- Ask: a grounded answer, a refusal, a koid link, a hostile answer ---
    const askSubmit = async q => {
      await page.fill('#ask-question', q, { timeout: 1500 }).catch(() => {});
      await page.press('#ask-question', 'Control+Enter', { timeout: 1500 }).catch(() => {});
      await page.waitForSelector('#ask-result .badge', { timeout: 8000 }).catch(() => {});
      await page.waitForTimeout(300);
    };
    await page.locator('button[data-panel="ask"]').click({ timeout: 2000 }).catch(() => {});
    await page.waitForTimeout(900);
    await page.selectOption('#ask-tenant', 't1', { timeout: 1500 }).catch(() => {});
    await askSubmit('Which companies exist?');
    ok('a grounded answer shows the green badge and its facts',
      await page.evaluate(() => { const r = document.getElementById('ask-result'); return !!r && !!r.querySelector('.badge-green') && /Acme is a company/.test(r.innerText) && /Company Acme: name=Acme/.test(r.textContent); }), await page.innerText('#ask-result').catch(() => 'no panel'));
    await page.locator('#ask-result summary').click({ timeout: 2000 }).catch(() => {});
    await page.locator('#ask-result a[data-click="openInInspector"]').first().click({ timeout: 2000 }).catch(() => {});
    await page.waitForTimeout(700);
    ok('claim evidence is the real koid the server matched, not the model\'s id',
      await page.evaluate(k => { const a = document.querySelector('#ask-result a[data-click="openInInspector"]'); return !!a && a.dataset.koid === k; }, chatState.companyKoid));
    ok('a koid link opens the Inspector',
      await page.evaluate(() => document.getElementById('panel-inspector').classList.contains('active') && document.getElementById('inspector-search-input').value.length > 8));
    await page.locator('button[data-panel="ask"]').click({ timeout: 2000 }).catch(() => {}); await page.waitForTimeout(500);
    await askSubmit('an unknowable thing');
    ok('a refused answer shows the amber badge',
      await page.evaluate(() => { const r = document.getElementById('ask-result'); return !!r && !!r.querySelector('.badge-orange') && !r.querySelector('.badge-green') && /UNKNOWN/.test(r.innerText); }), await page.innerText('#ask-result').catch(() => ''));
    await askSubmit('hostile companies');
    ok('a hostile answer string is rendered inertly',
      await page.evaluate(() => { const r = document.getElementById('ask-result'); return !!r && r.querySelectorAll('img,[onerror]').length === 0 && !window.__pwned && r.textContent.includes('<img src=x'); }));
    await page.fill('#ask-question', 'slow companies', { timeout: 1500 }).catch(() => {});
    await page.click('#ask-submit', { timeout: 1500 }).catch(() => {});
    const inFlight = await page.evaluate(() => { const b = document.getElementById('ask-submit'); return !!b && b.disabled; });
    await page.waitForTimeout(3500); // two model calls of ~0.9s each
    ok('the Ask button is disabled while a request is in flight and enabled after',
      inFlight && await page.evaluate(() => !document.getElementById('ask-submit').disabled));
    ok('no CSP violation and no console error on the Ask panel',
      (await page.evaluate(() => window.__csp)).length === 0 && consoleErrors.filter(e => !/Failed to load resource/.test(e)).length === 0, consoleErrors.join(' || '));

    // ---- sweep: every delegated control, in every panel, must REACT -------
    // downloads stall page.click (a file save is not a DOM change); downloadResults has its own explicit check
    const SKIP = new Set(['logout', 'adminRestoreBackup', 'signInSso', 'downloadAuditExport', 'downloadResults', 'saveDocument', 'removeDocumentKnowledge']); // the last two write/delete data: explicit checks below
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

    // ---- documents: a real PNG with known text — the visible OCR outcome ---
    // The server inherits this PATH, so the branch asserted is the one it takes.
    const hasTess = (process.env.PATH || '').split(path.delimiter).some(d => d && fs.existsSync(path.join(d, 'tesseract')));
    await page.click('button[data-panel="documents"]'); await page.waitForTimeout(700);
    await page.setInputFiles('#doc-file-input', { name: 'scan.png', mimeType: 'image/png', buffer: fs.readFileSync(path.join(ROOT, 'crates/ingestion/tests/fixtures/ocr-text.png')) });
    await page.click('[data-click="ingestDocument"]'); await page.waitForTimeout(3000);
    const ingestMsg = await page.innerText('#doc-ingest-result');
    ok('ingested document is listed with a rendered status badge', await page.evaluate(() => !!document.querySelector('#documents-list td .badge') && !/<span/.test(document.getElementById('documents-list').innerText)), await page.innerText('#documents-list'));
    ok(hasTess ? 'image ingest reports the text OCR read' : 'image ingest says OCR is unavailable', hasTess ? /ocr_complete/.test(ingestMsg) && /characters of text read by OCR/.test(ingestMsg) : /OCR is not available/.test(ingestMsg), ingestMsg);
    await page.click('[data-click="compileDocument"]'); await page.waitForTimeout(3000);
    // textContent: the IR sits in collapsed <details>, which innerText omits
    const compiled = await page.textContent('#doc-compile-result');
    ok('an uploaded image compiles', /Compilation Results/.test(compiled) && !/Compile failed/.test(compiled), compiled);
    ok(hasTess ? 'the OCR text reaches the compiled result' : 'the compiled result says OCR is unavailable', hasTess ? /Utrecht/.test(compiled) : /OCR is not available/.test(compiled), compiled);

    // ---- saving a document into the knowledge base: whole, verified n-of-n, browsable, removable ---
    page.removeAllListeners('dialog'); page.on('dialog', d => { dialogs++; d.accept().catch(() => {}); });
    const story = Array.from({ length: 60 }, (_, i) => `Chapter ${i + 1}. Alice Johnson met Bob Smith at Acme Corporation in Paris. They discussed the Beta Labs merger. `).join('');
    await page.setInputFiles('#doc-file-input', { name: 'story.txt', mimeType: 'text/plain', buffer: Buffer.from(story) });
    await page.click('[data-click="ingestDocument"]'); await page.waitForTimeout(2000);
    const storyRow = () => page.locator('#documents-list tr', { hasText: 'story.txt' });
    ok('an ingested document says it is not saved and offers Save', /not saved/.test(await storyRow().innerText()) && await storyRow().locator('[data-click="saveDocument"]').count() === 1, await storyRow().innerText());
    await page.click('[data-click="compileDocument"]'); await page.waitForTimeout(2500);
    ok('Compile says it is a preview and that nothing is stored', /Preview — nothing is stored/.test(await page.textContent('#doc-compile-result')));
    await storyRow().locator('[data-click="saveDocument"]').click();
    for (let i = 0; i < 80 && !/complete|incomplete/.test(await storyRow().innerText()); i++) await sleep(500);
    const savedText = await storyRow().innerText();
    const pairs = [...savedText.matchAll(/(\w+) (\d+)\/(\d+)/g)].map(m => [m[1], +m[2], +m[3]]);
    ok('Save stores the document and shows a verified, complete n-of-n for every kind', /✓ complete/.test(savedText) && pairs.length >= 5 && pairs.every(p => p[1] === p[2]) && ['pages', 'chunks', 'fragments', 'entities'].every(k => pairs.some(p => p[0] === k && p[2] > 0)), savedText.replace(/\\s+/g, ' '));
    // the global graph does not draw the document's objects, and says they exist
    await page.click('button[data-panel="graph"]'); await page.waitForTimeout(600);
    await page.click('[data-click="loadGraphAll"]'); await page.waitForTimeout(1500);
    ok('the global graph hides saved document objects and says how many', /saved document objects are in the knowledge base/.test(await page.innerText('#graph-banner')), await page.innerText('#graph-banner').catch(() => 'no banner'));
    await page.click('button[data-panel="documents"]'); await page.waitForTimeout(800);
    await storyRow().locator('[data-click="viewDocumentGraph"]').click(); await page.waitForTimeout(2000);
    const banner = await page.innerText('#graph-banner');
    ok('the document graph states completeness and which page it draws', /complete in the knowledge base/.test(banner) && /Drawing 1–\d+ of \d+ entities/.test(banner) && /Browse all as a table/.test(banner), banner);
    ok('the graph offers a picker of saved documents, with this one selected', await page.evaluate(() => { const sel = document.getElementById('graph-document'); return sel.style.display !== 'none' && [...sel.options].some(o => /story[.]txt/.test(o.text) && o.selected); }));
    // "Browse all as a table" on the graph must take the user to the table (it lives on the Documents page)
    await page.click('#graph-banner [data-click="browseDocument"]'); await page.waitForTimeout(1500);
    ok('Browse all from the graph opens the Documents page with the table showing', await page.evaluate(() => document.getElementById('panel-documents').classList.contains('active') && /Everything saved for this document/.test(document.getElementById('doc-browse').innerText)), await page.evaluate(() => location.hash));
    await page.click('button[data-panel="documents"]'); await page.waitForTimeout(800);
    await storyRow().locator('[data-click="browseDocument"]').click(); await page.waitForTimeout(1500);
    await page.click('#doc-browse [data-click="browseKind"][data-kind="chunk"]'); await page.waitForTimeout(1200);
    const totalChunks = +/of (\d+) chunks/.exec(await page.innerText('#doc-browse'))[1];
    let counted = 0, rowsOnPage = 0;
    for (let guard = 0; guard < 400; guard++) {
      rowsOnPage = (await page.$$('#doc-browse tbody tr')).length; counted += rowsOnPage;
      if (await page.locator('#doc-browse [data-click="browsePage"][data-dir="1"]').isDisabled()) break;
      await page.click('#doc-browse [data-click="browsePage"][data-dir="1"]'); await page.waitForTimeout(500);
    }
    ok('Browse all pages through every saved chunk exactly once', totalChunks >= 1 && counted === totalChunks, `${counted} rows of ${totalChunks}`);
    await page.fill('#browse-q', 'Chapter 60'); await page.click('#doc-browse [data-click="browseSearch"]'); await page.waitForTimeout(1000);
    ok('Browse search finds the last chapter of the document', /Chapter 60/.test(await page.innerText('#doc-browse')) && /matching/.test(await page.innerText('#doc-browse')), (await page.innerText('#doc-browse')).slice(0, 200));
    await page.click('#doc-browse [data-click="browseClose"]');
    await storyRow().locator('[data-click="removeDocumentKnowledge"]').click(); await page.waitForTimeout(1500);
    ok('Remove takes the knowledge out and the row says not saved again', /not saved/.test(await storyRow().innerText()) && /Removed \d+ objects/.test(await page.innerText('#doc-ingest-result')), await storyRow().innerText());
    page.removeAllListeners('dialog'); page.on('dialog', d => { dialogs++; d.dismiss().catch(() => {}); });

    await page.setInputFiles('#doc-file-input', { name: 'IMG_0001.heic', mimeType: 'image/heic', buffer: Buffer.from('ftypheic') });
    await page.click('[data-click="ingestDocument"]'); await page.waitForTimeout(1200);
    ok('an unsupported image type is refused at upload with a way forward', /convert the image to PNG or JPEG/.test(await page.innerText('#doc-ingest-result')), await page.innerText('#doc-ingest-result'));

    // ---- an over-limit document is read in batches, with visible progress --
    const statusCalls = () => requests.filter(u => /\/api\/v1\/documents\/[0-9a-f]+\/status$/.test(u)).length;
    await page.setInputFiles('#doc-file-input', { name: 'deck.pptx', mimeType: 'application/vnd.openxmlformats-officedocument.presentationml.presentation', buffer: fs.readFileSync(path.join(ROOT, 'crates/ingestion/tests/fixtures/office/deck.pptx')) });
    await page.click('[data-click="ingestDocument"]');
    await page.waitForSelector('#doc-ingest-result [data-batch-status]', { timeout: 10000 }).catch(() => {});
    ok('an over-limit upload shows the batch notice with a progress bar', await page.evaluate(() => !!document.querySelector('#doc-ingest-result [data-batch-status] [role="progressbar"]') && /batches/.test(document.getElementById('doc-ingest-result').innerText)), await page.innerText('#doc-ingest-result'));
    // a bounded poll (waitForFunction evaluates a string, which the page CSP reports)
    for (let i = 0; i < 60 && !(await page.evaluate(() => { const n = document.querySelector('#doc-ingest-result [data-batch-status]'); return !!n && n.dataset.batchStatus !== 'processing'; })); i++) await sleep(500);
    const batchDone = await page.innerText('#doc-ingest-result');
    ok('the batch notice ends with a clear success message and every batch result', /Done — all 2 batches processed/.test(batchDone) && /You can compile it now/.test(batchDone) && (await page.$$('#doc-ingest-result tbody tr')).length === 2, batchDone);
    await page.waitForTimeout(1200);
    ok('Compile is enabled once every batch has finished', await page.evaluate(() => [...document.querySelectorAll('#documents-list tr')].some(r => /deck\.pptx/.test(r.innerText) && !r.querySelector('[data-click="compileDocument"]').disabled)), await page.innerText('#documents-list'));
    const polledDone = statusCalls(); await page.waitForTimeout(4500);
    ok('status polling stops when the document is done', statusCalls() === polledDone, `${polledDone} -> ${statusCalls()}`);
    // a namespaced type (aikoql:document) must list its objects, not "No objects"
    await page.click('button[data-panel="explorer"]'); await page.waitForTimeout(900);
    await page.click('#explorer-tree-content .tree-item.type-node[data-type="aikoql:document"]'); await page.waitForTimeout(900);
    const docObjs = await page.innerText('#explorer-detail');
    ok('Explorer lists the objects of a namespaced type (aikoql:document)', !/No objects of type/.test(docObjs) && /objects\)/.test(docObjs), docObjs.slice(0, 200));
    await page.click('button[data-panel="documents"]'); await page.waitForTimeout(600);

    // mid-flight, made deterministic: the server answers "batch 1 of 3"
    const FAKE = 'ff00000000000000000000000000beef';
    const fakeDoc = { koid: FAKE, filename: 'huge.pdf', mime_type: 'application/pdf', sha256: 'f'.repeat(64), size_bytes: 9e6, char_count: 10, status: 'processing', page_count: 6, batches_total: 3, batches_done: 1, pages_done: 2, message: 'This document is large (6 pages). It is being processed in 3 batches of up to 2 pages: 1 of 3 done (2 of 6 pages). Please wait until it finishes before compiling.' };
    await page.route('**/api/v1/list-documents', r => r.fulfill({ contentType: 'application/json', body: JSON.stringify({ data: { documents: [fakeDoc] } }) }));
    await page.route(`**/api/v1/documents/${FAKE}/status`, r => r.fulfill({ contentType: 'application/json', body: JSON.stringify({ data: { ...fakeDoc, batches: [{ first_page: 1, last_page: 2, status: 'extracted', char_count: 10, images: 0, tables: 0 }] } }) }));
    await page.click('button[data-panel="graph"]'); await page.waitForTimeout(300);
    await page.click('button[data-panel="documents"]'); await page.waitForTimeout(1500);
    const fakeBtn = await page.$('#documents-list [data-click="compileDocument"]');
    ok('Compile is disabled with an explanation while a document is processing', !!fakeBtn && await fakeBtn.isDisabled() && /Still processing/.test(await fakeBtn.getAttribute('title')), await page.innerText('#documents-list'));
    ok('a processing document shows "keep this page open" and its progress', /keep this page open/.test(await page.innerText('#doc-ingest-result')) && await page.evaluate(() => document.querySelector('#doc-ingest-result [role="progressbar"]')?.getAttribute('aria-valuenow') === '1'), await page.innerText('#doc-ingest-result'));
    await page.waitForTimeout(2300);
    const polling = statusCalls();
    await page.click('button[data-panel="graph"]'); await page.waitForTimeout(4500);
    ok('status polling continues every ~2s and stops when the user leaves the panel', polling > polledDone + 1 && statusCalls() <= polling + 1, `${polledDone} -> ${polling} -> ${statusCalls()}`);
    await page.unroute('**/api/v1/list-documents'); await page.unroute(`**/api/v1/documents/${FAKE}/status`);

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
    // ---- E1: panels have addresses; navigation is labelled and keyboard-reachable ----
    const hash = () => page.evaluate(() => location.hash);
    const panel = () => page.evaluate(() => activePanel);
    await page.click('button[data-panel="ontology"]'); await page.waitForTimeout(500);
    ok('navigating writes the address (#/ontology)', (await hash()) === '#/ontology', await hash());
    ok('the current destination is announced (aria-current, exactly one)', (await page.getAttribute('button[data-panel="ontology"]', 'aria-current')) === 'page' && (await page.locator('#sidebar nav [aria-current]').count()) === 1);
    ok('the page title names the panel', /Ontology/.test(await page.title()), await page.title());
    await page.reload(); await page.waitForTimeout(1500);
    ok('reload lands on the same panel', (await panel()) === 'ontology' && await page.isVisible('#panel-ontology') && (await hash()) === '#/ontology', await panel() + ' ' + await hash());
    await page.click('button[data-panel="sources"]'); await page.waitForTimeout(400);
    await page.goBack(); await page.waitForTimeout(600);
    ok('Back returns to the previous panel', (await panel()) === 'ontology' && (await hash()) === '#/ontology', await panel());
    await page.goForward(); await page.waitForTimeout(600);
    ok('Forward returns to the next panel', (await panel()) === 'sources', await panel());
    await page.evaluate(() => { location.hash = '#/inspector'; }); await page.waitForTimeout(500);
    ok('a pasted #/panel link opens that panel', (await panel()) === 'inspector' && await page.isVisible('#panel-inspector'));
    await page.evaluate(() => { location.hash = '#/no-such-panel'; }); await page.waitForTimeout(400);
    ok('an unknown address is ignored, not an error', (await panel()) === 'inspector' && await page.isVisible('#panel-inspector'));
    ok('every nav entry has a text label and is a focusable button', await page.evaluate(() => [...document.querySelectorAll('#sidebar nav button')].every(b => b.querySelector('.lbl').textContent.trim() !== '' && b.tabIndex === 0)));
    ok('nav entries are grouped under headings', (await page.locator('#sidebar .nav-group').count()) >= 3);
    ok('labels are visible without hovering on a wide screen', (await page.evaluate(() => getComputedStyle(document.querySelector('#sidebar nav button .lbl')).opacity)) === '1');
    await page.setViewportSize({ width: 800, height: 700 }); await page.mouse.move(700, 400); await page.waitForTimeout(300);
    ok('on a narrow screen the rail is icons at rest', (await page.evaluate(() => getComputedStyle(document.querySelector('#sidebar nav button .lbl')).opacity)) === '0');
    await page.focus('.skip'); await page.keyboard.press('Tab'); await page.waitForTimeout(300); // a real key press: focus-visible
    ok('...and its labels appear on keyboard focus, not only on hover', (await page.evaluate(() => getComputedStyle(document.querySelector('#sidebar nav button .lbl')).opacity)) === '1');
    await page.setViewportSize({ width: 1280, height: 720 }); await page.waitForTimeout(200);
    await page.focus('.skip'); await page.keyboard.press('Enter'); await page.waitForTimeout(200);
    ok('the skip link moves focus to the content', (await page.evaluate(() => document.activeElement && document.activeElement.id)) === 'panel-content');
    await page.focus('button[data-panel="query"]');
    ok('keyboard focus is visible on a nav entry', (await page.evaluate(() => getComputedStyle(document.activeElement).outlineStyle)) !== 'none');
    await page.click('button[data-panel="query"]'); await page.waitForTimeout(300);
    // ---- E3: toasts are announced; a write after the session died returns to sign-in ----
    await page.evaluate(() => { toast('success', 'Saved the thing'); toast('error', 'It failed'); });
    ok('a success toast is a polite status', (await page.locator('#toasts [role=status]').innerText()).includes('Saved the thing'));
    ok('an error toast is an assertive alert', (await page.locator('#toasts [role=alert]').innerText()).includes('It failed'));
    await page.click('#toasts [role=status] button'); await page.waitForTimeout(200);
    ok('a toast can be dismissed with its named control', (await page.locator('#toasts [role=status]').count()) === 0 && (await page.getAttribute('#toasts [role=alert] button', 'aria-label')) !== null);
    await page.context().clearCookies();
    const expired = await page.evaluate(async () => { try { await apiPost('/api/v1/ontology', {}); return 'no error'; } catch (e) { return e.message; } });
    ok('a POST after the session died reports session expiry', expired === 'session expired', expired);
    ok('...and returns to the sign-in form with the reason', await page.isVisible('#login-overlay') && /Session expired/.test(await page.innerText('#login-error')), await page.innerText('#login-error'));
    await page.fill('#login-user', 'admin'); await page.fill('#login-pass', PW);
    await page.click('[data-click="doLogin"]'); await page.waitForSelector('#login-overlay', { state: 'hidden', timeout: 10000 }).catch(() => {});
    ok('signing in again resumes the same panel', await page.isHidden('#login-overlay') && (await panel()) === 'query', await panel());
    // ---- E2/E6: a draft survives a reload; named queries and history are real ----
    const cm = () => page.evaluate(() => window.__cmGetValue());
    await page.evaluate(() => window.__cmSetValue('')); await page.click('#query-editor .cm-content');
    await page.keyboard.type('MATCH Company RETURN *'); await page.waitForTimeout(900);
    await page.reload(); await page.waitForTimeout(1800);
    ok('the query draft survives a reload', (await cm()) === 'MATCH Company RETURN *', await cm());
    await page.fill('#query-save-name', 'all companies'); await page.click('[data-click="saveQuery"]'); await page.waitForTimeout(300);
    ok('Save puts the query in the Saved list', /all companies/.test(await page.innerText('#query-saved')), await page.innerText('#query-saved'));
    ok('saving is announced', /all companies/.test(await page.locator('#toasts [role=status]').last().innerText()));
    await page.evaluate(() => window.__cmSetValue('MATCH Person RETURN *')); await page.click('[data-click="runQuery"]'); await page.waitForTimeout(900);
    await page.evaluate(() => window.__cmSetValue('MATCH Company RETURN *')); await page.click('[data-click="runQuery"]'); await page.waitForTimeout(900);
    const recent = await page.innerText('#query-history');
    ok('Recent lists what ran, newest first', recent.indexOf('Company') !== -1 && recent.indexOf('Person') > recent.indexOf('Company'), recent);
    await page.evaluate(() => window.__cmSetValue('')); await page.click('#query-saved [data-click="loadQuery"]'); await page.waitForTimeout(300);
    ok('clicking a saved query loads it into the editor', (await cm()) === 'MATCH Company RETURN *', await cm());
    await page.reload(); await page.waitForTimeout(1800);
    ok('saved queries and history survive a reload', /all companies/.test(await page.innerText('#query-saved')) && /Person/.test(await page.innerText('#query-history')));
    await page.evaluate(() => window.__cmSetValue('MATCH Person RETURN *')); await page.click('#query-editor .cm-content');
    await page.keyboard.press('Control+s'); await page.waitForTimeout(300);
    ok('Ctrl+S with no name asks for one instead of pretending to save', (await page.evaluate(() => document.activeElement && document.activeElement.id)) === 'query-save-name');
    await page.click('#query-saved [data-click="deleteSaved"]'); await page.waitForTimeout(300);
    ok('a saved query can be deleted', !/all companies/.test(await page.innerText('#query-saved')));
    await page.evaluate(() => { Storage.prototype.setItem = function () { throw new Error('quota'); }; });
    await page.fill('#query-save-name', 'blocked'); await page.click('[data-click="saveQuery"]'); await page.waitForTimeout(300);
    ok('when the browser refuses storage the person is told, not lied to', /Could not save/.test(await page.locator('#toasts [role=alert]').last().innerText().catch(() => '')));
    await page.reload(); await page.waitForTimeout(1500);
    // ---- E4: the results table is sortable, copyable, exportable and linked ----
    await page.context().grantPermissions(['clipboard-read', 'clipboard-write'], { origin: BASE });
    await page.evaluate(() => window.__cmSetValue('MATCH Person RETURN *'));
    await page.click('[data-click="runQuery"]'); await page.waitForTimeout(1200);
    const rowCount = await page.locator('#query-results tbody tr').count();
    ok('the footer states the row count and the elapsed time', /^\d+ rows? · \d+ ms$/.test((await page.innerText('#query-results .res-count')).trim()), await page.innerText('#query-results .res-count').catch(() => ''));
    const col = async name => page.evaluate(n => {
      const ths = [...document.querySelectorAll('#query-results thead th')];
      const i = ths.findIndex(th => th.textContent.replace(/[▲▼]/g, '').trim() === n);
      return [...document.querySelectorAll('#query-results tbody tr')].map(tr => tr.children[i].textContent);
    }, name);
    const nameBefore = await col('name');
    await page.click('#query-results th button[data-colname="name"]'); await page.waitForTimeout(200);
    const asc = await col('name');
    ok('clicking a header sorts ascending and announces it', JSON.stringify(asc) === JSON.stringify([...nameBefore].sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))) && (await page.getAttribute('#query-results th:has(button[data-colname="name"])', 'aria-sort')) === 'ascending', JSON.stringify(asc));
    await page.click('#query-results th button[data-colname="name"]'); await page.waitForTimeout(200);
    ok('clicking again sorts descending', JSON.stringify(await col('name')) === JSON.stringify([...asc].reverse()) && (await page.getAttribute('#query-results th:has(button[data-colname="name"])', 'aria-sort')) === 'descending');
    await page.click('[data-click="copyResults"][data-kind="json"]'); await page.waitForTimeout(300);
    const clip = await page.evaluate(() => navigator.clipboard.readText());
    ok('Copy JSON puts every row on the clipboard, in the current order', Array.isArray(JSON.parse(clip)) && JSON.parse(clip).length === rowCount && JSON.parse(clip)[0].properties.name === (await col('name'))[0], clip.slice(0, 120));
    ok('copying is announced', /Copied/.test(await page.locator('#toasts [role=status]').last().innerText()));
    await page.click('[data-click="copyResults"][data-kind="csv"]'); await page.waitForTimeout(300);
    const csv = await page.evaluate(() => navigator.clipboard.readText());
    ok('Copy CSV has a header and one line per row', csv.split('\r\n').filter(Boolean).length === rowCount + 1 && /^koid,/.test(csv), csv.slice(0, 120));
    const [dl] = await Promise.all([page.waitForEvent('download', { timeout: 5000 }).catch(() => null), page.click('[data-click="downloadResults"][data-kind="csv"]')]);
    const dlText = dl ? fs.readFileSync(await dl.path(), 'utf8') : '';
    ok('Download CSV saves a file with the same rows', !!dl && /\.csv$/.test(dl.suggestedFilename()) && dlText.split('\r\n').filter(Boolean).length === rowCount + 1, dl ? dl.suggestedFilename() : 'no download');
    ok('CSV quoting is exact and a spreadsheet formula is defused', (await page.evaluate(() => resultsToCsv(['a', 'b'], [['=1+1', 'x,y'], ['"q"\nz', null], [-5, '@cmd']]))) === 'a,b\r\n\'=1+1,"x,y"\r\n"""q""\nz",\r\n-5,\'@cmd\r\n');
    ok('a nested value shows as JSON, not [object Object]', (await page.evaluate(() => cellText({ a: [1, { b: 2 }] }))) === '{"a":[1,{"b":2}]}');
    await page.click('#query-results a[data-click="openInInspector"]'); await page.waitForTimeout(600);
    ok('a row links to its object in the Inspector', (await panel()) === 'inspector' && (await page.inputValue('#inspector-search-input')).length > 8, await panel());
    await page.click('button[data-panel="query"]'); await page.waitForTimeout(300);
    // ---- E5: the command palette ----
    await page.fill('#query-save-name', 'palette query'); await page.evaluate(() => window.__cmSetValue('MATCH Company RETURN *')); await page.click('[data-click="saveQuery"]'); await page.waitForTimeout(300);
    await page.evaluate(() => window.__cmSetValue('MATCH Person RETURN *'));
    await page.focus('button[data-panel="query"]');
    await page.keyboard.press('Control+k'); await page.waitForTimeout(300);
    ok('Ctrl+K opens the palette with focus in its input', await page.isVisible('#palette') && (await page.evaluate(() => document.activeElement.id)) === 'palette-input');
    ok('the palette is an announced modal combobox', (await page.getAttribute('#palette', 'role')) === 'dialog' && (await page.getAttribute('#palette-input', 'role')) === 'combobox');
    ok('an empty palette lists every panel and the actions', (await page.locator('#palette-list [role=option]').count()) >= 20);
    await page.keyboard.type('onto'); await page.waitForTimeout(200);
    ok('typing ranks the best match first', /Ontology/.test(await page.locator('#palette-list [role=option]').first().innerText()), await page.locator('#palette-list [role=option]').first().innerText());
    ok('the first match is the active descendant', (await page.getAttribute('#palette-input', 'aria-activedescendant')) === (await page.locator('#palette-list [role=option]').first().getAttribute('id')));
    await page.keyboard.press('Enter'); await page.waitForTimeout(500);
    ok('Enter goes there and closes the palette', (await panel()) === 'ontology' && await page.isHidden('#palette'), await panel());
    await page.keyboard.press('Control+k'); await page.keyboard.press('Escape'); await page.waitForTimeout(200);
    ok('Escape closes it and returns focus to where it was', await page.isHidden('#palette') && (await page.evaluate(() => document.activeElement.tagName)) !== 'BODY');
    await page.click('button[data-panel="query"]'); await page.waitForTimeout(300);
    await page.keyboard.press('Control+k'); await page.keyboard.type('palette q'); await page.waitForTimeout(200);
    ok('saved queries are in the palette', /palette query/.test(await page.locator('#palette-list [role=option]').first().innerText()));
    await page.keyboard.press('Enter'); await page.waitForTimeout(300);
    ok('choosing one loads it into the editor', (await cm()) === 'MATCH Company RETURN *', await cm());
    await page.keyboard.press('Control+k'); await page.keyboard.type('run'); await page.waitForTimeout(200);
    await page.keyboard.press('Enter'); await page.waitForTimeout(1200);
    ok('"Run query" runs the query from anywhere', /\d+ rows? · \d+ ms/.test(await page.innerText('#query-results')), await page.innerText('#query-results'));
    await page.keyboard.press('Control+k'); await page.keyboard.type('zzzqqq'); await page.waitForTimeout(200);
    ok('no match says so, politely', /No matching commands/.test(await page.innerText('#palette-list')) && (await page.locator('#palette-list [role=option]').count()) === 0);
    await page.keyboard.press('Escape');
    await page.keyboard.press('Control+k'); await page.keyboard.press('ArrowDown'); await page.keyboard.press('ArrowDown'); await page.waitForTimeout(100);
    ok('arrow keys move the selection', (await page.locator('#palette-list [role=option][aria-selected=true]').count()) === 1 && (await page.locator('#palette-list [role=option]').nth(2).getAttribute('aria-selected')) === 'true');
    await page.click('#palette-backdrop', { position: { x: 5, y: 5 } }); await page.waitForTimeout(200);
    ok('clicking outside closes it', await page.isHidden('#palette'));
    ok('fuzzy ranking: a prefix beats a scattered match, a miss is rejected', await page.evaluate(() => fuzzyScore('onto', 'Ontology') > fuzzyScore('onto', 'Go to Ontology panel...x') && fuzzyScore('onto', 'Ontology') > fuzzyScore('onty', 'Ontology') && fuzzyScore('zzz', 'Ontology') === -1 && fuzzyScore('', 'anything') === 0));
    await page.evaluate(() => { const o = Object.keys(localStorage).filter(k => k.includes('.saved')); o.forEach(k => localStorage.setItem(k, '[]')); });
    // ---- P1: one working tenant, applied everywhere and remembered ----
    const TENANT_IDS = ['query', 'ontology', 'sources', 'ask', 'graph'];
    const tenantValues = () => page.evaluate(ids => ids.map(i => document.getElementById(i + '-tenant').value), TENANT_IDS);
    await page.selectOption('#tenant-context', 't1'); await page.waitForTimeout(500);
    ok('choosing a tenant in the header applies it to every panel', (await tenantValues()).every(v => v === 't1'), JSON.stringify(await tenantValues()));
    const reqBeforeTenant = requests.length;
    await page.evaluate(() => window.__cmSetValue('MATCH Company RETURN *')); await page.click('[data-click="runQuery"]'); await page.waitForTimeout(900);
    ok('queries run in the working tenant', requests.slice(reqBeforeTenant).some(u => /api\/aikoql\?.*tenant=t1/.test(u)), requests.slice(reqBeforeTenant).join(' '));
    await page.reload(); await page.waitForTimeout(1800);
    ok('the working tenant survives a reload', (await page.inputValue('#tenant-context')) === 't1' && (await tenantValues()).every(v => v === 't1'), JSON.stringify(await tenantValues()));
    await page.click('button[data-panel="ontology"]'); await page.waitForTimeout(400);
    await page.selectOption('#ontology-tenant', ''); await page.waitForTimeout(500);
    ok('a panel override stays local: the header and the other panels do not move', (await page.inputValue('#tenant-context')) === 't1' && (await page.inputValue('#query-tenant')) === 't1' && (await page.inputValue('#sources-tenant')) === 't1', await page.inputValue('#tenant-context'));
    await page.selectOption('#tenant-context', ''); await page.waitForTimeout(300);
    await page.keyboard.press('Control+k'); await page.keyboard.type('tenant t1'); await page.waitForTimeout(200);
    ok('the palette can switch tenant', /Switch tenant: t1/.test(await page.locator('#palette-list [role=option]').first().innerText().catch(() => '')));
    await page.keyboard.press('Enter'); await page.waitForTimeout(500);
    ok('...and does', (await page.inputValue('#tenant-context')) === 't1');
    await page.selectOption('#tenant-context', ''); await page.click('button[data-panel="query"]'); await page.waitForTimeout(300);
    // ---- P1: light and dark, by system preference or by choice ----
    const lum = rgb => { const [r, g, b] = rgb.match(/[\d.]+/g).slice(0, 3).map(Number).map(v => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
    const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
    const css = (sel, prop) => page.evaluate(([s, p]) => getComputedStyle(document.querySelector(s))[p], [sel, prop]);
    const themeAttr = () => page.evaluate(() => document.documentElement.dataset.theme || '');
    await page.evaluate(() => localStorage.removeItem('aikoql.studio.theme')); await page.reload(); await page.waitForTimeout(1500);
    await page.emulateMedia({ colorScheme: 'light' }); await page.waitForTimeout(200);
    ok('with no choice a light system gets the light theme', (await themeAttr()) === '' && lum(await css('body', 'backgroundColor')) > 0.7, await css('body', 'backgroundColor'));
    await page.emulateMedia({ colorScheme: 'dark' }); await page.waitForTimeout(200);
    ok('...and a dark system gets the dark theme', lum(await css('body', 'backgroundColor')) < 0.05, await css('body', 'backgroundColor'));
    await page.click('#theme-toggle'); await page.waitForTimeout(250);
    ok('the toggle makes light explicit, beating a dark system', (await themeAttr()) === 'light' && lum(await css('body', 'backgroundColor')) > 0.7 && /Light/.test(await page.innerText('#theme-toggle')));
    ok('the control announces its state', /Theme: light/.test(await page.getAttribute('#theme-toggle', 'aria-label')));
    await page.reload(); await page.waitForTimeout(1500);
    ok('the choice survives a reload (applied before first paint)', (await themeAttr()) === 'light' && lum(await css('body', 'backgroundColor')) > 0.7);
    ok('light: secondary text is readable on the page and on cards (WCAG AA)', ratio(await css('#panel-title', 'color'), await css('#panel-header', 'backgroundColor')) >= 4.5 && ratio(await css('.panel.active .query-editor-label', 'color'), await css('.panel.active .card', 'backgroundColor')) >= 4.5, await css('.panel.active .query-editor-label', 'color') + ' on ' + await css('.panel.active .card', 'backgroundColor'));
    ok('light: the editor follows the theme (light surface, readable text)', lum(await css('#query-editor .cm-editor', 'backgroundColor')) > 0.8 && ratio(await css('#query-editor .cm-content', 'color'), await css('#query-editor .cm-editor', 'backgroundColor')) >= 7, await css('#query-editor .cm-editor', 'backgroundColor'));
    await page.click('button[data-panel="graph"]'); await page.waitForTimeout(1200);
    ok('the graph draws in the light theme without errors', await page.isVisible('#graph-viz') && realErrors.length === 0, realErrors.slice(0, 2).join(' | '));
    await page.click('button[data-panel="query"]'); await page.waitForTimeout(300);
    await page.click('#theme-toggle'); await page.waitForTimeout(250);
    ok('the next press makes dark explicit, beating a light system', (await themeAttr()) === 'dark' && lum(await css('body', 'backgroundColor')) < 0.05);
    await page.emulateMedia({ colorScheme: 'light' }); await page.waitForTimeout(200);
    ok('dark stays dark when the system is light', lum(await css('body', 'backgroundColor')) < 0.05);
    ok('dark: the editor is dark again', lum(await css('#query-editor .cm-editor', 'backgroundColor')) < 0.05 && ratio(await css('#query-editor .cm-content', 'color'), await css('#query-editor .cm-editor', 'backgroundColor')) >= 7);
    ok('dark: secondary text meets WCAG AA', ratio(await css('.panel.active .query-editor-label', 'color'), await css('.panel.active .card', 'backgroundColor')) >= 4.5);
    await page.keyboard.press('Control+k'); await page.keyboard.type('theme: auto'); await page.waitForTimeout(200); await page.keyboard.press('Enter'); await page.waitForTimeout(300);
    ok('the palette can set the theme, and auto follows the system again', (await themeAttr()) === '' && lum(await css('body', 'backgroundColor')) > 0.7 && /Auto/.test(await page.innerText('#theme-toggle')));
    await page.emulateMedia({ colorScheme: 'dark' }); await page.waitForTimeout(200);
    ok('auto tracks a system change live', lum(await css('body', 'backgroundColor')) < 0.05);
    await page.emulateMedia({ colorScheme: null });
    // ---- P1: a phone ----
    await page.setViewportSize({ width: 390, height: 844 }); await page.waitForTimeout(300);
    ok('on a phone the navigation toggle is shown and the drawer is closed', await page.isVisible('#nav-toggle') && await page.isHidden('#sidebar') && (await page.getAttribute('#nav-toggle', 'aria-expanded')) === 'false');
    await page.click('#nav-toggle'); await page.waitForTimeout(350);
    ok('the toggle opens the drawer, labelled, with a backdrop', await page.isVisible('#sidebar') && (await page.getAttribute('#nav-toggle', 'aria-expanded')) === 'true' && await page.isVisible('#nav-backdrop') && (await css('#sidebar nav button .lbl', 'opacity')) === '1');
    await page.click('button[data-panel="graph"]'); await page.waitForTimeout(500);
    ok('choosing a panel closes the drawer and goes there', (await panel()) === 'graph' && await page.isHidden('#sidebar') && (await page.getAttribute('#nav-toggle', 'aria-expanded')) === 'false', await panel());
    await page.click('#nav-toggle'); await page.waitForTimeout(300); await page.keyboard.press('Escape'); await page.waitForTimeout(300);
    ok('Escape closes it and returns focus to the toggle', await page.isHidden('#sidebar') && (await page.evaluate(() => document.activeElement.id)) === 'nav-toggle');
    await page.click('#nav-toggle'); await page.waitForTimeout(300); await page.click('#nav-backdrop', { position: { x: 380, y: 400 } }); await page.waitForTimeout(300);
    ok('tapping outside closes it', await page.isHidden('#sidebar'));
    const overflowing = [];
    for (const name of await page.$$eval('#sidebar nav button[data-panel]', b => b.map(x => x.dataset.panel))) {
      await page.evaluate(n => { location.hash = '#/' + n; }, name); await page.waitForTimeout(1300);
      const w = await page.evaluate(() => ({ doc: document.scrollingElement.scrollWidth, view: window.innerWidth, content: document.getElementById('panel-content').scrollWidth, contentView: document.getElementById('panel-content').clientWidth, main: document.getElementById('main').getBoundingClientRect().width }));
      if (w.doc > w.view + 1 || w.content > w.contentView + 1 || w.main > w.view + 1) overflowing.push(name + ' ' + JSON.stringify(w));
    }
    ok('no panel makes the page scroll sideways on a phone', overflowing.length === 0, overflowing.join(' ; '));
    await page.evaluate(() => { location.hash = '#/query'; }); await page.waitForTimeout(500);
    const runBox = await page.locator('[data-click="runQuery"]').boundingBox();
    ok('buttons are touch-sized on a phone (>= 40px tall)', runBox && runBox.height >= 40, JSON.stringify(runBox));
    ok('form inputs are 16px so the phone does not zoom on focus', (await css('#query-save-name', 'fontSize')) === '16px', await css('#query-save-name', 'fontSize'));
    await page.setViewportSize({ width: 900, height: 700 }); await page.waitForTimeout(300);
    ok('a tablet keeps the compact rail and no toggle', await page.isHidden('#nav-toggle') && await page.isVisible('#sidebar'));
    await page.setViewportSize({ width: 1280, height: 720 }); await page.waitForTimeout(300);
    ok('on a desktop there is no toggle and the sidebar is open', await page.isHidden('#nav-toggle') && await page.isVisible('#sidebar'));
    await page.evaluate(() => { location.hash = '#/query'; });
    // ---- polish from screenshots ----
    await page.setViewportSize({ width: 1280, height: 560 }); await page.waitForTimeout(300);
    const navScroll = await page.evaluate(() => { const n = document.querySelector('#sidebar nav'); n.scrollTop = n.scrollHeight; const last = [...n.querySelectorAll('button')].pop().getBoundingClientRect(), box = n.getBoundingClientRect(); return { overflowY: getComputedStyle(n).overflowY, scrolls: n.scrollHeight > n.clientHeight, lastInside: last.bottom <= box.bottom + 1 && last.top >= box.top }; });
    ok('on a short window the navigation scrolls and its last entry is reachable', navScroll.overflowY === 'auto' && navScroll.scrolls && navScroll.lastInside, JSON.stringify(navScroll));
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.keyboard.press('Control+k'); await page.keyboard.type('o'); await page.waitForTimeout(200);
    ok('one letter in the palette never offers Sign out first', !/Sign out/.test(await page.locator('#palette-list [role=option]').first().innerText()) && !/Sign out/.test(await page.innerText('#palette-list')));
    await page.keyboard.press('Escape'); await page.keyboard.press('Control+k'); await page.keyboard.type('sign'); await page.waitForTimeout(200);
    ok('...but typing "sign" finds it', /Sign out/.test(await page.locator('#palette-list [role=option]').first().innerText()));
    await page.keyboard.press('Escape');
    ok('bare selects follow the theme (a bordered, themed control)', (await css('#sources-tenant', 'borderTopWidth')) !== '0px' && lum(await css('#sources-tenant', 'backgroundColor')) < 0.05);
    await page.setViewportSize({ width: 390, height: 844 }); await page.evaluate(() => { location.hash = '#/query'; }); await page.waitForTimeout(500);
    const labelBoxes = await page.evaluate(() => [...document.querySelectorAll('.panel.active .query-editor-label span')].filter(e => e.offsetParent !== null).map(e => { const r = e.getBoundingClientRect(); return [r.left, r.top, r.right, r.bottom]; }));
    const overlap = labelBoxes.some((a, i) => labelBoxes.some((b, j) => i < j && a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]));
    ok('on a phone the editor label and its hint never overlap', !overlap, JSON.stringify(labelBoxes));
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.evaluate(() => { location.hash = '#/sources'; }); await page.waitForTimeout(1200);
    ok('a one-table source says "1 container", not "1 containers"', !/\b1 containers\b/.test(await page.innerText('#sources-list')), (await page.innerText('#sources-list')).slice(0, 200));
    await page.evaluate(() => { location.hash = '#/query'; }); await page.waitForTimeout(300);
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
