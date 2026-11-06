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

// A scripted OpenAI-style model. It answers the query skill with a fixed
// MATCH (or a refusal for "unknowable") and the answer skill with a claim that
// quotes the one Company fact and cites its koid (state.companyKoid).
function startChatStub(state) {
  const http = require('http');
  const queryFor = q => /unknowable/.test(q) ? { query: null, refusal_reason: 'cannot be answered' } : { query: 'MATCH Company RETURN *', refusal_reason: null };
  const answerFor = q => ({
    answer: /hostile/.test(q) ? HOSTILE : 'Acme is a company.', grounded: true,
    claims: [{ statement: 'Company Acme: name=Acme', evidence_ids: [state.companyKoid] }], refusal_reason: null,
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
  const chatState = { companyKoid: '' };
  const chatStub = await startChatStub(chatState);
  fs.writeFileSync(path.join(dir, 'aikoql.toml'), `[ingestion]\nmax_pages = 2\n[chat]\nurl = "http://127.0.0.1:${chatStub.address().port}"\nmodel = "e2e-stub"\n`);
  const server = spawn(BIN, ['serve', '--config', path.join(dir, 'aikoql.toml'), '--metrics-addr', `127.0.0.1:${port}`, path.join(dir, 'db')], {
    env: { ...process.env, AIKOQL_ADMIN_PASSWORD: PW }, stdio: ['ignore', 'ignore', 'pipe'],
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
    await page.locator('#ask-result a[data-click="openInInspector"]').first().click({ timeout: 2000 }).catch(() => {});
    await page.waitForTimeout(700);
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
    await page.waitForTimeout(1800);
    ok('the Ask button is disabled while a request is in flight and enabled after',
      inFlight && await page.evaluate(() => !document.getElementById('ask-submit').disabled));
    ok('no CSP violation and no console error on the Ask panel',
      (await page.evaluate(() => window.__csp)).length === 0 && consoleErrors.filter(e => !/Failed to load resource/.test(e)).length === 0, consoleErrors.join(' || '));

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
