// Aikoql Studio application script. Served same-origin at /studio/assets/app.js
// (embedded in the binary); the page has no inline script or handler.
// ═══════════════════════════════════════════
// State
// ═══════════════════════════════════════════
let authToken = null, currentUser = null;
let activePanel = 'query';
let explorerData = null;       // cached schema for explorer tree
let graphNetwork = null;
let graphNodes, graphEdges;
let graphRawData = null;
const COLORS = ['#6cc7f0','#f070a0','#5cd89c','#f0a060','#b89ae0','#f0556a','#f0d060','#60d8b0','#f090c8','#98dc80'];
let typeColors = {}, colorIdx = 0;

function cssVar(name) { return getComputedStyle(document.documentElement).getPropertyValue(name).trim(); }
function colorFor(type) { if(!typeColors[type]) typeColors[type]=COLORS[colorIdx++%COLORS.length]; return typeColors[type]; }

// ═══════════════════════════════════════════
// Theme: follow the system, or be told. Stored globally (it applies before sign-in).
// ═══════════════════════════════════════════
const THEME_KEY = 'aikoql.studio.theme';
const THEME_LABELS = {auto: '◐ Auto', light: '☀ Light', dark: '☾ Dark'};
function savedTheme() {
  try { const t = localStorage.getItem(THEME_KEY); return t === 'light' || t === 'dark' ? t : 'auto'; } catch (_) { return 'auto'; }
}
function effectiveDark() {
  const t = document.documentElement.dataset.theme;
  if (t) return t === 'dark';
  return !window.matchMedia('(prefers-color-scheme: light)').matches;
}
function applyTheme(mode) {
  const root = document.documentElement;
  if (mode === 'auto') delete root.dataset.theme; else root.dataset.theme = mode;
  try { if (mode === 'auto') localStorage.removeItem(THEME_KEY); else localStorage.setItem(THEME_KEY, mode); } catch (_) { /* choice lasts for this page only */ }
  const btn = document.getElementById('theme-toggle');
  if (btn) {
    btn.textContent = THEME_LABELS[mode];
    btn.setAttribute('aria-label', 'Theme: ' + (mode === 'auto' ? 'automatic, follows your system' : mode) + '. Activate to change.');
  }
  if (window.__cmSetTheme) window.__cmSetTheme(effectiveDark());
  // the graph paints with colours read at creation: rebuild it in the new palette
  if (graphNetwork) { graphNetwork.destroy(); graphNetwork = null; if (activePanel === 'graph') initGraph(); }
}
function cycleTheme() {
  const order = ['auto', 'light', 'dark'];
  applyTheme(order[(order.indexOf(savedTheme()) + 1) % order.length]);
}
window.matchMedia('(prefers-color-scheme: light)').addEventListener('change', () => { if (savedTheme() === 'auto') applyTheme('auto'); });

// ═══════════════════════════════════════════
// Auth
// ═══════════════════════════════════════════
function showLogin() {
  document.getElementById('login-overlay').style.display = 'flex';
  document.getElementById('app').style.display = 'none';
  authToken = null; currentUser = null;
}
async function doLogin() {
  const u = document.getElementById('login-user').value.trim();
  const p = document.getElementById('login-pass').value.trim();
  try {
    const res = await fetch('/api/login', { method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({username:u, password:p}) });
    const data = await res.json();
    if (data.token) {
      currentUser = u; // the session itself is the HttpOnly cookie the login set
      document.getElementById('login-overlay').style.display = 'none';
      document.getElementById('app').style.display = 'flex';
      document.getElementById('user-badge').textContent = u + (u==='admin'?' (admin)':'');
      initApp();
    } else { document.getElementById('login-error').textContent = data.error || 'Login failed'; }
  } catch(e) { document.getElementById('login-error').textContent = 'Connection error: ' + e.message; }
}
function logout() {
  if (graphNetwork) { graphNetwork.destroy(); graphNetwork = null; }
  fetch('/api/logout', {method: 'POST', headers: authHeaders()}).catch(() => {}).finally(showLogin);
}
document.getElementById('login-pass').addEventListener('keydown', e => { if(e.key==='Enter') doLogin(); });
// Resume after a reload: the HttpOnly cookie is still valid, so ask the server
// who it is instead of showing the sign-in form again.
window.addEventListener('load', async () => {
  if (new URLSearchParams(location.hash ? location.hash.slice(1) : location.search).has('token')) return; // SSO hand-off owns this
  try {
    const res = await fetch('/api/session', {headers: authHeaders()});
    if (!res.ok) return;
    const me = await res.json();
    currentUser = me.username;
    document.getElementById('login-overlay').style.display = 'none';
    document.getElementById('app').style.display = 'flex';
    document.getElementById('user-badge').textContent = currentUser + (currentUser === 'admin' ? ' (admin)' : '');
    initApp();
  } catch (_) { /* no session: the sign-in form stays */ }
});

// ═══════════════════════════════════════════
// API helper
// ═══════════════════════════════════════════
// The session is an HttpOnly cookie script cannot read; every request also
// carries the custom CSRF header the server requires of cookie-authenticated
// writes. authToken is set only by the SSO hand-off (a Bearer fallback) —
// and never travels in a URL (history, logs, Referer).
function authHeaders(extra) {
  return Object.assign({'X-Aikoql-Csrf': '1'}, extra || {}, authToken ? {'Authorization': 'Bearer ' + authToken} : {});
}
// One transport for every verb: CSRF header, JSON, a dead session returns to
// sign-in (whatever the verb), and one error shape.
async function request(method, url, body) {
  const init = {method: method, headers: authHeaders(body === undefined ? {} : {'Content-Type': 'application/json'})};
  if (body !== undefined) init.body = JSON.stringify(body);
  const res = await fetch(url, init);
  // S-10 (POC-4): a 401 means the server restarted and dropped the session —
  // return to the login overlay instead of leaking the raw body into a panel.
  if (res.status === 401) {
    showLogin();
    document.getElementById('login-error').textContent = 'Session expired — sign in again';
    throw new Error('session expired');
  }
  if (!res.ok) {
    let msg = 'HTTP ' + res.status;
    try { const b = await res.json(); if (b.error) msg = b.error; } catch(_) {}
    throw new Error(msg);
  }
  return res.json();
}
async function api(url) { return request('GET', url); }
async function apiPost(url, body) { return request('POST', url, body || {}); }
async function apiPut(url, body) { return request('PUT', url, body || {}); }

// Notifications: a polite live region; errors are alerts. Text only, never markup.
function toast(kind, text, ms) {
  const box = document.getElementById('toasts');
  if (!box) return;
  const t = document.createElement('div');
  t.className = 'toast toast-' + kind;
  t.setAttribute('role', kind === 'error' ? 'alert' : 'status');
  const msg = document.createElement('span');
  msg.textContent = text;
  const x = document.createElement('button');
  x.className = 'toast-x';
  x.setAttribute('aria-label', 'Dismiss notification');
  x.dataset.click = 'dismissToast';
  x.textContent = '×';
  t.append(msg, x);
  box.append(t);
  while (box.children.length > 4) box.firstChild.remove();
  setTimeout(() => t.remove(), ms || (kind === 'error' ? 9000 : 4000));
}

// Epoch millis → "YYYY-MM-DD HH:MM:SS (13s ago)".
function fmtTime(ms) {
  if (ms === null || ms === undefined || !Number.isFinite(Number(ms))) return '?';
  const iso = new Date(Number(ms)).toISOString().replace('T', ' ').slice(0, 19);
  const secs = Math.round((Date.now() - Number(ms)) / 1000);
  const ago = secs < 0 ? 'future'
    : secs < 60 ? secs + 's ago'
    : secs < 3600 ? Math.round(secs / 60) + 'm ago'
    : secs < 86400 ? Math.round(secs / 3600) + 'h ago'
    : Math.round(secs / 86400) + 'd ago';
  return iso + ' (' + ago + ')';
}

// ═══════════════════════════════════════════
// Navigation
// ═══════════════════════════════════════════
// Phone navigation: a drawer. On wider screens these are no-ops (the rail is always there).
function setNav(open) {
  document.getElementById('app').classList.toggle('nav-open', open);
  document.getElementById('nav-backdrop').hidden = !open;
  const t = document.getElementById('nav-toggle');
  t.setAttribute('aria-expanded', open ? 'true' : 'false');
  t.setAttribute('aria-label', open ? 'Close navigation' : 'Open navigation');
}
function toggleNav() {
  if (document.getElementById('app').classList.contains('nav-open')) { closeNav(); return; }
  setNav(true);
  const first = document.querySelector('#sidebar nav button.active') || document.querySelector('#sidebar nav button');
  if (first) first.focus();
}
function closeNav() {
  if (!document.getElementById('app').classList.contains('nav-open')) return;
  setNav(false);
  document.getElementById('nav-toggle').focus();
}
document.addEventListener('keydown', e => { if (e.key === 'Escape') closeNav(); });
window.addEventListener('resize', () => { if (window.innerWidth >= 800) setNav(false); });

// Routing: a panel is a place with an address (#/ontology). Refresh, Back/Forward
// and a pasted link all land on the same panel. Only #/<panel> hashes are ours:
// the SSO hand-off carries its credentials in the hash and must be left alone.
function panelFromHash() {
  const m = /^#\/([a-z-]+)/.exec(location.hash);
  return m && document.getElementById('panel-' + m[1]) ? m[1] : null;
}
window.addEventListener('hashchange', () => {
  const p = panelFromHash();
  if (p && p !== activePanel) switchPanel(p, true);
});
function switchPanel(name, fromRoute) {
  closeNav();
  activePanel = name;
  if (!fromRoute && location.hash !== '#/' + name) history.pushState(null, '', '#/' + name);
  document.querySelectorAll('#sidebar nav button').forEach(b => {
    const on = b.dataset.panel === name;
    b.classList.toggle('active', on);
    if (on) b.setAttribute('aria-current', 'page'); else b.removeAttribute('aria-current');
  });
  document.querySelectorAll('.panel').forEach(p => p.classList.remove('active'));
  const panel = document.getElementById('panel-' + name);
  if (panel) panel.classList.add('active');
  const titles = { query:'Query Editor', graph:'Knowledge Graph', explorer:'Knowledge Explorer', schema:'Schema Explorer', ontology:'Ontology', inspector:'KO Inspector', admin:'Administration', timeline:'Timeline', provenance:'Provenance', debugger:'Program Debugger', benchmarks:'Benchmark Center', profiler:'Query Profiler', providers:'Provider Manager', documents:'Document Explorer', ask:'Ask', sources:'Data Sources' };
  document.getElementById('panel-title').textContent = titles[name] || name;
  document.title = (titles[name] || name) + ' · Aikoql Studio';
  document.getElementById('panel-breadcrumb').textContent = '';
  if (name === 'graph') initGraph();
  if (name === 'explorer') loadExplorerTree();
  if (name === 'schema') loadSchemaPanel();
  if (name === 'ontology') loadOntologyPanel();
  if (name === 'admin') { loadAdminPanel(); loadUsers(); }
  if (name === 'providers') loadProviders();
  if (name === 'debugger') loadDebuggerPrograms();
  if (name === 'benchmarks') loadBenchmarks();
  if (name === 'documents') loadDocuments();
  if (name === 'sources') loadSourcesPanel();
  if (name === 'ask') loadAskPanel();
}
document.querySelectorAll('#sidebar nav button').forEach(b => {
  b.addEventListener('click', () => switchPanel(b.dataset.panel));
});

// ═══════════════════════════════════════════
// Status bar
// ═══════════════════════════════════════════
async function updateStatus() {
  try {
    const h = await api('/health');
    document.getElementById('status-left').innerHTML = '<span class="dot ok"></span> ' + esc((h.status||'ok'));
    document.getElementById('status-right').textContent = 'uptime ' + (h.uptime_seconds||0).toFixed(0) + 's';
  } catch(e) { document.getElementById('status-left').innerHTML = '<span class="dot" style="background:var(--red)"></span> Offline'; }
}
setInterval(updateStatus, 15000);

// ═══════════════════════════════════════════
// Init
// ═══════════════════════════════════════════
function initApp() {
  // land where the address says (or where the person was), and make the address true
  const first = panelFromHash() || 'query';
  if (location.hash !== '#/' + first && !/^#[a-z]+=/.test(location.hash)) history.replaceState(null, '', '#/' + first);
  switchPanel(first, true);
  updateStatus();
  loadTenants();
  loadUsers();
  // Init CodeMirror 6 on the query editor div.
  if (window.__initCodeMirror) window.__initCodeMirror();
  if (window.__cmSetTheme) window.__cmSetTheme(effectiveDark());
  applyTheme(savedTheme());
  wireDraft();
}

// Note: Ctrl+Enter query execution is handled inside CodeMirror's contentDOM listener
// (see window.__initCodeMirror). Ctrl+S save feedback is below.
// One working tenant for the whole console. The header sets every panel's selector; a
// panel can still be pointed elsewhere, and that stays LOCAL to the panel: a view filter
// (the graph's tenant) must never silently change where another panel's writes land.
const TENANT_SELECTS = ['query-tenant', 'ontology-tenant', 'sources-tenant', 'ask-tenant', 'graph-tenant'];
let tenantContext = '';
function applyTenantContext() {
  TENANT_SELECTS.forEach(id => {
    const sel = document.getElementById(id);
    if (sel && sel.value !== tenantContext) sel.value = tenantContext;
  });
  const g = document.getElementById('tenant-context');
  if (g && g.value !== tenantContext) g.value = tenantContext;
}
function setTenantContext(el) {
  tenantContext = el.value;
  store.set('tenant', tenantContext);
  applyTenantContext();
  // the panel on screen re-reads its data under the new tenant
  const sel = document.getElementById(activePanel + '-tenant');
  if (sel) sel.dispatchEvent(new Event('change', {bubbles: true}));
}
function loadTenants() {
  api('/api/schema').then(d => {
    if (!d.schema) return;
    // Populate dynamic type names for CodeMirror autocomplete.
    window.__schemaTypes = Object.keys(d.schema);
    const propNames = new Set();
    Object.values(d.schema).forEach(i => (i.properties||[]).forEach(p => propNames.add(p)));
    window.__schemaProps = [...propNames];
    const tenants = new Set();
    Object.values(d.schema).forEach(info => { if (info.tenants) info.tenants.forEach(t => tenants.add(t)); });
    const opts = '<option value="">Default tenant</option>' + [...tenants].map(t => '<option value="'+esc(t)+'">'+esc(t)+'</option>').join('');
    document.getElementById('query-tenant').innerHTML = opts;
    const ot = document.getElementById('ontology-tenant'), cur = ot.value;
    ot.innerHTML = '<option value="">Global</option>' + [...tenants].map(t => '<option value="'+esc(t)+'">'+esc(t)+'</option>').join('');
    ot.value = cur;
    const st = document.getElementById('sources-tenant'), srcCur = st.value;
    st.innerHTML = '<option value="">Global</option>' + [...tenants].map(t => '<option value="'+esc(t)+'">'+esc(t)+'</option>').join('');
    st.value = srcCur;
    const at = document.getElementById('ask-tenant'), askCur = at.value;
    at.innerHTML = '<option value="">All tenants</option>' + [...tenants].map(t => '<option value="'+esc(t)+'">'+esc(t)+'</option>').join('');
    at.value = askCur;
    const gctx = document.getElementById('tenant-context');
    gctx.innerHTML = '<option value="">Each panel’s default</option>' + [...tenants].map(t => '<option value="'+esc(t)+'">'+esc(t)+'</option>').join('');
    tenantContext = store.get('tenant', '');
    document.getElementById('graph-tenant').innerHTML = '<option value="">All tenants</option>' + [...tenants].map(t => '<option>'+esc(t)+'</option>').join('');
    applyTenantContext();
  }).catch(() => {});
}

// ═══════════════════════════════════════════
// Panel: Query Editor
// ═══════════════════════════════════════════
async function runQuery() {
  const query = window.__cmGetValue().trim();
  if (!query) return;
  const resDiv = document.getElementById('query-results');
  resDiv.innerHTML = '<div class="loading">Running...</div>';
  const tenant = document.getElementById('query-tenant').value;
  let url = '/api/aikoql?query=' + encodeURIComponent(query);
  if (tenant) url += '&tenant=' + encodeURIComponent(tenant);
  const started = performance.now();
  try {
    const data = await api(url);
    pushHistory(query, !data.error);
    if (data.error) {
      // S-8 (POC-4): an unknown type is fixable from the studio — point at
      // the discovery flow instead of leaving a dead end.
      resDiv.innerHTML = '<div class="error-text">'+esc(data.error)+'</div>' + esc((data.error.includes('AIKOQL1030') ? '<div style="font-size:10px;color:var(--muted);margin-top:4px;">type not registered — run Ontology → Auto-Discover</div>' : ''));
      return;
    }
    if (data.created) { resDiv.innerHTML = '<div class="card"><span class="badge badge-green">Created</span> '+esc(data.created)+' v'+esc(data.version)+'</div>'; return; }
    if (data.results && data.results.length > 0) {
      renderResults(resDiv, data.results, Math.round(performance.now() - started));
    } else { resDiv.innerHTML = '<div class="empty-state">Query returned 0 results.</div>'; }
  } catch(e) { pushHistory(query, false); resDiv.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}
window.runQuery = runQuery;
async function explainQuery() {
  const query = window.__cmGetValue().trim();
  if (!query) return;
  const resDiv = document.getElementById('query-results');
  resDiv.innerHTML = '<div class="loading">Explaining...</div>';
  try {
    const data = await api('/api/explain?query=' + encodeURIComponent(query));
    if (data.error) { resDiv.innerHTML = '<div class="error-text">'+esc(data.error)+'</div>'; return; }
    let html = '<div class="card"><h3>Query Plan — '+esc(data.operator_count||0)+' operators</h3>';
    html += '<div class="mono" style="color:var(--muted);margin-bottom:8px;">'+esc(data.query)+'</div>';
    html += '<table><thead><tr><th>#</th><th>Operator</th></tr></thead><tbody>';
    (data.operators||[]).forEach((op,i) => { html += '<tr><td style="color:var(--muted);">'+esc(i+1)+'</td><td class="mono">'+esc(op)+'</td></tr>'; });
    html += '</tbody></table></div>';
    resDiv.innerHTML = html;
  } catch(e) { resDiv.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}
// ── Results table: ONE component for every query result ──────────────────
// Sortable, copyable as JSON/CSV, downloadable, each row linked to its object.
// Display is capped (copy/download carry every row).
let lastResult = null;
const RESULT_DISPLAY_CAP = 1000;
function cellText(v) {
  if (v === null || v === undefined) return '';
  return typeof v === 'object' ? JSON.stringify(v) : String(v);
}
function resultColumns(results) {
  const keys = new Set();
  results.forEach(r => {
    Object.keys(r).forEach(k => { if (k !== 'properties') keys.add(k); });
    if (r.properties) Object.keys(r.properties).forEach(k => keys.add(k));
  });
  return [...keys];
}
function resultMatrix(results, cols) {
  return results.map(r => cols.map(k => {
    let v = r[k];
    if (v === undefined && r.properties) v = r.properties[k];
    return v === undefined ? null : v;
  }));
}
// Numbers by value, everything else by text with numeric-aware collation; empty cells last.
function compareCells(a, b) {
  if (typeof a === 'number' && typeof b === 'number') return a - b;
  return cellText(a).localeCompare(cellText(b), undefined, {numeric: true});
}
function sortedOrder(rows, sort) {
  const order = rows.map((_, i) => i);
  if (!sort) return order;
  return order.sort((x, y) => {
    const a = rows[x][sort.col], b = rows[y][sort.col];
    const an = a === null || a === '', bn = b === null || b === '';
    if (an || bn) return an === bn ? x - y : (an ? 1 : -1);   // empties stay last in both directions
    const c = compareCells(a, b);
    return c !== 0 ? (sort.desc ? -c : c) : x - y;
  });
}
function csvCell(v) {
  let t = cellText(v);
  // a text cell that a spreadsheet would run as a formula is defused with a leading quote
  if (typeof v === 'string' && /^[=+\-@\t\r]/.test(t)) t = "'" + t;
  return /[",\r\n]/.test(t) ? '"' + t.replace(/"/g, '""') + '"' : t;
}
function resultsToCsv(cols, rows) {
  return [cols].concat(rows).map(r => r.map(csvCell).join(',')).join('\r\n') + '\r\n';
}
function tableHtml(cols, rows, order, sort, sortable) {
  let html = '<table><thead><tr>';
  cols.forEach((k, i) => {
    if (!sortable) { html += '<th>'+esc(k)+'</th>'; return; }
    const dir = sort && sort.col === i ? (sort.desc ? 'descending' : 'ascending') : 'none';
    const arrow = dir === 'none' ? '' : (dir === 'ascending' ? ' ▲' : ' ▼');
    html += '<th aria-sort="'+esc(dir)+'"><button class="th-btn" data-click="sortResults" data-col="'+esc(i)+'" data-colname="'+esc(k)+'">'+esc(k)+'<span aria-hidden="true">'+esc(arrow)+'</span></button></th>';
  });
  html += '</tr></thead><tbody>';
  order.slice(0, RESULT_DISPLAY_CAP).forEach(ri => {
    html += '<tr>';
    cols.forEach((k, ci) => {
      const v = rows[ri][ci], t = cellText(v);
      if (k === 'koid' && t) {
        html += '<td class="mono"><a href="#" data-koid="'+esc(t)+'" data-click="openInInspector" title="Open '+esc(t)+' in the Inspector" style="color:var(--accent);">'+esc(t.substring(0, 16))+'…</a></td>';
      } else {
        html += '<td title="'+esc(t)+'">'+esc(t.length > 80 ? t.substring(0, 80) + '…' : t)+'</td>';
      }
    });
    html += '</tr>';
  });
  return html + '</tbody></table>';
}
function resultsHtml() {
  const r = lastResult, order = sortedOrder(r.rows, r.sort);
  const n = r.rows.length;
  const html = tableHtml(r.cols, r.rows, order, r.sort, true);
  let h = '<div class="res-bar">'
    + '<button class="btn btn-sec btn-sm" data-click="copyResults" data-kind="json">Copy JSON</button>'
    + '<button class="btn btn-sec btn-sm" data-click="copyResults" data-kind="csv">Copy CSV</button>'
    + '<button class="btn btn-sec btn-sm" data-click="downloadResults" data-kind="csv">Download CSV</button>'
    + '<button class="btn btn-sec btn-sm" data-click="downloadResults" data-kind="json">Download JSON</button></div>';
  h += '<div class="card" style="padding:0;overflow-x:auto;">' + html + '</div>';
  h += '<div class="res-count">'+esc(n)+' row'+esc(n !== 1 ? 's' : '')+' · '+esc(r.ms)+' ms</div>';
  if (n > RESULT_DISPLAY_CAP) h += '<div class="res-note">Showing the first '+esc(RESULT_DISPLAY_CAP)+' of '+esc(n)+' rows — copy or download to get all of them.</div>';
  return h;
}
function renderResults(box, results, ms) {
  const cols = resultColumns(results);
  lastResult = {cols: cols, rows: resultMatrix(results, cols), raw: results, sort: null, ms: ms};
  box.innerHTML = resultsHtml();
}
function sortResults(el) {
  if (!lastResult) return;
  const col = Number(el.dataset.col), s = lastResult.sort;
  lastResult.sort = !s || s.col !== col ? {col: col, desc: false} : {col: col, desc: !s.desc};
  document.getElementById('query-results').innerHTML = resultsHtml();
  const again = document.querySelector('#query-results th button[data-col="' + col + '"]');
  if (again) again.focus();
}
function exportPayload(kind) {
  const r = lastResult, order = sortedOrder(r.rows, r.sort);
  return kind === 'csv'
    ? {text: resultsToCsv(r.cols, order.map(i => r.rows[i])), mime: 'text/csv;charset=utf-8', ext: 'csv', what: 'CSV'}
    : {text: JSON.stringify(order.map(i => r.raw[i]), null, 2), mime: 'application/json', ext: 'json', what: 'JSON'};
}
async function copyResults(el) {
  if (!lastResult) return;
  const p = exportPayload(el.dataset.kind);
  try {
    await navigator.clipboard.writeText(p.text);
  } catch (_) {
    const ta = document.createElement('textarea');
    ta.value = p.text; ta.style.position = 'fixed'; ta.style.opacity = '0';
    document.body.append(ta); ta.select();
    let ok = false;
    try { ok = document.execCommand('copy'); } catch (__) { ok = false; }
    ta.remove();
    if (!ok) { toast('error', 'Could not copy — use Download instead.'); return; }
  }
  toast('success', 'Copied ' + lastResult.rows.length + ' row' + (lastResult.rows.length !== 1 ? 's' : '') + ' as ' + p.what + '.');
}
function downloadResults(el) {
  if (!lastResult) return;
  const p = exportPayload(el.dataset.kind);
  const url = URL.createObjectURL(new Blob([p.text], {type: p.mime}));
  const a = document.createElement('a');
  a.href = url;
  a.download = 'aikoql-results-' + new Date().toISOString().slice(0, 19).replace(/[-:T]/g, '') + '.' + p.ext;
  document.body.append(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  toast('success', 'Downloaded ' + a.download + '.');
}
async function streamQuery() {
  const query = window.__cmGetValue().trim();
  if (!query) return;
  const resDiv = document.getElementById('query-results');
  resDiv.innerHTML = '<div class="loading">Running; rows will be shown in chunks of 100 (client-side)...</div>';
  let chunks = [];
  // ponytail: streaming via REST fallback — call /api/aikoql repeatedly with LIMIT/OFFSET.
  // True MCP notifications/notify streaming requires a WebSocket or SSE transport on the REST side.
  // For now: run the query and chunk the results client-side.
  try {
    const data = await api('/api/aikoql?query=' + encodeURIComponent(query));
    if (data.error) { resDiv.innerHTML = '<div class="error-text">'+esc(data.error)+'</div>'; return; }
    if (!data.results || data.results.length === 0) { resDiv.innerHTML = '<div class="empty-state">0 results.</div>'; return; }
    const all = data.results;
    const CHUNK = 100;
    for (let i = 0; i < all.length; i += CHUNK) {
      chunks.push(all.slice(i, i + CHUNK));
    }
    let html = '';
    chunks.forEach((chunk, ci) => {
      const cols = resultColumns(chunk);
      const h = tableHtml(cols, resultMatrix(chunk, cols), chunk.map((_, i) => i), null, false);
      html += '<div class="card" style="border-left: 2px solid var(--accent);"><h3>Chunk '+esc(ci+1)+'/'+esc(chunks.length)+' ('+esc(chunk.length)+' rows)</h3>' + h + '</div>';
    });
    html += '<div style="font-size:10px;color:var(--muted);margin-top:4px;">'+esc(all.length)+' total rows in '+esc(chunks.length)+' chunk'+esc(chunks.length!==1?'s':'')+'</div>';
    resDiv.innerHTML = html;
  } catch(e) { resDiv.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}
// Workspace memory: what a person typed, saved and ran survives a reload. Browser
// storage can be absent, blocked or full, so every access is guarded and a failed
// save says so instead of pretending.
const store = {
  key(k) { return 'aikoql.studio.' + (currentUser || '') + '.' + k; },
  get(k, d) { try { const v = localStorage.getItem(store.key(k)); return v === null ? d : JSON.parse(v); } catch (_) { return d; } },
  set(k, v) { try { localStorage.setItem(store.key(k), JSON.stringify(v)); return true; } catch (_) { return false; } },
};
let draftTimer = null, draftWired = false;
function persistDraft() {
  clearTimeout(draftTimer);
  draftTimer = setTimeout(() => store.set('draft', window.__cmGetValue()), 400);
}
function wireDraft() {
  if (!draftWired) {
    draftWired = true;
    const ed = document.getElementById('query-editor');
    ['keyup', 'input'].forEach(ev => ed.addEventListener(ev, persistDraft));
    window.addEventListener('pagehide', () => store.set('draft', window.__cmGetValue()));
  }
  const d = store.get('draft', '');
  if (typeof d === 'string' && d && !window.__cmGetValue()) window.__cmSetValue(d);
  renderLibrary();
}
function firstLine(q) {
  const l = String(q).split('\n').map(x => x.trim()).find(Boolean) || '';
  return l.length > 90 ? l.slice(0, 87) + '…' : l;
}
function pushHistory(q, ok) {
  const h = store.get('history', []).filter(x => x && x.q !== q);
  h.unshift({q: q, ok: ok, at: Date.now()});
  store.set('history', h.slice(0, 50));
  renderLibrary();
}
function renderLibrary() {
  const saved = store.get('saved', []), hist = store.get('history', []);
  document.getElementById('query-saved').innerHTML = saved.length
    ? saved.map((s, i) => '<div class="lib-item"><button class="lib-load" data-click="loadQuery" data-kind="saved" data-index="'+esc(i)+'" title="'+esc(s.q)+'"><b>'+esc(s.name)+'</b> <span class="mono">'+esc(firstLine(s.q))+'</span></button><button class="toast-x" data-click="deleteSaved" data-index="'+esc(i)+'" aria-label="Delete saved query '+esc(s.name)+'">×</button></div>').join('')
    : '<div class="empty-state">Nothing saved yet — name a query above and press Save.</div>';
  document.getElementById('query-history').innerHTML = hist.length
    ? hist.map((h, i) => '<div class="lib-item"><button class="lib-load" data-click="loadQuery" data-kind="history" data-index="'+esc(i)+'" title="'+esc(h.q)+'"><span class="badge '+esc(h.ok ? 'badge-green' : 'badge-red')+'">'+esc(h.ok ? 'ok' : 'error')+'</span> <span class="mono">'+esc(firstLine(h.q))+'</span> <span class="lib-when">'+esc(fmtTime(h.at))+'</span></button></div>').join('')
    : '<div class="empty-state">Queries you run appear here.</div>';
}
function saveQuery() {
  const q = window.__cmGetValue().trim();
  if (!q) { toast('error', 'Write a query first.'); return; }
  const input = document.getElementById('query-save-name');
  const name = input.value.trim().slice(0, 60);
  if (!name) { toast('info', 'Give the query a name, then press Save.'); input.focus(); return; }
  const saved = store.get('saved', []);
  const at = saved.findIndex(s => s.name === name);
  const entry = {name: name, q: q, at: Date.now()};
  if (at >= 0) saved[at] = entry; else saved.unshift(entry);
  if (!store.set('saved', saved.slice(0, 100))) { toast('error', 'Could not save in this browser (storage is blocked or full).'); return; }
  input.value = '';
  renderLibrary();
  toast('success', (at >= 0 ? 'Updated “' : 'Saved “') + name + '”');
}
function loadQuery(el) {
  const list = store.get(el.dataset.kind === 'saved' ? 'saved' : 'history', []);
  const entry = list[Number(el.dataset.index)];
  if (!entry) return;
  window.__cmSetValue(entry.q);
  persistDraft();
}
function deleteSaved(el) {
  const saved = store.get('saved', []);
  saved.splice(Number(el.dataset.index), 1);
  if (!store.set('saved', saved)) { toast('error', 'Could not update saved queries in this browser.'); return; }
  renderLibrary();
}
function clearHistory() { store.set('history', []); renderLibrary(); }
// ── Command palette (Ctrl/Cmd+K): jump anywhere, run anything, from the keyboard ──
let paletteItems = [], paletteIdx = 0, lastFocus = null;
// Subsequence match, scored: consecutive letters, word starts and a prefix rank higher; -1 = no match.
function fuzzyScore(q, text) {
  q = q.toLowerCase().replace(/\s+/g, '');
  const t = text.toLowerCase();
  if (!q) return 0;
  let score = 0, ti = 0, prev = -2;
  for (let qi = 0; qi < q.length; qi++) {
    const idx = t.indexOf(q[qi], ti);
    if (idx === -1) return -1;
    score += 1;
    if (idx === prev + 1) score += 3;
    if (idx === 0 || /[\s_\-:./]/.test(t[idx - 1])) score += 4;
    prev = idx; ti = idx + 1;
  }
  if (t.startsWith(q)) score += 10;
  return score - (ti - q.length) * 0.05;
}
function paletteCommands() {
  const cmds = [];
  document.querySelectorAll('#sidebar nav button[data-panel]').forEach(b => {
    cmds.push({label: 'Go to ' + b.querySelector('.lbl').textContent.trim(), hint: 'Panel', run: () => switchPanel(b.dataset.panel)});
  });
  const onQuery = fn => () => { switchPanel('query'); fn(); };
  cmds.push({label: 'Run query', hint: 'Query', run: onQuery(runQuery)});
  cmds.push({label: 'Explain query plan', hint: 'Query', run: onQuery(explainQuery)});
  cmds.push({label: 'Save query', hint: 'Query', run: onQuery(saveQuery)});
  if (lastResult) {
    cmds.push({label: 'Copy results as CSV', hint: 'Results', run: () => copyResults({dataset: {kind: 'csv'}})});
    cmds.push({label: 'Copy results as JSON', hint: 'Results', run: () => copyResults({dataset: {kind: 'json'}})});
  }
  document.querySelectorAll('#tenant-context option').forEach(o => {
    const sel = document.getElementById('tenant-context');
    cmds.push({label: o.value ? 'Switch tenant: ' + o.value : 'Use each panel’s default tenant', hint: 'Tenant', run: () => { sel.value = o.value; setTenantContext(sel); }});
  });
  ['auto', 'light', 'dark'].forEach(m => cmds.push({label: 'Theme: ' + m, hint: 'Appearance', run: () => applyTheme(m)}));
  cmds.push({label: 'Sign out', hint: 'Session', risky: true, run: logout});
  const load = q => () => { switchPanel('query'); window.__cmSetValue(q); persistDraft(); };
  store.get('saved', []).forEach(s => cmds.push({label: 'Load saved: ' + s.name, hint: firstLine(s.q), run: load(s.q)}));
  store.get('history', []).slice(0, 10).forEach(h => cmds.push({label: 'Recent: ' + firstLine(h.q), hint: h.ok ? 'ok' : 'error', run: load(h.q)}));
  return cmds;
}
function renderPalette() {
  const q = document.getElementById('palette-input').value;
  // a command that ends the session needs three typed characters (or an empty query, where it is listed last)
  const typed = q.replace(/\s+/g, '').length;
  paletteItems = paletteCommands().map(c => ({c: c, s: fuzzyScore(q, c.label)})).filter(x => x.s >= 0 && (!x.c.risky || typed === 0 || typed >= 3))
    .map((x, i) => Object.assign(x, {i: i})).sort((a, b) => b.s - a.s || a.i - b.i);
  paletteIdx = 0;
  document.getElementById('palette-list').innerHTML = paletteItems.length
    ? paletteItems.map((x, i) => '<li role="option" id="pal-'+esc(i)+'" aria-selected="'+esc(i === 0)+'" data-click="paletteRun" data-index="'+esc(i)+'"><span>'+esc(x.c.label)+'</span><span class="pal-hint">'+esc(x.c.hint || '')+'</span></li>').join('')
    : '<li class="pal-empty" role="presentation">No matching commands</li>';
  document.getElementById('palette-input').setAttribute('aria-activedescendant', paletteItems.length ? 'pal-0' : '');
}
function movePalette(to) {
  if (!paletteItems.length) return;
  paletteIdx = (to + paletteItems.length) % paletteItems.length;
  document.querySelectorAll('#palette-list [role=option]').forEach((li, i) => li.setAttribute('aria-selected', i === paletteIdx ? 'true' : 'false'));
  const li = document.getElementById('pal-' + paletteIdx);
  document.getElementById('palette-input').setAttribute('aria-activedescendant', 'pal-' + paletteIdx);
  if (li) li.scrollIntoView({block: 'nearest'});
}
function openPalette() {
  if (!currentUser) return;
  lastFocus = document.activeElement;
  document.getElementById('palette-backdrop').hidden = false;
  document.getElementById('palette').hidden = false;
  const input = document.getElementById('palette-input');
  input.value = '';
  renderPalette();
  input.focus();
}
function closePalette() {
  document.getElementById('palette').hidden = true;
  document.getElementById('palette-backdrop').hidden = true;
  if (lastFocus && document.contains(lastFocus)) lastFocus.focus();
}
function runPaletteItem(i) {
  const item = paletteItems[i];
  if (!item) return;
  closePalette();
  item.c.run();
}
function paletteKey(e) {
  if (e.key === 'Escape') { e.preventDefault(); closePalette(); }
  else if (e.key === 'ArrowDown') { e.preventDefault(); movePalette(paletteIdx + 1); }
  else if (e.key === 'ArrowUp') { e.preventDefault(); movePalette(paletteIdx - 1); }
  else if (e.key === 'Enter') { e.preventDefault(); runPaletteItem(paletteIdx); }
  else if (e.key === 'Tab') { e.preventDefault(); }   // the dialog is one control: focus stays in it
}
document.getElementById('palette-input').addEventListener('input', renderPalette);
document.getElementById('palette-input').addEventListener('keydown', paletteKey);
document.addEventListener('keydown', e => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
    if (!currentUser) return;
    e.preventDefault();
    if (document.getElementById('palette').hidden) openPalette(); else closePalette();
  }
}, true);
// Ctrl/Cmd+S saves the query under the name in the box, or asks for the name.
document.addEventListener('keydown', e => {
  if ((e.ctrlKey || e.metaKey) && e.key === 's' && activePanel === 'query') {
    e.preventDefault();
    saveQuery();
  }
});


// ═══════════════════════════════════════════
// Panel: Knowledge Graph
// ═══════════════════════════════════════════
function initGraph() {
  if (graphNetwork) return;
  graphNodes = new vis.DataSet([]);
  graphEdges = new vis.DataSet([]);
  const container = document.getElementById('graph-viz');
  graphNetwork = new vis.Network(container, {nodes:graphNodes, edges:graphEdges}, {
    physics: { solver:'forceAtlas2Based', forceAtlas2Based:{gravitationalConstant:-35,centralGravity:0.008,springLength:160,springConstant:0.03} },
    edges: { arrows:{to:{enabled:true,scaleFactor:0.5}}, smooth:{type:'continuous',roundness:0.3}, font:{size:9,color:cssVar('--muted'),strokeWidth:0}, width:1.5 },
    nodes: { shape:'dot', font:{size:11,color:cssVar('--text'),face:'sans-serif',strokeWidth:1,strokeColor:cssVar('--panel')}, borderWidth:1.5, shadow:{enabled:true,size:6} },
    interaction: { hover:true, navigationButtons:true, keyboard:true },
  });
  graphNetwork.on('click', p => { if(p.nodes.length>0) inspectNode(p.nodes[0]); });
  graphNetwork.on('doubleClick', p => { if(p.nodes.length>0) { document.getElementById('graph-search').value=p.nodes[0]; loadGraph(); } });
  graphNetwork.on('hoverNode', p => showGraphTooltip(p.node));
  graphNetwork.on('blurNode', () => { const tt = document.getElementById('graph-tooltip'); if(tt) tt.style.display='none'; });
}
function loadGraphAll() { graphDocView = null; document.getElementById('graph-search').value=''; document.getElementById('graph-tenant').value=''; loadGraph(); }
// One saved document drawn a page at a time (the knowledge base holds all of it; a browser draws a page).
let graphDocView = null;
function viewDocumentGraph(koid) {
  graphDocView = {koid: koid, kind: 'entity', offset: 0, size: 200};
  switchPanel('graph');
  return loadGraph();
}
function graphKind(kind) { if (graphDocView) { graphDocView.kind = kind; graphDocView.offset = 0; return loadGraph(); } }
function graphPage(dir) { if (graphDocView) { graphDocView.offset = Math.max(0, graphDocView.offset + dir * graphDocView.size); return loadGraph(); } }
async function graphPickDocument(koid) { if (!koid) { graphDocView = null; return loadGraph(); } return viewDocumentGraph(koid); }
async function renderGraphBanner(data) {
  const el = document.getElementById('graph-banner');
  let h = '';
  if (data.view) {
    const v = data.view, m = data.manifest || {};
    h += '<b>'+esc((data.nodes.find(n => n.koid === data.document) || {}).label || 'Document')+'</b> — ';
    if (m.complete) h += '<span class="badge badge-green">✓ complete in the knowledge base</span> ';
    else h += '<span class="badge badge-orange">saved incompletely — see the Documents tab</span> ';
    h += '<span class="mono" style="font-size:10px;">'+esc(kbCounts(m))+'</span>';
    h += '<div style="margin-top:4px;">Drawing <b>'+esc(v.total ? v.offset + 1 : 0)+'–'+esc(v.offset + v.shown)+' of '+esc(v.total)+'</b> '+esc(KIND_LABEL[v.kind])+' (most connected first) — the graph draws a page because a browser cannot draw them all; the rest are stored and one click away. ';
    BROWSE_KINDS.forEach(k => { h += '<button class="btn '+esc(k === v.kind ? 'btn-pri' : 'btn-sec')+' btn-sm" data-click="graphKind" data-kind="'+esc(k)+'">'+esc(KIND_LABEL[k])+' ('+esc((v.totals || {})[k])+')</button> '; });
    h += '<button class="btn btn-sec btn-sm" data-click="graphPage" data-dir="-1" '+esc(v.offset > 0 ? '' : 'disabled')+'>Previous page</button> <button class="btn btn-sec btn-sm" data-click="graphPage" data-dir="1" '+esc(v.next_offset !== null && v.next_offset !== undefined ? '' : 'disabled')+'>Next page</button> <button class="btn btn-sec btn-sm" data-click="browseDocument" data-koid="'+esc(data.document)+'">Browse all as a table</button></div>';
  } else if (data.hidden && data.hidden.document_objects > 0) {
    h += esc(data.hidden.document_objects.toLocaleString())+' saved document objects are in the knowledge base but are not drawn here (they would bury everything else). Pick a document to see its graph: ';
  }
  const sel = document.getElementById('graph-document');
  try {
    const listed = await api('/api/v1/list-documents');
    const docs = (listed.documents || (listed.data || {}).documents || []).filter(d => d.graph && d.graph.status === 'saved');
    let o = '<option value="">All objects (documents hidden)</option>';
    docs.forEach(d => { o += '<option value="'+esc(d.koid)+'"'+esc(graphDocView && graphDocView.koid === d.koid ? ' selected' : '')+'>'+esc(d.filename)+'</option>'; });
    sel.innerHTML = o;
    sel.style.display = docs.length ? '' : 'none';
  } catch(_) {}
  el.innerHTML = h;
  el.style.display = h ? '' : 'none';
}
async function loadGraph() {
  initGraph();
  const koid = document.getElementById('graph-search').value.trim();
  const tenant = document.getElementById('graph-tenant').value;
  let url = '/api/graph';
  let params = [];
  params.push('detail=1'); // S-1/S-2 (POC-4): extensions drive epistemic styling + virtual retraction edges
  if (graphDocView) {
    params.push('document='+encodeURIComponent(graphDocView.koid), 'kind='+encodeURIComponent(graphDocView.kind), 'offset='+graphDocView.offset, 'limit='+graphDocView.size);
  } else {
    if (koid) params.push('koid='+encodeURIComponent(koid));
    if (tenant) params.push('tenant='+encodeURIComponent(tenant));
  }
  if (params.length) url += '?'+params.join('&');
  try {
    const data = await api(url);
    graphRawData = data;
    typeColors={}; colorIdx=0;
    graphNodes.clear(); graphEdges.clear();
    // a dense view (a saved document's page) is drawn small, labelled on its hubs only, without edge labels:
    // everything is still in the tooltip, the Inspector and Browse all
    const dense = data.nodes.length > 80;
    const degrees = data.nodes.map(n => n.edge_count || 0).sort((a, b) => b - a);
    const labelMin = dense ? (degrees[Math.min(24, degrees.length - 1)] || 0) : 0;
    const nodes = data.nodes.map(n => {
      // S-1 (POC-4): retracted rows keep lifecycle.state "draft" — style from
      // the epistemic extension: superseded → dashed hollow circle + muted
      // palette, so the retraction is visible at a glance.
      const es = n.extensions&&n.extensions.epistemic_status;
      const superseded = es==='superseded';
      return {
        id: n.koid, label: (dense && (n.edge_count || 0) < labelMin && n.type_name !== 'aikoql:document') ? '' : (n.label||n.type_name)+(n.tenant?'\n@'+n.tenant:''),
        title: nodeTip(n, es),
        color: superseded
          ? {background:'#3d3d4a',border:'#8a8a99',highlight:{background:'#4a4a58',border:'#fff'}}
          : {background:colorFor(n.type_name),border:(n.tenant?'#b89ae0':'#252538'),highlight:{background:colorFor(n.type_name),border:'#fff'}},
        shape: superseded?'circle':'dot',
        shapeProperties: superseded?{borderDashes:[4,3]}:{},
        size: dense ? (n.type_name === 'aikoql:document' ? 24 : Math.min(16, 5 + (n.edge_count || 0) / 5)) : (n.size||20), borderWidth: superseded?2.5:(n.tenant?2.5:(n.edge_count>2?3:1.5)),
        font: {size:Math.max(9,Math.min(13,(n.size||20)/2+3))},
      };
    });
    graphNodes.add(nodes);
    const edges = (data.edges||[]).map(e => ({from:e.source,to:e.target,label:((data.edges||[]).length > 150 ? undefined : e.rel_type),arrows:'to',color:{color:cssVar('--border-strong'),highlight:cssVar('--accent')},font:{size:9,color:cssVar('--muted')},width:((data.edges||[]).length > 150 ? 0.5 : 1.2)}));
    // S-2 (POC-4): supersede-with-retract (B3-3) leaves no SUPERSEDES edge —
    // derive virtual "retracts" edges: the superseded row's successor shares
    // tenant + case_id + kind and asserts valid_from == the predecessor's
    // valid_to_asserted (the corpus's re-assertion convention).
    const retracted = data.nodes.filter(n => n.extensions && n.extensions.epistemic_status==='superseded' && n.extensions.valid_to_asserted != null);
    retracted.forEach(p => {
      data.nodes.forEach(s => {
        if (s.koid===p.koid) return;
        if ((s.tenant||'')!==(p.tenant||'')) return;
        const pc=p.properties&&p.properties.case_id, sc=s.properties&&s.properties.case_id;
        if (pc===undefined||sc===undefined||JSON.stringify(pc)!==JSON.stringify(sc)) return;
        const pk=p.properties&&p.properties.kind, sk=s.properties&&s.properties.kind;
        if (pk===undefined||sk===undefined||JSON.stringify(pk)!==JSON.stringify(sk)) return;
        if (!s.extensions || s.extensions.valid_from!==p.extensions.valid_to_asserted) return;
        edges.push({from:p.koid,to:s.koid,label:'retracts',arrows:'to',dashes:[4,3],color:{color:cssVar('--orange'),highlight:cssVar('--yellow')},font:{size:9,color:cssVar('--orange')},width:1});
      });
    });
    graphEdges.add(edges);
    // Legend
    let lh = '';
    for (const [t,c] of Object.entries(typeColors)) { const cnt = data.nodes.filter(n=>n.type_name===t).length; lh += '<div class="legend-item"><div class="legend-dot" style="background:'+esc(c)+'"></div>'+esc(t)+' ('+esc(cnt)+')</div>'; }
    document.getElementById('legend').innerHTML = lh||'<div style="color:var(--muted)">No types</div>';
    document.getElementById('status-center').textContent = data.nodes.length+' objects, '+edges.length+' relationships';
    renderGraphBanner(data);
    setTimeout(() => { if(graphNetwork&&nodes.length>0) graphNetwork.fit({animation:{duration:400}}); }, 200);
  } catch(e) { document.getElementById('status-center').textContent = 'Error: '+e.message; }
}
function showGraphTooltip(nodeId) {
  if (!graphRawData) return;
  const node = graphRawData.nodes.find(n => n.koid === nodeId);
  if (!node) return;
  let tt = document.getElementById('graph-tooltip');
  if (!tt) { tt = document.createElement('div'); tt.id = 'graph-tooltip'; tt.style.cssText = 'position:absolute;top:8px;right:8px;background:rgba(19,19,31,0.96);border:1px solid var(--border);border-radius:6px;padding:8px 12px;font-size:11px;max-width:260px;z-index:10;'; document.getElementById('graph-area').appendChild(tt); }
  tt.style.display = 'block';
  let h = '<div style="color:var(--accent);font-weight:600;">'+esc(node.type_name)+'</div>';
  if (node.tenant) h += '<div style="color:var(--purple);font-size:10px;">@'+esc(node.tenant)+'</div>';
  if (node.key_props) node.key_props.slice(0,5).forEach(p => h += '<div style="color:var(--muted);">'+esc(p.key)+': '+esc(JSON.stringify(p.value))+'</div>');
  // S-1 (POC-4): surface the epistemic status and its last transition —
  // lifecycle.state stays "draft" on retracted rows.
  const es = node.extensions&&node.extensions.epistemic_status;
  if (es) h += '<div style="color:'+esc(es==='superseded'?'#c9a86a':'var(--muted)')+';">epistemic: '+esc(es)+'</div>';
  const hist = node.extensions&&node.extensions.epistemic_history;
  if (hist && hist.length) {
    const last = hist[hist.length-1];
    h += '<div style="color:var(--muted);">'+esc(last.from)+' → '+esc(last.to)+esc(last.reason?' ('+last.reason+')':'')+' by '+esc(last.by||'?')+'</div>';
  }
  tt.innerHTML = h;
}
async function inspectNode(koid) {
  try {
    const data = await api('/api/graph?koid='+encodeURIComponent(koid)+'&detail=1');
    const node = data.nodes.find(n => n.koid === koid);
    if (!node) return;
    let h = renderNodeDetail(node);
    // Show in a floating card inside the graph panel
    let detail = document.getElementById('graph-detail-card');
    if (!detail) { detail = document.createElement('div'); detail.id = 'graph-detail-card'; detail.style.cssText = 'position:absolute;top:40px;right:8px;width:340px;max-height:70vh;overflow-y:auto;background:rgba(19,19,31,0.97);border:1px solid var(--border);border-radius:8px;padding:12px 16px;font-size:11px;z-index:10;'; document.getElementById('graph-area').appendChild(detail); }
    detail.innerHTML = '<div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:6px;"><span style="font-weight:600;color:var(--accent);">'+esc(node.type_name)+'</span><button class="btn btn-sec btn-sm" data-click="closeDetail">✕</button></div>' + h;
  } catch(e) {}
}

// ═══════════════════════════════════════════
// Panel: Knowledge Explorer
// ═══════════════════════════════════════════
async function loadExplorerTree() {
  try {
    const data = await api('/api/schema');
    explorerData = data;
    renderExplorerTree();
  } catch(e) { document.getElementById('explorer-tree-content').innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}
function renderExplorerTree() {
  if (!explorerData || !explorerData.schema) return;
  const filter = (document.getElementById('explorer-filter').value || '').toLowerCase();
  const types = Object.entries(explorerData.schema).filter(([t]) => !filter || t.toLowerCase().includes(filter));
  let html = '';
  types.forEach(([typeName, info]) => {
    const count = info.count||0;
    if (count === 0) return; // skip empty types — prevents "No objects" dead-end click
    const tenantList = (info.tenants||[]).join(', ');
    html += '<div class="tree-item type-node" data-type="'+esc(typeName)+'" data-click="exploreType">';
    html += '<span style="color:'+esc(colorFor(typeName))+'">●</span> '+esc(typeName);
    html += '<span class="count">'+esc(count)+'</span>';
    html += '</div>';
    if (info.tenants && info.tenants.length > 0) {
      info.tenants.forEach(t => {
        html += '<div class="tree-item" style="padding-left:32px;font-size:10px;" data-type="'+esc(typeName)+'" data-tenant="'+esc(t)+'" data-click="exploreTypeTenant">@'+esc(t)+'</div>';
      });
    }
  });
  document.getElementById('explorer-tree-content').innerHTML = html || '<div class="empty-state">No types found.</div>';
}
async function exploreType(typeName) {
  document.getElementById('explorer-detail').innerHTML = '<div class="loading">Loading '+esc(typeName)+'...</div>';
  try {
    const data = await api('/api/graph?type=' + encodeURIComponent(typeName));
    if (!data.nodes || data.nodes.length === 0) {
      document.getElementById('explorer-detail').innerHTML = '<div class="empty-state">No objects of type '+esc(typeName)+' found.</div>';
      return;
    }
    let html = '<div class="card"><h3>'+esc(typeName)+' ('+esc(data.nodes.length)+' objects)</h3>';
    html += '<table><thead><tr><th>KOID</th><th>Label</th><th>Tenant</th><th>Version</th><th>Edges</th></tr></thead><tbody>';
    data.nodes.forEach(n => {
      html += '<tr>';
      html += '<td class="mono"><a href="#" data-koid="'+esc(n.koid)+'" data-click="openInInspector" style="color:var(--accent);">'+esc(n.koid.substring(0,20))+'...</a></td>';
      html += '<td>'+esc(n.label||n.type_name)+'</td>';
      if (n.tenant) html += '<td><span class="badge badge-purple">@'+esc(n.tenant)+'</span></td>'; else html += '<td>―</td>';
      html += '<td>'+esc(n.version||'?')+'</td>';
      html += '<td>'+esc(n.edge_count||0)+'</td>';
      html += '</tr>';
    });
    html += '</tbody></table></div>';
    document.getElementById('explorer-detail').innerHTML = html;
    // Highlight selected
    document.querySelectorAll('#explorer-tree-content .tree-item').forEach(el => el.classList.remove('selected'));
    const sel = document.querySelector('#explorer-tree-content .tree-item[data-type="'+typeName+'"]');
    if (sel) sel.classList.add('selected');
  } catch(e) { document.getElementById('explorer-detail').innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}
async function exploreTypeTenant(typeName, tenant) {
  document.getElementById('explorer-detail').innerHTML = '<div class="loading">Loading '+esc(typeName)+' @'+esc(tenant)+'...</div>';
  try {
    const data = await api('/api/graph?type='+encodeURIComponent(typeName)+'&tenant='+encodeURIComponent(tenant));
    let html = '<div class="card"><h3>'+esc(typeName)+' <span class="badge badge-purple">@'+esc(tenant)+'</span> ('+esc((data.nodes||[]).length)+' objects)</h3>';
    if (data.nodes && data.nodes.length > 0) {
      html += '<table><thead><tr><th>KOID</th><th>Label</th><th>Version</th><th>Edges</th></tr></thead><tbody>';
      data.nodes.forEach(n => html += '<tr><td class="mono" style="font-size:10px;">'+esc(n.koid)+'</td><td>'+esc(n.label||'')+'</td><td>'+esc(n.version||'?')+'</td><td>'+esc(n.edge_count||0)+'</td></tr>');
      html += '</tbody></table>';
    }
    html += '</div>';
    document.getElementById('explorer-detail').innerHTML = html;
  } catch(e) { document.getElementById('explorer-detail').innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

// ═══════════════════════════════════════════
// Panel: Schema Explorer
// ═══════════════════════════════════════════
async function loadSchemaPanel() {
  try {
    const data = await api('/api/schema');
    if (!data.schema) { document.getElementById('schema-content').innerHTML = '<div class="empty-state">No schema data available.</div>'; return; }
    let html = '<div style="margin-bottom:12px;font-size:12px;color:var(--accent);">'+esc(Object.keys(data.schema).length)+' types</div>';
    for (const [typeName, info] of Object.entries(data.schema)) {
      html += '<div class="card">';
      html += '<h3 style="display:flex;justify-content:space-between;align-items:center;">';
      html += '<span><span style="color:'+esc(colorFor(typeName))+'">●</span> '+esc(typeName)+'</span>';
      html += '<span class="badge badge-cyan">'+esc(info.count||0)+' objects</span>';
      html += '</h3>';
      if (info.tenants && info.tenants.length > 0) html += '<div style="margin-bottom:6px;">'+info.tenants.map(t => '<span class="badge badge-purple">@'+esc(t)+'</span>').join(' ')+'</div>';
      if (info.properties && info.properties.length > 0) {
        html += '<div style="font-size:11px;"><span style="color:var(--muted);">Properties: </span>';
        html += info.properties.map(p => '<code style="background:var(--bg);padding:1px 6px;border-radius:3px;font-size:10px;color:var(--green);">'+esc(p)+'</code>').join(' ');
        html += '</div>';
      } else { html += '<div style="font-size:11px;color:var(--muted);">No properties discovered yet.</div>'; }
      if (info.relationship_types && info.relationship_types.length > 0) {
        html += '<div style="font-size:11px;margin-top:4px;"><span style="color:var(--muted);">Relationships: </span>';
        html += info.relationship_types.map(r => '<span class="badge badge-orange">'+esc(r)+'</span>').join(' ');
        html += '</div>';
      }
      html += '</div>';
    }
    document.getElementById('schema-content').innerHTML = html || '<div class="empty-state">No types found. Create KOs to populate the schema.</div>';
  } catch(e) { document.getElementById('schema-content').innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

// ═══════════════════════════════════════════
// Shared: render node detail (used by Inspector + Graph)
// ═══════════════════════════════════════════
function renderNodeDetail(node) {
  let h = '';
  // Header badges
  if (node.tenant) h += '<span class="badge badge-purple">@'+esc(node.tenant)+'</span> ';
  if (node.lifecycle) {
    const st = typeof node.lifecycle === 'object' ? node.lifecycle.state : node.lifecycle;
    const cls = st==='active'?'badge-green':st==='archived'?'badge-red':st==='draft'?'badge-orange':'badge-cyan';
    h += '<span class="badge '+esc(cls)+'">'+esc(st)+'</span> ';
  }
  // S-1 (POC-4): the lifecycle badge alone is misleading on retracted rows
  // ("draft") — show the epistemic status next to it.
  if (node.extensions && node.extensions.epistemic_status) {
    const es = node.extensions.epistemic_status;
    h += '<span class="badge '+esc(es==='superseded'?'badge-orange':'badge-muted')+'">'+esc(es)+'</span> ';
  }
  if (node.security && node.security.classification) {
    h += '<span class="badge badge-red">'+esc(node.security.classification)+'</span> ';
  }
  h += '<div style="margin-top:6px;"></div>';

  // Identity
  h += '<div class="section-title">Identity</div>';
  h += '<div class="kv-row"><span class="kv-key">KOID</span><span class="kv-val mono" style="font-size:10px;">'+esc(node.koid)+'</span></div>';
  h += '<div class="kv-row"><span class="kv-key">Type</span><span class="kv-val">'+esc(node.type_name)+'</span></div>';
  h += '<div class="kv-row"><span class="kv-key">Version</span><span class="kv-val">'+esc(node.version||'?')+' (schema v'+esc(node.schema_version||1)+')</span></div>';

  // Lifecycle
  if (node.lifecycle) {
    h += '<div class="section-title">Lifecycle</div>';
    if (typeof node.lifecycle === 'object') {
      h += '<div class="kv-row"><span class="kv-key">State</span><span class="kv-val">'+esc(node.lifecycle.state)+'</span></div>';
      if (node.lifecycle.origin) h += '<div class="kv-row"><span class="kv-key">Origin</span><span class="kv-val">'+esc(node.lifecycle.origin)+'</span></div>';
    }
    h += '<div class="kv-row"><span class="kv-key">Edges</span><span class="kv-val">'+esc(node.edge_count||0)+'</span></div>';
  }

  // Security
  if (node.security) {
    h += '<div class="section-title">Security</div>';
    h += '<div class="kv-row"><span class="kv-key">Owner</span><span class="kv-val">'+esc(node.security.owner||'?')+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Classification</span><span class="kv-val">'+esc(node.security.classification||'none')+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">ACL Entries</span><span class="kv-val">'+esc(node.security.acl_count||0)+'</span></div>';
  }

  // Semantic (embedding enrichment)
  if (node.semantic) {
    h += '<div class="section-title">Semantic</div>';
    if (node.semantic.embedding_model) h += '<div class="kv-row"><span class="kv-key">Embedding Model</span><span class="kv-val">'+esc(node.semantic.embedding_model)+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Embedding Dims</span><span class="kv-val">'+esc(node.semantic.embedding_dims||0)+'</span></div>';
    if (node.semantic.summary) h += '<div class="kv-row"><span class="kv-key">Summary</span><span class="kv-val">'+esc(node.semantic.summary)+'</span></div>';
  }

  // Properties
  if (node.properties && Object.keys(node.properties).length > 0) {
    h += '<div class="section-title">Properties</div>';
    for (const [k,v] of Object.entries(node.properties)) {
      h += '<div class="kv-row"><span class="kv-key">'+esc(k)+'</span><span class="kv-val">'+esc(JSON.stringify(v))+'</span></div>';
    }
  }

  // Relationships
  if (node.relationships && node.relationships.length > 0) {
    h += '<div class="section-title">Relationships ('+esc(node.relationships.length)+')</div>';
    node.relationships.forEach(r => {
      h += '<div class="kv-row"><span class="kv-key">'+esc(r.direction)+'</span><span class="kv-val"><span class="badge badge-orange">'+esc(r.type)+'</span> → <span class="mono" style="font-size:10px;">'+esc((r.target||'').substring(0,20))+'...</span></span></div>';
    });
  }

  // Extensions
  if (node.extensions && Object.keys(node.extensions).length > 0) {
    h += '<div class="section-title">Extensions</div>';
    for (const [k,v] of Object.entries(node.extensions)) {
      h += '<div class="kv-row"><span class="kv-key">'+esc(k)+'</span><span class="kv-val">'+esc(JSON.stringify(v))+'</span></div>';
    }
  }

  // Events & Audit
  if (node.event_refs) h += '<div class="section-title">Audit</div><div class="kv-row"><span class="kv-key">Events</span><span class="kv-val">'+esc(node.event_refs)+' journal references</span></div>';

  return h;
}

// ═══════════════════════════════════════════
// Panel: KO Inspector
// ═══════════════════════════════════════════
let inspectorSearchTimeout = null;
async function inspectorSearch() {
  clearTimeout(inspectorSearchTimeout);
  inspectorSearchTimeout = setTimeout(async () => {
    const q = document.getElementById('inspector-search-input').value.trim();
    const sug = document.getElementById('inspector-suggestions');
    if (!q) { sug.innerHTML = '<div class="empty-state">Enter KOID or type name to search.</div>'; return; }
    // If it looks like a KOID (long hex), fetch directly
    if (/^[0-9a-fA-F]{16,}$/.test(q)) {
      sug.innerHTML = '<div class="loading">Loading...</div>';
      try {
        const data = await api('/api/graph?koid='+encodeURIComponent(q)+'&detail=1');
        const node = data.nodes.find(n => n.koid === q);
        if (node) {
          document.getElementById('inspector-detail').innerHTML = renderNodeDetail(node);
          sug.innerHTML = '<div style="font-size:10px;color:var(--green);padding:6px 10px;">✓ Found</div>';
        } else { sug.innerHTML = '<div class="empty-state">Not found.</div>'; }
      } catch(e) { sug.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
      return;
    }
    // Search by type name
    sug.innerHTML = '<div class="loading">Searching...</div>';
    try {
      const data = await api('/api/graph?type='+encodeURIComponent(q));
      if (data.nodes && data.nodes.length > 0) {
        let html = '';
        data.nodes.slice(0, 30).forEach(n => {
          html += '<div class="suggest-item" data-koid="'+esc(n.koid)+'" data-click="inspectorLoadKOID">';
          html += '<span><span style="color:'+esc(colorFor(n.type_name))+'">●</span> '+esc(n.type_name)+'</span>';
          html += '<span class="mono" style="font-size:10px;color:var(--muted);">'+esc(n.koid.substring(0,16))+'...</span>';
          html += '</div>';
        });
        if (data.nodes.length > 30) html += '<div style="font-size:10px;color:var(--muted);padding:4px 10px;">+ '+esc(data.nodes.length-30)+' more (narrow your search)</div>';
        sug.innerHTML = html;
      } else { sug.innerHTML = '<div class="empty-state">Nothing matches "'+esc(q)+'".</div>'; }
    } catch(e) { sug.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
  }, 300);
}
async function inspectorLoadKOID(koid) {
  document.getElementById('inspector-detail').innerHTML = '<div class="loading">Loading...</div>';
  document.getElementById('inspector-search-input').value = koid;
  try {
    const data = await api('/api/graph?koid='+encodeURIComponent(koid)+'&detail=1');
    const node = data.nodes.find(n => n.koid === koid);
    if (node) {
      document.getElementById('inspector-detail').innerHTML = renderNodeDetail(node);
      document.getElementById('inspector-suggestions').innerHTML = '<div style="font-size:10px;color:var(--green);padding:6px 10px;">✓ '+esc(node.type_name)+'</div>';
    } else { document.getElementById('inspector-detail').innerHTML = '<div class="empty-state">Object not found.</div>'; }
  } catch(e) { document.getElementById('inspector-detail').innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

// ═══════════════════════════════════════════
// Panel: Ontology
// ═══════════════════════════════════════════
// The ontology the compiler uses (GET /api/v1/ontology) — never an arbitrary
// row. Its version and source are kept for saveOntology's CAS.
let ontologyVersion = null, ontologySource = null;
// fill = true also loads the active definition into the editor (empty when the
// scope has none): after a tenant switch or a save, never leave another
// scope's text there to be saved over this one.
async function loadOntologyPanel(fill) {
  const editor = document.getElementById('ontology-yaml-input');
  try {
    const tenant = document.getElementById('ontology-tenant').value;
    const raw = await api('/api/v1/ontology' + (tenant ? '?tenant=' + encodeURIComponent(tenant) : ''));
    const data = {results: (raw.data || raw).properties ? [raw.data || raw] : []};
    const statusDiv = document.getElementById('ontology-status');
    const classesDiv = document.getElementById('ontology-classes');
    const relsDiv = document.getElementById('ontology-relationships');
    const mapsDiv = document.getElementById('ontology-mappings');

    if (!data.results || data.results.length === 0) {
      ontologyVersion = null; ontologySource = null;
      if (fill) editor.value = '';
      statusDiv.innerHTML = '<div class="empty-state">No ontology defined yet. Use the form below to create one.</div>';
      classesDiv.innerHTML = ''; relsDiv.innerHTML = ''; mapsDiv.innerHTML = '';
      fillReconcileClasses([]);
      return;
    }
    const ko = data.results[0];
    ontologyVersion = ko.version; ontologySource = ko.source;
    if (fill) editor.value = JSON.stringify(ko.properties || {}, null, 2);
    window.__ontologyClasses = ((ko.properties||{}).classes||[]).map(c => c && c.name).filter(Boolean);
    fillReconcileClasses((ko.properties || {}).mappings || []);
    const props = ko.properties || {};
    statusDiv.innerHTML = '<div style="color:var(--green);margin-bottom:8px;">✓ Ontology loaded — <b>'+ esc(props.namespace||'?') +'</b> v'+ esc(props.version||'?') +' <span style="color:var(--muted);">('+ esc(ko.source||'?') +', revision '+ esc(ko.version) +')</span></div>';

    // Render classes
    if (props.classes && Array.isArray(props.classes)) {
      let h = '<h3>Classes ('+esc(props.classes.length)+')</h3><div class="ontology-grid">';
      props.classes.forEach(c => {
        const cm = typeof c === 'object' ? c : {};
        const mine = (props.mappings || []).filter(m => m && m.class === cm.name);
        h += '<div class="card" style="padding:8px 12px;"><span style="color:var(--accent);font-weight:600;">'+ esc(cm.name||'?') +'</span>';
        mine.forEach(m => {
          h += '<div style="font-size:10px;margin-top:3px;"><span class="mono">'+esc(m.source)+'.'+esc(m.physical_type)+'</span> ';
          if ((m.identity || []).length) h += '<span class="badge badge-green">key: '+esc(m.identity.join(', '))+'</span>';
          else h += '<span class="badge badge-orange">no key</span>';
          h += '</div>';
        });
        if (cm.parent) h += ' <span style="color:var(--muted);">extends</span> <span style="color:var(--green);">'+esc(cm.parent)+'</span>';
        if (cm.description) h += '<div style="font-size:10px;color:var(--muted);">'+esc(cm.description)+'</div>';
        h += '</div>';
      });
      h += '</div>';
      classesDiv.innerHTML = h;
    }

    // Render relationships
    if (props.relationships && Array.isArray(props.relationships)) {
      let h = '<h3>Relationships ('+esc(props.relationships.length)+')</h3><table><thead><tr><th>Name</th><th>Domain</th><th>Range</th><th>Card.</th></tr></thead><tbody>';
      props.relationships.forEach(r => {
        h += '<tr><td style="color:var(--accent);">'+ esc(r.name||'?') +'</td><td>'+ esc(r.domain||'*') +'</td><td>'+ esc(r.range||'*') +'</td><td><span class="badge badge-purple">'+ esc(r.cardinality||'?') +'</span></td></tr>';
      });
      h += '</tbody></table>';
      relsDiv.innerHTML = h;
    }

    // Render mappings
    if (props.mappings && Array.isArray(props.mappings)) {
      let h = '<h3>Mappings ('+esc(props.mappings.length)+')</h3><table><thead><tr><th>Source</th><th>Physical Type</th><th>Class</th><th>Key</th><th>Links</th><th>Authority</th><th>Property Map</th></tr></thead><tbody>';
      props.mappings.forEach(m => {
        const pm = typeof m.property_map === 'object' ? m.property_map : {};
        const pmStr = Object.entries(pm).map(([k,v]) => k+' → '+v).join(', ');
        h += '<tr><td><span class="badge badge-cyan">'+ esc(m.source||'?') +'</span></td><td class="mono" style="font-size:10px;">'+ esc(m.physical_type||'?') +'</td><td>'+ esc(m.class||'?') +'</td>'
          + '<td class="mono" style="font-size:10px;">'+esc((m.identity || []).join(', ') || '—')+'</td>'
          + '<td style="font-size:10px;">'+esc((m.links || []).map(l => l.name + ' → ' + l.target_type + '.' + l.target_column).join('; ') || '—')+'</td>'
          + '<td style="font-size:10px;">'+esc((m.asserts || []).map(a => a.class + ' (rank ' + a.rank + ')').join('; ') || '—')+'</td>'
          + '<td style="font-size:10px;color:var(--muted);">'+esc(pmStr)+'</td></tr>';
      });
      h += '</tbody></table>';
      mapsDiv.innerHTML = h;
    }
  } catch(e) {
    document.getElementById('ontology-status').innerHTML = '<div class="error-text">'+esc(e.message)+'</div>';
  }
}

// The classes some mapping asserts properties of: the ones that can be reconciled.
function fillReconcileClasses(mappings) {
  const sel = document.getElementById('reconcile-class');
  const list = Array.isArray(mappings) ? mappings : [];
  const names = [...new Set(list.flatMap(m => (m && Array.isArray(m.asserts) ? m.asserts : []).map(a => a && a.class).filter(Boolean)))];
  sel.innerHTML = names.length ? names.map(n => '<option value="'+esc(n)+'">'+esc(n)+'</option>').join('') : '<option value="">(no authority declared yet)</option>';
  document.getElementById('reconcile-run').disabled = !names.length;
}
// Resolve one class, then show what the sources disagree about and who wins.
async function resolveClass() {
  const cls = document.getElementById('reconcile-class').value;
  const out = document.getElementById('reconcile-result');
  if (!cls) { out.innerHTML = '<div class="empty-state">Declare authority first: Data Sources → Preview → Authority.</div>'; return; }
  const tenant = document.getElementById('ontology-tenant').value;
  if (/["\\]/.test(cls)) { out.innerHTML = '<div class="error-text">Class names with quotes or backslashes cannot be reconciled from Studio.</div>'; return; }
  out.innerHTML = '<div class="loading">Resolving...</div>';
  try {
    const body = tenant ? {class: cls, tenant: tenant} : {class: cls};
    const r = (await apiPost('/api/v1/sources/resolve', body)).data;
    let url = '/api/aikoql?query=' + encodeURIComponent('MATCH aikoql:contradiction WHERE class == "' + cls + '" RETURN *');
    if (tenant) url += '&tenant=' + encodeURIComponent(tenant);
    const found = await api(url);
    const list = found.results || [];
    let h = '<div style="font-size:12px;margin:8px 0;"><b>'+esc(r.entities)+'</b> entities from <b>'+esc(r.claims)+'</b> claims · '
      + '<b>'+esc(r.contradictions.resolved)+'</b> disagreements settled by authority, <b>'+esc(r.contradictions.open)+'</b> open';
    if (r.ambiguous) h += ' · '+esc(r.ambiguous)+' key(s) skipped because a source gave them several rows';
    if (r.no_key) h += ' · '+esc(r.no_key)+' row(s) without a key';
    h += '</div>';
    (r.proposals || []).forEach(pr => {
      h += '<div style="font-size:11px;color:var(--orange);">Possibly the same: '+esc(pr.keys.join(' ≈ '))+' ('+esc(pr.rule)+', '+esc(pct(pr.confidence))+') — not merged.</div>';
    });
    if (list.length) {
      h += '<table><thead><tr><th>Entity</th><th>Property</th><th>Sources say</th><th>Decision</th><th></th></tr></thead><tbody>';
      list.forEach(row => {
        const c = row.properties || {};
        const says = (c.values || []).map(v => v.source + ': ' + v.value).join(' | ');
        h += '<tr><td class="mono">'+esc(c.key)+'</td><td>'+esc(c.property)+'</td><td style="font-size:11px;">'+esc(says)+'</td>'
          + '<td><span class="badge '+esc(c.status === 'resolved' ? 'badge-green' : 'badge-orange')+'">'+esc(c.status)+'</span> <span style="font-size:10px;color:var(--muted);">'+esc(c.winner ? c.winner + ' — ' : '')+esc(c.rule)+'</span></td>'
          + '<td><button class="btn btn-sec btn-sm" data-click="openInInspector" data-koid="'+esc(row.koid)+'">Inspect</button></td></tr>';
      });
      h += '</tbody></table>';
    } else {
      h += '<div class="empty-state">The sources agree about every property they both assert.</div>';
    }
    out.innerHTML = h;
  } catch (e) {
    out.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>';
  }
}

// Discovery is a proposal: the first click previews (nothing is written), the
// Save button it offers calls again with save=true.
async function discoverOntology(save) {
  const msg = document.getElementById('ontology-save-msg');
  msg.innerHTML = '<span style="color:var(--muted);">Discovering from stored KOs...</span>';
  try {
    const data = await apiPost('/api/v1/discover-ontology', {preview: !save});
    const d = data.data || data;
    const html = esc(d.classes)+' classes, '+esc(d.relationships)+' relationships, '+esc(d.mappings)+' mappings from '+esc((d.types_discovered||[]).length)+' types';
    if (save) {
      msg.innerHTML = '<span style="color:var(--green);">✓ Saved: '+html+esc(d.changed===false?' (unchanged)':'')+'.</span>';
      loadOntologyPanel(true);
    } else {
      msg.innerHTML = '<span style="color:var(--accent);">Proposal: '+html+' — not saved yet.</span> <button class="btn btn-pri btn-sm" data-click="discoverOntologySave">Save discovered ontology</button>';
    }
  } catch(e) { msg.innerHTML = '<span style="color:var(--red);">'+esc(e.message)+'</span>'; }
}

async function saveOntology() {
  const msg = document.getElementById('ontology-save-msg');
  const tenant = document.getElementById('ontology-tenant').value;
  msg.innerHTML = '<span style="color:var(--muted);">Saving...</span>';
  try {
    const raw = document.getElementById('ontology-yaml-input').value.trim();
    if (!raw) { msg.innerHTML = '<span style="color:var(--red);">Please paste an ontology definition.</span>'; return; }
    const def = JSON.parse(raw);
    if (!def.namespace) { msg.innerHTML = '<span style="color:var(--red);">Missing "namespace" field.</span>'; return; }
    if (!def.classes) { msg.innerHTML = '<span style="color:var(--red);">Missing "classes" array.</span>'; return; }
    // Saved in place under a CAS on the revision this editor loaded; the server
    // validates the definition and answers with the new revision. The
    // identity comes from the bearer token, not from this body.
    const payload = {definition: def};
    if (tenant) payload.tenant = tenant;
    if (ontologySource === 'manual') payload.expected_version = ontologyVersion;
    const r = await apiPost('/api/v1/ontology', payload);
    const d = r.data || r;
    msg.innerHTML = '<span style="color:var(--green);">✓ Ontology saved (revision '+esc(d.version)+').</span>';
    loadOntologyPanel(true);
  } catch(e) {
    msg.innerHTML = '<span style="color:var(--red);">'+esc(e.message)+' — reload the panel if the ontology changed since you opened it.</span>';
  }
}

// ═══════════════════════════════════════════
// Panel: Administration
// ═══════════════════════════════════════════
// The last backup/verify/restore result, re-shown after the panel reloads itself.
let adminNote = '';
function adminMsg(html) { adminNote = html; document.getElementById('admin-backup-result').innerHTML = html; }
async function loadAdminPanel() {
  try {
    const [health, metrics, backups, compliance] = await Promise.all([
      api('/health').catch(() => ({status:'offline'})),
      api('/api/v1/metrics-info').then(r => r.data).catch(() => ({})),
      api('/api/v1/backups').then(r => r.data).catch(() => ({backups:[]})),
      api('/api/v1/compliance').then(r => r.data).catch(() => null)
    ]);
    let html = '';
    // Health card
    html += '<div class="card"><h3>System Health</h3>';
    html += '<div class="metric-big">'+esc(health.status||'?')+'</div>';
    html += '<div class="metric-label">uptime '+esc((health.uptime_seconds||0).toFixed(0))+'s</div>';
    html += '<div style="margin-top:8px;font-size:11px;color:var(--muted);">Journal seq: '+esc(metrics.journal_seq||'?')+' · Objects: '+esc(metrics.total_objects||'?')+' · Types: '+esc(Object.keys(metrics.by_type||{}).length||'?')+'</div>';
    html += '</div>';
    // Metrics card
    html += '<div class="card"><h3>Database Metrics</h3>';
    html += '<div class="kv-row"><span class="kv-key">Objects</span><span class="kv-val">'+esc(metrics.total_objects||0)+'</span></div>';
    html += '<div class="kv-row"><span class="kv-key">Active</span><span class="kv-val">'+esc(metrics.active_objects||0)+'</span></div>';
    html += '<div class="kv-row"><span class="kv-key">Journal Seq</span><span class="kv-val mono">'+esc(metrics.journal_seq||0)+'</span></div>';
    html += '<div class="kv-row"><span class="kv-key">Uptime</span><span class="kv-val">'+esc((metrics.uptime_seconds||0).toFixed(0))+'s</span></div>';
    if (metrics.by_type) {
      html += '<div style="margin-top:6px;font-size:11px;">';
      for (const [t,c] of Object.entries(metrics.by_type)) html += '<span class="badge badge-cyan" style="margin:1px;">'+esc(t)+': '+esc(c)+'</span> ';
      html += '</div>';
    }
    html += '</div>';
    // Compliance card (new)
    if (compliance) {
      const cs = compliance.field_crypto_summary || {};
      html += '<div class="card"><h3>Encryption</h3>';
      html += '<div class="kv-row"><span class="kv-key">Enabled</span><span class="kv-val">'+esc(compliance.encryption_enabled?'✅ Yes':'⬜ No')+'</span></div>';
      html += '<div class="kv-row"><span class="kv-key">Policies</span><span class="kv-val">'+esc(compliance.policies_registered||0)+'</span></div>';
      html += '<div class="kv-row"><span class="kv-key">Tenant Keys</span><span class="kv-val">'+esc(cs.tenant_keys||0)+'</span></div>';
      html += '<div style="font-size:10px;color:var(--muted);margin-top:4px;">Policy types: '+esc((compliance.policy_types||[]).join(', ')||'none')+'</div>';
      html += '</div>';
    }
    // Backups card (upgraded: actions + correct columns)
    html += '<div class="card"><h3>Backups & Recovery</h3>';
    html += '<div style="display:flex;gap:8px;margin-bottom:10px;">';
    html += '<button class="btn btn-pri btn-sm" data-click="adminCreateBackup">📦 Create Backup</button>';
    html += '</div>';
    html += '<div id="admin-backup-result" style="margin-bottom:8px;"></div>';
    const blist = backups.backups || [];
    if (blist.length > 0) {
      html += '<table><thead><tr><th>Name</th><th>Objects</th><th>Journal</th><th>Actions</th></tr></thead><tbody>';
      blist.forEach(b => {
        const meta = b.meta || {};
        html += '<tr><td class="mono" style="font-size:10px;">'+esc(b.name)+'</td><td>'+esc(meta.object_count||'?')+'</td><td>'+esc(meta.journal_seq||'?')+'</td>';
        html += '<td>';
        html += '<button class="btn btn-sec btn-sm" data-name="'+esc(b.name)+'" data-click="adminVerifyBackup" style="margin-right:4px;">Verify</button>';
        html += '<button class="btn btn-sec btn-sm" data-name="'+esc(b.name)+'" data-click="adminRestoreBackup">Restore</button>';
        html += '</td></tr>';
      });
      html += '</tbody></table>';
    } else { html += '<div style="color:var(--muted);font-size:11px;">No backups found.</div>'; }
    html += '</div>';
    // Audit card
    html += '<div class="card"><h3>Audit</h3>';
    html += '<div style="font-size:11px;color:var(--muted);margin-bottom:8px;">SHA-256 audit chain. Every mutation is cryptographically verifiable.</div>';
    html += '<button class="btn btn-sec btn-sm" data-click="loadAuditReport">View Audit Report</button> ';
    html += '<button class="btn btn-sec btn-sm" data-click="loadComplianceReport">Compliance Detail</button>';
    html += '<div id="admin-audit-result" style="margin-top:8px;"></div>';
    html += '</div>';
    document.getElementById('admin-grid').innerHTML = html;
    document.getElementById('admin-backup-result').innerHTML = adminNote;
  } catch(e) { document.getElementById('admin-grid').innerHTML = '<div class="error-text">Failed to load admin dashboard: '+esc(e.message)+'</div>'; }
}

// ═══════════════════════════════════════════
// Panel: Users (enterprise; hidden without an admin session)
// ═══════════════════════════════════════════
async function loadUsers() {
  const panel = document.getElementById('users-panel');
  if (!panel) return;
  try {
    // community mode has no user admin — ask the licence first, don't 404
    const lic = await api('/api/license');
    if (!(lic.features || []).includes('multi-user')) { panel.style.display = 'none'; return; }
    const d = await api('/api/users');
    panel.style.display = 'block';
    renderUsers(d.users || []);
  } catch(e) { panel.style.display = 'none'; } // 403/404: no admin, or no license
}
// A vis-network tooltip as DOM text nodes (a title string may be parsed as HTML).
function nodeTip(n, es) {
  const d = document.createElement('div'), b = document.createElement('b');
  d.style.whiteSpace = 'pre';
  b.textContent = n.type_name;
  d.appendChild(b);
  d.append((n.tenant ? ' @' + n.tenant : '') + '\n' + n.koid + (es ? '\n' + es : ''));
  return d;
}
// Open a KO in the Inspector (a data-koid attribute feeds it — never splice a
// KOID into an inline handler's JS string).
function openInInspector(koid) {
  switchPanel('inspector');
  document.getElementById('inspector-search-input').value = koid;
  inspectorSearch();
  return false;
}
// Escape & < > " ' — rendered rows carry stored data and IdP-influenceable
// claims (sub/name in SSO mode), which must never reach the markup raw.
function esc(s) {
  if (s === null || s === undefined) return '';
  return String(s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
}
function renderUsers(users) {
  const el = document.getElementById('users-list');
  if (!users.length) { el.innerHTML = '<div class="empty-state">No users yet.</div>'; return; }
  let html = '<table style="width:100%;border-collapse:collapse;font-size:11px;"><tr style="color:var(--muted);text-align:left;"><th>sub</th><th>name</th><th>roles</th><th></th></tr>';
  users.forEach(u => {
    const sub = esc(u.sub), name = esc(u.name), roles = esc((u.roles||[]).join(', '));
    html += '<tr><td class="mono" style="padding:4px 0;">'+esc(sub)+'</td><td>'+esc(name)+'</td><td>'+esc(roles)+'</td><td><button class="btn btn-sec btn-sm" data-sub="'+esc(sub)+'" data-click="setUserRoles">Set roles</button></td></tr>';
  });
  html += '</table>';
  el.innerHTML = html;
}
async function createUser() {
  const sub = document.getElementById('user-sub').value.trim();
  const name = document.getElementById('user-name').value.trim();
  const roles = document.getElementById('user-roles').value.split(',').map(s => s.trim()).filter(Boolean);
  if (!sub) return;
  const body = {sub: sub, roles: roles};
  if (name) body.name = name;
  try {
    await apiPost('/api/users', body);
    document.getElementById('user-sub').value = '';
    document.getElementById('user-name').value = '';
    document.getElementById('users-msg').innerHTML = '<span style="color:var(--green);">User provisioned.</span>';
    loadUsers();
  } catch(e) { document.getElementById('users-msg').innerHTML = '<span style="color:var(--red);">'+esc(e.message)+'</span>'; }
}
async function setUserRoles(sub) {
  const roles = document.getElementById('user-roles').value.split(',').map(s => s.trim()).filter(Boolean);
  try {
    await apiPut('/api/users/' + encodeURIComponent(sub) + '/roles', {roles: roles});
    loadUsers();
  } catch(e) { document.getElementById('users-msg').innerHTML = '<span style="color:var(--red);">'+esc(e.message)+'</span>'; }
}
// fetch + Blob so the download authenticates with the header, not the URL
async function downloadAuditExport() {
  try {
    const res = await fetch('/api/audit-export', {headers: authHeaders()});
    if (!res.ok) throw new Error('HTTP ' + res.status);
    const m = /filename="?([^";]+)"?/.exec(res.headers.get('Content-Disposition') || '');
    const a = document.createElement('a');
    a.href = URL.createObjectURL(await res.blob());
    a.download = m ? m[1] : 'audit-export.jsonl';
    document.body.appendChild(a); a.click(); a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
  } catch(e) { alert('Audit export failed: ' + e.message); }
}
async function adminCreateBackup() {
  adminMsg('<span style="color:var(--muted);">Creating backup...</span>');
  try {
    const data = await apiPost('/api/v1/backup');
    const d = data.data || data;
    adminMsg('<span style="color:var(--green);">Backup created: '+esc(d.backup)+' ('+esc(d.object_count)+' objects, verified: '+esc(d.verified)+')</span>');
    setTimeout(() => loadAdminPanel(), 1500);
  } catch(e) { adminMsg('<span style="color:var(--red);">'+esc(e.message)+'</span>'); }
}
async function adminVerifyBackup(name) {
  adminMsg('<span style="color:var(--muted);">Verifying '+esc(name)+'...</span>');
  try {
    const data = await apiPost('/api/v1/verify-backup', {backup: name});
    const d = data.data || data;
    adminMsg('<span style="color:'+esc(d.verified?'var(--green)':'var(--red)')+';">Backup '+esc(name)+': '+esc(d.verified?'✅ Verified':'❌ Invalid')+' (seq '+esc(d.expected_journal_seq)+')</span>');
  } catch(e) { adminMsg('<span style="color:var(--red);">'+esc(e.message)+'</span>'); }
}
async function adminRestoreBackup(name) {
  if (!confirm('Restore backup "'+name+'"? This will replace the current database.')) return;
  adminMsg('<span style="color:var(--orange);">Restoring from '+esc(name)+'... server may restart.</span>');
  try {
    const data = await apiPost('/api/v1/restore', {backup: name});
    const d = data.data || data;
    adminMsg('<span style="color:var(--green);">Restored from '+esc(name)+'. Recovery point: seq '+esc(d.recovery_point?.journal_seq||'?')+'</span>');
    setTimeout(() => loadAdminPanel(), 2000);
  } catch(e) { adminMsg('<span style="color:var(--red);">'+esc(e.message)+'</span>'); }
}
async function loadAuditReport() {
  const div = document.getElementById('admin-audit-result');
  div.innerHTML = '<div class="loading">Loading...</div>';
  try {
    const data = await api('/api/v1/audit');
    div.innerHTML = '<div class="pre-box">'+esc(JSON.stringify(data, null, 2))+'</div>';
  } catch(e) { div.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}
async function loadComplianceReport() {
  const div = document.getElementById('admin-audit-result');
  div.innerHTML = '<div class="loading">Loading...</div>';
  try {
    const data = await api('/api/v1/compliance');
    div.innerHTML = '<div class="pre-box">'+esc(JSON.stringify(data, null, 2))+'</div>';
  } catch(e) { div.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

// ═══════════════════════════════════════════
// Timeline Panel (S2)
// ═══════════════════════════════════════════
async function loadTimeline() {
  const koid = document.getElementById('timeline-koid-input').value.trim();
  const div = document.getElementById('timeline-result');
  if (!koid) { div.innerHTML = '<div class="error-text">Enter a KOID.</div>'; return; }
  div.innerHTML = '<div class="loading">Loading version history...</div>';
  try {
    const trace = await api('/api/v1/trace/' + encodeURIComponent(koid));
    const data = trace.data || trace;
    // S-6: events[] carries kind/actor/note; versions[] only the version
    // skeleton. Join by version for the asserted event-time column.
    const events = data.events || data.versions || [];
    const vmap = {};
    (data.versions || []).forEach(v => { vmap[v.version] = v; });
    if (!events.length) { div.innerHTML = '<div class="empty-state">No versions found for this KOID.</div>'; return; }
    let h = '<div style="font-size:11px;color:var(--muted);margin-bottom:8px;">'+esc(events.length)+' versions</div>';
    h += '<div class="timeline-track">';
    events.forEach((e, i) => {
      const ver = e.version !== undefined ? 'v' + e.version : '';
      const vf = (vmap[e.version] || {}).valid_from;
      h += '<div class="timeline-dot" style="margin-bottom:8px;">';
      h += '<span class="badge badge-cyan">' + esc(ver) + '</span> ';
      h += '<span class="badge badge-purple">' + esc(e.kind || '') + '</span> ';
      h += '<span style="font-size:11px;">' + esc(fmtTime(e.commit_ts)) + '</span> ';
      if (vf) h += '<span style="font-size:10px;color:var(--muted);">event time: ' + esc(fmtTime(vf)) + '</span> ';
      h += '<span style="font-size:10px;color:var(--muted);">by ' + esc(e.actor || '?') + '</span>';
      h += '</div>';
    });
    h += '</div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

// ═══════════════════════════════════════════
// Provenance Panel (S2)
// ═══════════════════════════════════════════
async function loadProvenance() {
  const koid = document.getElementById('provenance-koid-input').value.trim();
  const div = document.getElementById('provenance-result');
  if (!koid) { div.innerHTML = '<div class="error-text">Enter a KOID.</div>'; return; }
  div.innerHTML = '<div class="loading">Tracing provenance chain...</div>';
  try {
    const trace = await api('/api/v1/trace/' + encodeURIComponent(koid));
    const data = trace.data || trace;
    // S-3: events[] carries kind/actor/note/audit_hash — versions[] only
    // the version skeleton, which is how every row rendered "Source: ?".
    const events = data.events || data.versions || [];
    if (!events.length) { div.innerHTML = '<div class="empty-state">No provenance data for this KOID.</div>'; return; }
    let h = '<div class="provenance-chain">';
    events.forEach((e, i) => {
      h += '<div class="provenance-node" style="border-left:2px solid var(--cyan);padding-left:12px;margin-bottom:12px;">';
      h += '<div style="font-size:12px;font-weight:600;">#' + esc(i + 1) + ' — v' + esc(e.version || '?') + ' <span class="badge badge-purple">' + esc(e.kind || '') + '</span></div>';
      h += '<div style="font-size:11px;color:var(--muted);">' + esc(fmtTime(e.commit_ts)) + '</div>';
      h += '<div style="font-size:10px;color:var(--muted);">Actor: ' + esc(e.actor || '?') + '</div>';
      if (e.note) h += '<div style="font-size:10px;color:var(--muted);">Note: ' + esc(e.note) + '</div>';
      if (e.audit_hash) h += '<div style="font-size:10px;font-family:var(--font-mono);color:var(--green);">SHA-256: ' + esc(e.audit_hash.substring(0, 16)) + '...</div>';
      h += '</div>';
    });
    h += '</div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

async function verifyProvenance() {
  const div = document.getElementById('provenance-result');
  const koid = document.getElementById('provenance-koid-input').value.trim();
  if (!koid) { div.innerHTML = '<div class="error-text">Enter a KOID first.</div>'; return; }
  div.innerHTML = '<div class="loading">Verifying audit chain...</div>';
  try {
    const proof = await api('/api/v1/prove?koid=' + encodeURIComponent(koid));
    const data = proof.data || proof;
    // The response carries {claim, events, chain_valid, head_audit_hash};
    // head_audit_hash is a byte array on the wire — hex-join it.
    const headHex = (data.head_audit_hash || []).map(b => b.toString(16).padStart(2,'0')).join('');
    const ok = data.chain_valid;
    const col = ok ? 'var(--green)' : 'var(--red)';
    div.innerHTML = '<div class="card" style="border-left:3px solid ' + esc(col) + ';padding:12px;">' +
      '<div style="font-size:14px;font-weight:600;color:' + esc(col) + ';">' + esc(ok ? '✅ Audit Chain Verified' : '❌ Audit Chain Broken') + '</div>' +
      '<div style="font-size:11px;color:var(--muted);margin-top:4px;">Chain valid: ' + esc(ok ? 'yes' : 'no') + '</div>' +
      '<div style="font-size:10px;font-family:var(--font-mono);color:var(--muted);margin-top:4px;">Claim: ' + esc(data.claim || '?') + '</div>' +
      '<div style="font-size:10px;font-family:var(--font-mono);color:var(--muted);margin-top:4px;">Head: ' + esc(headHex.substring(0, 32)) + '...</div>' +
      '<div style="font-size:10px;color:var(--muted);margin-top:4px;">Events: ' + esc(data.events||0) + ' in chain</div>' +
      '</div>';
  } catch(e) { div.innerHTML = '<div class="error-text">Chain verification failed: '+esc(e.message)+'</div>'; }
}

// ═══════════════════════════════════════════
// Program Debugger Panel (S2)
// ═══════════════════════════════════════════
async function loadDebuggerPrograms() {
  const sel = document.getElementById('debugger-program-select');
  sel.innerHTML = '<option value="">Loading...</option>';
  try {
    const data = await api('/api/v1/list-programs');
    const programs = data.programs || [];
    sel.innerHTML = '<option value="">Select a program...</option>';
    programs.forEach(p => {
      sel.innerHTML += '<option value="'+esc(p.koid)+'">'+esc(p.name)+' (v'+esc(p.version)+')</option>';
    });
  } catch(e) { sel.innerHTML = '<option value="">Error loading programs</option>'; }
}

async function loadProgramDebugger() {
  const koid = document.getElementById('debugger-program-select').value;
  const div = document.getElementById('debugger-result');
  if (!koid) { div.innerHTML = '<div class="error-text">Select a program.</div>'; return; }
  div.innerHTML = '<div class="loading">Loading program...</div>';
  try {
    // Get program details via graph query.
    const data = await api('/api/v1/list-programs');
    const programs = data.programs || [];
    const prog = programs.find(p => p.koid === koid);
    if (!prog) { div.innerHTML = '<div class="error-text">Program not found.</div>'; return; }
    let h = '<div class="card"><h3>'+esc(prog.name)+'</h3>';
    h += '<div class="kv-row"><span class="kv-key">KOID</span><span class="kv-val mono">'+esc(prog.koid)+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Language</span><span class="kv-val">'+ esc(prog.language||'aikoql') +'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Version</span><span class="kv-val">'+ esc(prog.version||'?') +'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Lifecycle</span><span class="kv-val">'+ esc(prog.lifecycle||'?') +'</span></div>';
    h += '</div>';
    // Show source.
    if (prog.body) {
      h += '<div class="card" style="margin-top:8px;"><h3>Source Code</h3>';
      h += '<pre style="background:var(--bg);padding:10px;border-radius:5px;font-size:12px;font-family:var(--font-mono);overflow-x:auto;">'+esc(prog.body)+'</pre>';
      h += '</div>';
    }
    // Execution stats.
    h += '<div class="card" style="margin-top:8px;"><h3>Execution Stats</h3>';
    try {
      const stats = await api('/api/v1/execution-stats');
      const s = stats.data || stats;
      h += '<div class="kv-row"><span class="kv-key">Programs Executed</span><span class="kv-val">'+esc(s.programs_executed||0)+'</span></div>';
      h += '<div class="kv-row"><span class="kv-key">Total Rows</span><span class="kv-val">'+esc(s.total_rows_returned||0)+'</span></div>';
      h += '<div class="kv-row"><span class="kv-key">Cache Hit %</span><span class="kv-val">'+esc((s.cache_hit_pct||0).toFixed(1))+'%</span></div>';
    } catch(e) { h += '<div style="color:var(--muted);font-size:11px;">Stats unavailable</div>'; }
    h += '</div>';
    // Show dependencies.
    h += '<div class="card" style="margin-top:8px;"><h3>Dependencies</h3>';
    h += '<button class="btn btn-sec btn-sm" data-koid="'+esc(prog.koid)+'" data-click="loadProgramDeps">Show Dependency Graph</button>';
    h += '<div id="debugger-deps" style="margin-top:8px;"></div>';
    h += '</div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

async function loadProgramDeps(koid) {
  const div = document.getElementById('debugger-deps');
  div.innerHTML = '<div class="loading">Loading...</div>';
  try {
    const data = await api('/api/v1/trace/' + encodeURIComponent(koid));
    const versions = (data.data||data).versions || (data.data||data).events || [];
    div.innerHTML = '<div style="font-size:11px;color:var(--muted);">'+esc(versions.length)+' versions in trace chain. Program DAG is stored as DEPENDS_ON relationships.</div>';
  } catch(e) { div.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

// ═══════════════════════════════════════════
// Benchmark Center Panel (S2)
// ═══════════════════════════════════════════
async function loadBenchmarks() {
  const div = document.getElementById('benchmarks-result');
  div.innerHTML = '<div class="loading">Loading benchmarks...</div>';
  try {
    const data = await api('/api/v1/list-benchmarks');
    const benchmarks = data.benchmarks || [];
    if (!benchmarks.length) { div.innerHTML = '<div class="empty-state">No benchmarks deployed. Create one below.</div>'; return; }
    let h = '<table><thead><tr><th>Name</th><th>Target Query</th><th>Iterations</th><th>Lifecycle</th><th>Actions</th></tr></thead><tbody>';
    benchmarks.forEach(b => {
      h += '<tr>';
      h += '<td><strong>'+esc(b.name)+'</strong></td>';
      h += '<td style="font-family:var(--font-mono);font-size:11px;">'+esc(b.target_query||'?')+'</td>';
      h += '<td>'+esc(b.iterations||0)+'</td>';
      h += '<td><span class="badge badge-cyan">'+esc(b.lifecycle)+'</span></td>';
      h += '<td><button class="btn btn-sec btn-sm" data-koid="'+esc(b.koid)+'" data-click="runBenchmark">▶ Run</button></td>';
      h += '</tr>';
    });
    h += '</tbody></table>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

async function runBenchmark(koid) {
  const div = document.getElementById('benchmarks-result');
  div.innerHTML = '<div class="loading">Running benchmark '+esc(koid)+'...</div>';
  try {
    // Deploy a program from the benchmark's target query for execution.
    const data = await api('/api/v1/list-benchmarks');
    const bm = (data.benchmarks||[]).find(b => b.koid === koid);
    if (!bm) { div.innerHTML = '<div class="error-text">Benchmark not found.</div>'; return; }
    const query = bm.target_query || '';
    const name = bm.name || 'unnamed';
    const iterations = parseInt(bm.iterations||'100', 10);

    // Execute the query via aikoql and measure.
    const start = performance.now();
    let results = null;
    for (let i = 0; i < iterations; i++) {
      try {
        results = await api('/api/v1/aikoql?query=' + encodeURIComponent(query));
      } catch(e) { /* continue */ }
    }
    const elapsed = (performance.now() - start).toFixed(1);
    const opsPerSec = (iterations / (elapsed / 1000)).toFixed(1);

    let h = '<div class="card" style="border-left:3px solid var(--cyan);">';
    h += '<h3>Benchmark: ' + esc(name) + '</h3>';
    h += '<div class="kv-row"><span class="kv-key">Iterations</span><span class="kv-val">' + esc(iterations) + '</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Total Time</span><span class="kv-val">' + esc(elapsed) + 'ms</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Throughput</span><span class="kv-val">' + esc(opsPerSec) + ' ops/s</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Avg Latency</span><span class="kv-val">' + esc((elapsed / iterations).toFixed(2)) + 'ms</span></div>';
    h += '</div>';
    h += '<button class="btn btn-sec btn-sm" style="margin-top:8px;" data-click="loadBenchmarks">← Back to Benchmarks</button>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

function showBenchmarkDeployForm() {
  const form = document.getElementById('benchmark-deploy-form');
  form.style.display = form.style.display === 'none' ? 'block' : 'none';
}

async function deployBenchmark() {
  const name = document.getElementById('bm-name').value.trim();
  const query = document.getElementById('bm-query').value.trim();
  const iterations = document.getElementById('bm-iterations').value;
  const warmup = document.getElementById('bm-warmup').value;
  const msg = document.getElementById('bm-deploy-msg');
  if (!name || !query) { msg.innerHTML = '<span style="color:var(--red);">Name and query required.</span>'; return; }
  msg.innerHTML = 'Deploying...';
  try {
    const body = JSON.stringify({name:name, target_query:query, iterations:parseInt(iterations,10), warmup:parseInt(warmup,10)});
    const res = await fetch('/api/v1/deploy-benchmark', {method:'POST', headers:authHeaders({'Content-Type':'application/json'}), body});
    const data = await res.json();
    if (data.koid) {
      msg.innerHTML = '<span style="color:var(--green);">Deployed: '+esc(data.koid)+'</span>';
      document.getElementById('bm-name').value = '';
      document.getElementById('bm-query').value = '';
      setTimeout(() => loadBenchmarks(), 500);
    } else { msg.innerHTML = '<span style="color:var(--red);">'+ esc(data.error||'Failed') +'</span>'; }
  } catch(e) { msg.innerHTML = '<span style="color:var(--red);">'+esc(e.message)+'</span>'; }
}

// ═══════════════════════════════════════════
// Query Profiler (S3)
// ═══════════════════════════════════════════
async function profileRun() {
  const query = document.getElementById('profiler-query').value.trim();
  const div = document.getElementById('profiler-results');
  if (!query) { div.innerHTML = '<div class="error-text">Enter a query.</div>'; return; }
  div.innerHTML = '<div class="loading">Profiling...</div>';
  const t0 = performance.now();
  try {
    const data = await api('/api/v1/aikoql?query=' + encodeURIComponent(query));
    const t1 = performance.now();
    const rows = data.rows || data.data?.rows || [];
    let h = '<div class="card" style="border-left:3px solid var(--accent);">';
    h += '<h4>Results</h4>';
    h += '<div style="font-size:11px;color:var(--muted);margin-bottom:8px;">' + esc(rows.length) + ' rows in ' + esc((t1-t0).toFixed(1)) + 'ms</div>';
    if (rows.length > 0) {
      h += '<table><thead><tr>';
      Object.keys(rows[0]).forEach(k => h += '<th>'+esc(k)+'</th>');
      h += '</tr></thead><tbody>';
      rows.slice(0,50).forEach(r => {
        h += '<tr>';
        Object.values(r).forEach(v => h += '<td style="font-size:10px;font-family:var(--font-mono);max-width:200px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;">'+esc(v===null?'∅':String(v).substring(0,80))+'</td>');
        h += '</tr>';
      });
      h += '</tbody></table>';
      if (rows.length > 50) h += '<div style="font-size:10px;color:var(--muted);margin-top:4px;">Showing 50 of '+esc(rows.length)+' rows.</div>';
    }
    h += '</div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}
async function profileExplain() {
  const koid = document.getElementById('profiler-koid-input').value.trim();
  const div = document.getElementById('profiler-results');
  if (!koid) { div.innerHTML = '<div class="error-text">Enter a KOID for EXPLAIN.</div>'; return; }
  div.innerHTML = '<div class="loading">Explaining...</div>';
  try {
    const data = await api('/api/v1/explain/' + encodeURIComponent(koid));
    const d = data.data || data;
    let h = '<div class="card"><h4>EXPLAIN — '+esc(d.origin)+'</h4>';
    h += '<div class="kv-row"><span class="kv-key">KOID</span><span class="kv-val mono">'+esc(d.koid)+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Version</span><span class="kv-val">'+esc(d.version)+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Source</span><span class="kv-val mono" style="font-size:10px;">'+esc((d.source||'').substring(0,200))+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Confidence</span><span class="kv-val">'+esc(d.confidence)+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Verified</span><span class="kv-val">'+esc(d.verified)+'</span></div>';
    if (d.evidence && d.evidence.length > 0) {
      h += '<div style="margin-top:8px;font-size:11px;font-weight:600;">Evidence ('+esc(d.evidence.length)+'):</div>';
      d.evidence.forEach(e => h += '<div class="kv-row"><span class="kv-key">'+esc(e.rel_type)+'</span><span class="kv-val mono">'+esc(e.target)+'</span></div>');
    }
    h += '</div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

// ═══════════════════════════════════════════
// Provider Manager (S3)
// ═══════════════════════════════════════════
async function loadProviders() {
  const div = document.getElementById('providers-list');
  try {
    const data = await api('/api/v1/list-connectors');
    const connectors = data.connectors || data.data?.connectors || [];
    if (connectors.length === 0) {
      div.innerHTML = '<div class="empty-state">No connectors deployed. Use the form below to add one.</div>';
      return;
    }
    let h = '<div class="card"><h4>Connectors ('+esc(connectors.length)+')</h4><table><thead><tr><th>KOID</th><th>Name</th><th>Plugin</th><th>Status</th></tr></thead><tbody>';
    connectors.forEach(c => {
      const lc = c.lifecycle || '?';
      const badge = lc === 'active' ? 'badge-green' : lc === 'draft' ? 'badge-cyan' : 'badge-muted';
      h += '<tr><td class="mono" style="font-size:10px;">'+esc(c.koid.substring(0,16))+'...</td><td>'+esc(c.name)+'</td><td>'+ esc(c.plugin||'?') +'</td><td><span class="badge '+esc(badge)+'">'+esc(lc)+'</span></td></tr>';
    });
    h += '</tbody></table></div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}
async function deployConnector() {
  const name = document.getElementById('connector-name').value.trim();
  const plugin = document.getElementById('connector-plugin').value.trim();
  const msg = document.getElementById('connector-deploy-result');
  if (!name || !plugin) { msg.innerHTML = '<span style="color:var(--red);">Name and plugin required.</span>'; return; }
  msg.innerHTML = 'Deploying...';
  try {
    const data = await apiPost('/api/v1/deploy-connector', {name, plugin, config:{}, schedule:'manual'});
    const d = data.data || data;
    msg.innerHTML = '<span style="color:var(--green);">Deployed: '+esc(d.koid)+'</span>';
    document.getElementById('connector-name').value = '';
    document.getElementById('connector-plugin').value = '';
    setTimeout(() => loadProviders(), 500);
  } catch(e) { msg.innerHTML = '<span style="color:var(--red);">'+esc(e.message)+'</span>'; }
}

// ═══════════════════════════════════════════
// Document Explorer (MRFC-0050 Phase D0)
// ═══════════════════════════════════════════
// ── Saving a document into the knowledge base (docs/ONTOLOGY… document knowledge graph) ──────────
// Compile is only a preview. Saving stores every page, fragment, chunk, entity, relation and fact, then
// reads them back and shows planned-vs-stored per kind, so completeness is shown, never assumed.
let saveWatch = null;
function kbCounts(m) {
  return Object.entries(m.kinds || {}).map(([k, v]) => k + ' ' + v.stored + '/' + v.planned).join(' · ');
}
function kbCell(doc) {
  const g = doc.graph || {status: 'not_saved'};
  let h = '';
  const busy = doc.status === 'processing' || doc.status === 'interrupted';
  if (g.status === 'saving') {
    h += '<div>Saving… '+esc(g.phase || '')+'</div>';
    if (g.total) h += '<div class="mono" style="font-size:10px;">'+esc(g.progress)+'/'+esc(g.total)+' objects</div>';
  } else if (g.status === 'saved' && g.manifest) {
    const m = g.manifest;
    if (m.complete) h += '<span class="badge badge-green">✓ complete</span>';
    else h += '<span class="badge badge-orange">incomplete</span>';
    h += '<div class="mono" style="font-size:10px;margin-top:3px;max-width:340px;">'+esc(kbCounts(m))+'</div>';
    (m.gaps || []).forEach(gap => { h += '<div style="font-size:10px;color:var(--orange);">'+esc(gap)+'</div>'; });
    if ((m.supplemented_pages || []).length) h += '<div style="font-size:10px;color:var(--muted);">The compiler made no chunk for page '+esc(m.supplemented_pages.join(', '))+' (image text); one was added so its text is searchable.</div>';
    h += '<div style="margin-top:4px;">'
      + '<button class="btn btn-sec btn-sm" data-click="viewDocumentGraph" data-koid="'+esc(doc.koid)+'">View graph</button> '
      + '<button class="btn btn-sec btn-sm" data-click="browseDocument" data-koid="'+esc(doc.koid)+'">Browse all</button> '
      + '<button class="btn btn-sec btn-sm" data-click="removeDocumentKnowledge" data-koid="'+esc(doc.koid)+'" title="Remove its knowledge from the knowledge base (the document stays ingested)">Remove</button></div>';
  } else if (g.status === 'failed' || g.status === 'interrupted') {
    h += '<div style="color:var(--red);font-size:11px;">'+esc(g.status === 'interrupted' ? 'Saving was interrupted by a server restart.' : (g.message || 'Saving failed.'))+'</div>';
    h += '<button class="btn btn-pri btn-sm" data-click="saveDocument" data-koid="'+esc(doc.koid)+'">Save again</button>';
  } else {
    h += '<span style="color:var(--muted);font-size:11px;">not saved</span> ';
    if (!busy) h += '<button class="btn btn-pri btn-sm" data-click="saveDocument" data-koid="'+esc(doc.koid)+'" title="Store this document in the knowledge base: every page, chunk, entity and relation">Save to knowledge base</button>';
  }
  return h;
}
async function saveDocument(koid) {
  if (!window.confirm('Save this document into the knowledge base?\n\nIt is read in full and every page, chunk, fragment, entity, relation and fact is stored. A large book can take several minutes; you can leave this page.')) return;
  const msg = document.getElementById('doc-ingest-result');
  try {
    await apiPost('/api/v1/documents/commit', {koid});
    toast('success', 'Saving the document into the knowledge base…');
    watchSaving(koid);
    loadDocuments();
  } catch(e) { msg.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}
function watchSaving(koid) {
  if (saveWatch) clearTimeout(saveWatch.timer);
  const me = saveWatch = { koid, timer: null };
  const msg = document.getElementById('doc-ingest-result');
  const tick = async () => {
    if (saveWatch !== me) return;
    if (!document.getElementById('panel-documents').classList.contains('active')) { saveWatch = null; return; }
    let d;
    try { const r = await api('/api/v1/documents/'+encodeURIComponent(koid)+'/status'); d = r.data || r; }
    catch(e) { saveWatch = null; msg.innerHTML = '<div class="error-text">Could not read the saving status: '+esc(e.message)+'</div>'; return; }
    if (saveWatch !== me) return;
    const g = d.graph || {};
    if (g.status === 'saving') {
      msg.innerHTML = '<div class="card"><b>Saving to the knowledge base…</b> '+esc(g.phase || '')+esc(g.total ? ' — '+g.progress+' of '+g.total+' objects written' : '')+'<div style="font-size:11px;color:var(--muted);margin-top:4px;">Compiling a large book takes minutes. You can leave this page; the result is shown in the Knowledge base column.</div></div>';
      loadDocumentsQuiet();
      me.timer = setTimeout(tick, 2000);
      return;
    }
    saveWatch = null;
    if (g.status === 'saved' && g.manifest) {
      const m = g.manifest;
      let h = '<div class="card">';
      if (m.complete) h += '<b style="color:var(--green);">✓ Saved — complete.</b> ';
      else h += '<b style="color:var(--orange);">Saved, but incomplete.</b> ';
      h += '<span class="mono" style="font-size:11px;">'+esc(kbCounts(m))+'</span>';
      h += '<div style="font-size:11px;color:var(--muted);margin-top:4px;">Stored counts were read back from the knowledge base and compared with what was planned. Page text: '+esc(m.page_chars.stored)+' of '+esc(m.page_chars.planned)+' characters.</div>';
      (m.gaps || []).forEach(gap => { h += '<div style="color:var(--orange);font-size:12px;">'+esc(gap)+'</div>'; });
      h += '</div>';
      msg.innerHTML = h;
    } else {
      msg.innerHTML = '<div class="error-text">'+esc(g.message || 'Saving did not finish.')+'</div>';
    }
    loadDocuments();
  };
  tick();
}
async function loadDocumentsQuiet() { try { await loadDocuments(); } catch(_) {} }
async function removeDocumentKnowledge(koid) {
  if (!window.confirm('Remove this document from the knowledge base?\n\nIts pages, chunks, entities and relations are removed from queries and the graph. The ingested document itself stays and can be saved again.')) return;
  const msg = document.getElementById('doc-ingest-result');
  try {
    const r = (await apiPost('/api/v1/documents/uncommit', {koid})).data;
    msg.innerHTML = '<div class="card">Removed '+esc((r || {}).removed)+' objects from the knowledge base.</div>';
    loadDocuments();
  } catch(e) { msg.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

// ── Browse every saved object, paged (the complete listing; the graph only draws a page) ─────────
const browse = { koid: null, kind: 'entity', q: '', offset: 0, size: 50 };
const BROWSE_KINDS = ['entity', 'chunk', 'page', 'fact', 'fragment'];
const KIND_LABEL = {entity: 'entities', chunk: 'chunks', page: 'pages', fact: 'facts', fragment: 'fragments'};
// The table lives on the Documents page: from anywhere else (the graph's "Browse all as a table"), go there first.
async function browseDocument(koid) {
  Object.assign(browse, {koid: koid, kind: 'entity', q: '', offset: 0});
  if (!document.getElementById('panel-documents').classList.contains('active')) switchPanel('documents');
  await renderBrowse();
  const el = document.getElementById('doc-browse');
  if (el && el.scrollIntoView) el.scrollIntoView({block: 'start'});
}
async function renderBrowse() {
  const el = document.getElementById('doc-browse');
  if (!browse.koid) { el.innerHTML = ''; return; }
  el.innerHTML = '<div class="loading">Loading…</div>';
  try {
    const url = '/api/v1/documents/objects?koid='+encodeURIComponent(browse.koid)+'&kind='+encodeURIComponent(browse.kind)+'&offset='+browse.offset+'&limit='+browse.size+(browse.q ? '&q='+encodeURIComponent(browse.q) : '');
    const r = (await api(url)).data || {};
    const rows = r.rows || [];
    let h = '<div class="card"><h4>Everything saved for this document</h4>';
    h += '<div style="margin:6px 0;">';
    BROWSE_KINDS.forEach(k => { h += '<button class="btn '+esc(k === browse.kind ? 'btn-pri' : 'btn-sec')+' btn-sm" data-click="browseKind" data-kind="'+esc(k)+'">'+esc(KIND_LABEL[k])+'</button> '; });
    h += '<input id="browse-q" placeholder="Search text…" value="'+esc(browse.q)+'" aria-label="Search saved text" style="margin-left:8px;"> <button class="btn btn-sec btn-sm" data-click="browseSearch">Search</button>';
    h += ' <button class="btn btn-sec btn-sm" data-click="browseClose">Close</button></div>';
    const from = rows.length ? r.offset + 1 : 0;
    h += '<div style="font-size:12px;margin-bottom:6px;"><b>'+esc(from)+'–'+esc(r.offset + rows.length)+' of '+esc(r.total)+'</b> '+esc(KIND_LABEL[browse.kind])+esc(browse.q ? ' matching “'+browse.q+'”' : '')+' — every one is reachable with Next.</div>';
    h += '<table class="data-table"><thead><tr><th>#</th><th>Page</th><th>Text / name</th><th>Type</th><th>Links</th><th></th></tr></thead><tbody>';
    rows.forEach(x => { h += '<tr><td>'+esc(x.ordinal)+'</td><td>'+esc(x.page)+'</td><td style="max-width:640px;">'+esc(x.label)+'</td><td>'+esc(x.entity_type)+'</td><td>'+esc(x.links)+'</td><td><button class="btn btn-sec btn-sm" data-click="openInInspector" data-koid="'+esc(x.koid)+'">Inspect</button></td></tr>'; });
    h += '</tbody></table>';
    h += '<div style="margin-top:6px;"><button class="btn btn-sec btn-sm" data-click="browsePage" data-dir="-1" '+esc(browse.offset > 0 ? '' : 'disabled')+'>Previous</button> <button class="btn btn-sec btn-sm" data-click="browsePage" data-dir="1" '+esc(r.next_offset !== null && r.next_offset !== undefined ? '' : 'disabled')+'>Next</button></div></div>';
    el.innerHTML = h;
  } catch(e) { el.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}
function browseKind(kind) { Object.assign(browse, {kind: kind, offset: 0}); return renderBrowse(); }
function browseSearch() { browse.q = (document.getElementById('browse-q').value || '').trim(); browse.offset = 0; return renderBrowse(); }
function browsePage(dir) { browse.offset = Math.max(0, browse.offset + dir * browse.size); return renderBrowse(); }

async function loadDocuments() {
  const el = document.getElementById('documents-list');
  try {
    const d = await api('/api/v1/list-documents');
    const docs = (d.documents || d.data?.documents || []);
    if (!docs.length) { el.innerHTML = '<div class="empty-state">No documents ingested yet. Upload one above.</div>'; return; }
    let html = '<table class="data-table"><thead><tr><th>KOID</th><th>Filename</th><th>Type</th><th>Size</th><th>Status</th><th>Knowledge base</th><th>SHA-256</th><th>Actions</th></tr></thead><tbody>';
    for (const doc of docs) {
      const sz = doc.size_bytes < 1024 ? doc.size_bytes+' B' : doc.size_bytes < 1048576 ? (doc.size_bytes/1024).toFixed(1)+' KB' : (doc.size_bytes/1048576).toFixed(1)+' MB';
      const busy = doc.status === 'processing' || doc.status === 'interrupted';
      const hp = kbCell(doc);
      const shown = doc.status === 'processing' ? doc.status+' '+(doc.batches_done||0)+'/'+(doc.batches_total||0) : doc.status;
      const compileTip = doc.status === 'processing' ? 'Still processing in batches — Compile is available once every batch has finished.' : 'Processing was interrupted — upload the file again to finish it.';
      html += '<tr><td class="mono"><a href="#" data-koid="'+esc(doc.koid)+'" data-click="openInInspector" style="color:var(--accent);">'+esc(doc.koid.substring(0,16))+'...</a></td><td>'+esc(doc.filename)+'</td><td>'+esc(doc.mime_type)+'</td><td>'+esc(sz)+'</td><td><span class="badge '+esc(doc.status === 'extracted' || doc.status === 'ocr_complete' ? 'badge-green' : doc.status === 'ocr_unavailable' || doc.status === 'ocr_partial' || doc.status === 'ocr_disabled' || doc.status === 'partial' ? 'badge-orange' : doc.status === 'limit_exceeded' || doc.status === 'interrupted' ? 'badge-red' : doc.status === 'processing' ? 'badge-cyan' : '')+'" title="'+esc(doc.message || ocrNotice(doc.status, doc.char_count))+'">'+esc(shown)+'</span></td><td>'+hp+'</td><td class="mono" style="font-size:9px;">'+esc(doc.sha256.substring(0,12))+'...</td><td><button class="btn btn-pri btn-sm" data-koid="'+esc(doc.koid)+'" data-click="compileDocument" '+esc(busy ? 'disabled' : '')+' title="'+esc(busy ? compileTip : 'Run the document compiler')+'">Compile</button></td></tr>';
    }
    html += '</tbody></table>';
    el.innerHTML = html;
    // back on the panel with a document still in batches: keep watching it
    const running = docs.find(d => d.status === 'processing');
    if (running && !batchWatch) watchBatches(running.koid);
    const saving = docs.find(d => d.graph && d.graph.status === 'saving');
    if (saving && !saveWatch) watchSaving(saving.koid);
  } catch(e) { el.innerHTML = '<div class="error-text">'+esc(e.message)+'</div>'; }
}

// What a document's OCR outcome means for its text, in words ('' = nothing to add).
function ocrNotice(status, chars) {
  if (status === 'ocr_unavailable') return 'No text extracted: OCR is not available on this server (install Tesseract to read text in images). The file is stored.';
  if (status === 'ocr_complete') return chars > 0 ? chars+' characters of text read by OCR.' : 'OCR found no text in this document.';
  if (status === 'ocr_partial') return 'OCR failed on part of this document; what could not be read has no text.';
  if (status === 'ocr_disabled') return 'No text extracted: OCR is switched off in this server\'s [ingestion] settings. The file is stored.';
  if (status === 'limit_exceeded') return 'Not extracted: the document exceeds a server ingestion limit ([ingestion] settings) and cannot be split into batches. The file is stored.';
  if (status === 'processing') return 'Large document: it is being processed in batches. Please wait until it finishes before compiling.';
  if (status === 'partial') return 'Some batches of this document failed; the other pages were read. Upload the file again to retry.';
  if (status === 'interrupted') return 'Processing was interrupted by a server restart. Upload the file again to finish it.';
  return '';
}

// Over-limit documents are read in batches on the server: show progress
// until every batch is done. One watch at a time; it stops when the
// document finishes or the user leaves the Documents panel (coming back
// resumes it from loadDocuments).
let batchWatch = null;
function watchBatches(koid) {
  if (batchWatch) clearTimeout(batchWatch.timer);
  const me = batchWatch = { koid, timer: null };
  const msg = document.getElementById('doc-ingest-result');
  const tick = async () => {
    if (batchWatch !== me) return;
    if (!document.getElementById('panel-documents').classList.contains('active')) { batchWatch = null; return; }
    let d;
    try { const r = await api('/api/v1/documents/'+encodeURIComponent(koid)+'/status'); d = r.data || r; }
    catch(e) { batchWatch = null; msg.innerHTML = '<div class="error-text">Could not read the processing status: '+esc(e.message)+'</div>'; return; }
    if (batchWatch !== me) return;
    renderBatches(msg, d);
    if (d.status === 'processing') { me.timer = setTimeout(tick, 2000); return; }
    batchWatch = null;
    loadDocuments();
  };
  tick();
}
function renderBatches(el, d) {
  const total = d.batches_total || 0, done = d.batches_done || 0;
  const pct = total ? Math.round(100 * done / total) : 0;
  const failed = (d.batches || []).filter(b => b.status === 'failed').length;
  let h;
  if (d.status === 'processing') h = '<h4>⏳ Large document — processing in batches</h4><div>'+esc(d.message || '')+'</div><div style="margin-top:4px;color:var(--orange);">Please keep this page open and wait until it finishes. Compile becomes available when every batch is done.</div>';
  else if (d.status === 'partial') h = '<h4 style="color:var(--orange);">Finished — '+esc(failed)+' of '+esc(total)+' batches failed</h4><div>'+esc(d.message || '')+'</div>';
  else if (d.status === 'interrupted') h = '<h4 style="color:var(--red);">Processing was interrupted</h4><div>'+esc(d.message || '')+'</div>';
  else h = '<h4 style="color:var(--green);">✓ Done — all '+esc(total)+' batches processed</h4><div>'+esc(d.filename || 'The document')+' is ready ('+esc(d.char_count || 0)+' characters from '+esc(d.page_count || 0)+' pages). You can compile it now.</div>';
  let html = '<div class="card batch-notice" data-batch-status="'+esc(d.status)+'">'+h;
  html += '<div class="progress" role="progressbar" aria-label="Batches processed" aria-valuemin="0" aria-valuemax="'+esc(total)+'" aria-valuenow="'+esc(done)+'"><div class="progress-bar" style="width:'+esc(pct)+'%;"></div></div>';
  html += '<div style="font-size:11px;color:var(--muted);">Batch '+esc(done)+' of '+esc(total)+' done · '+esc(d.pages_done || 0)+' of '+esc(d.page_count || 0)+' pages</div>';
  if ((d.batches || []).length) {
    html += '<table class="data-table" style="margin-top:8px;"><thead><tr><th>Pages</th><th>Result</th><th>Characters</th><th>Images</th><th>Tables</th></tr></thead><tbody>';
    for (const b of d.batches) {
      const bad = b.status === 'failed';
      html += '<tr><td>'+esc(b.first_page)+'–'+esc(b.last_page)+'</td><td><span class="badge '+esc(bad ? 'badge-red' : 'badge-green')+'">'+esc(b.status)+'</span> <span style="color:var(--red);">'+esc(bad ? b.error || '' : '')+'</span></td><td>'+esc(bad ? '' : b.char_count)+'</td><td>'+esc(bad ? '' : b.images)+'</td><td>'+esc(bad ? '' : b.tables)+'</td></tr>';
    }
    html += '</tbody></table>';
  }
  el.innerHTML = html + '</div>';
}

async function ingestDocument() {
  const fileInput = document.getElementById('doc-file-input');
  const msg = document.getElementById('doc-ingest-result');
  const file = fileInput.files[0];
  if (!file) { msg.innerHTML = '<span style="color:var(--red);">Select a file.</span>'; return; }
  msg.innerHTML = 'Reading file...';
  try {
    const base64 = await new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        const b64 = reader.result.split(',')[1];
        if (b64) resolve(b64); else reject(new Error('not a data URL'));
      };
      reader.onerror = () => reject(new Error('read error'));
      reader.readAsDataURL(file);
    });
    msg.innerHTML = 'Ingesting...';
    const data = await apiPost('/api/v1/documents', {filename: file.name, content_base64: base64, mime_type: file.type || 'application/octet-stream'});
    const d = data.data || data;
    fileInput.value = '';
    if (d.status === 'processing') { watchBatches(d.koid); setTimeout(() => loadDocuments(), 500); return; }
    const sz = d.size_bytes < 1024 ? d.size_bytes+' B' : (d.size_bytes/1024).toFixed(1)+' KB';
    const note = ocrNotice(d.status, d.char_count);
    let html = '<span style="color:var(--green);">✓ Ingested: '+esc(d.koid)+' ('+esc(d.status)+', '+esc(sz)+', SHA-256: '+esc(d.sha256.substring(0,16))+'...)</span>';
    if (note) html += '<div style="margin-top:4px;color:var(--'+esc(d.char_count > 0 ? 'muted' : 'orange')+');">'+esc(note)+'</div>';
    // why OCR failed, from the server (the first few pages)
    ((d.ocr_stats && d.ocr_stats.failures) || []).forEach(f => { html += '<div class="mono" style="margin-top:2px;font-size:11px;color:var(--muted);">'+esc(f)+'</div>'; });
    if (d.error) html += '<div style="margin-top:4px;color:var(--red);">'+esc(d.error)+'</div>';
    msg.innerHTML = html;
    setTimeout(() => loadDocuments(), 500);
  } catch(e) { msg.innerHTML = '<span style="color:var(--red);">'+esc(e.message)+'</span>'; }
}

async function compileDocument(koid) {
  const el = document.getElementById('doc-compile-result');
  el.innerHTML = '<div class="loading">Compiling document... (running D3→D9 pipeline)</div>';
  try {
    // a book compiles to tens of MB: ask for a view a page can hold (the API has the full result)
    const raw = await apiPost('/api/v1/documents/compile', {koid, limit: 100, max_text: 1000});
    const r = raw.data || raw; // REST API wraps in {"data": ...}

    // Helper: extract enum variant name from serde's tagged representation.
    // e.g. {"CreateKO": {...}} → "CreateKO", {"Skip": {...}} → "Skip"
    const actionKind = (a) => {
      if (a.CreateKO) return 'CreateKO';
      if (a.UpdateKO) return 'UpdateKO';
      if (a.Skip) return 'Skip';
      if (a.NeedsReview) return 'NeedsReview';
      return '?';
    };
    const actionPayload = (a) => a.CreateKO || a.UpdateKO || a.Skip || a.NeedsReview || {};
    const badgeFor = (kind) => {
      if (kind === 'CreateKO') return '<span class="badge badge-green">Create</span>';
      if (kind === 'UpdateKO') return '<span class="badge badge-cyan">Update</span>';
      if (kind === 'Skip') return '<span class="badge">Skip</span>';
      if (kind === 'NeedsReview') return '<span class="badge badge-orange">Review</span>';
      return '<span class="badge">'+esc(kind)+'</span>';
    };

    let html = '<div class="card" style="margin-bottom:12px;"><h4>🔬 Compilation Results</h4>';
    // Compile is a dry run: nothing is stored until the document is saved
    html += '<div style="margin-bottom:8px;padding:8px;border:1px solid var(--border);border-radius:6px;font-size:12px;"><b>Preview — nothing is stored.</b> To make this document part of the knowledge base (and its graph), save it: every page, chunk, entity and relation is stored and verified. '
      + '<button class="btn btn-pri btn-sm" data-click="saveDocument" data-koid="'+esc(koid)+'">Save to knowledge base</button></div>';
    if (r.extraction_status && !r.total_chars) {
      html += '<div style="margin-bottom:8px;color:var(--orange);">'+esc(ocrNotice(r.extraction_status, 0) || 'No text was extracted from this document.')+'</div>';
    }

    if (r.truncated) {
      // a large document: the server bounds every list, and the counts below are of what is shown
      html += '<div style="margin-bottom:8px;color:var(--orange);font-size:12px;">Large result: showing the first items of each list only — '
        + esc(Object.entries(r.truncated).map(([k, v]) => k === 'long_text' ? v.fields + ' long text field(s) cut to ' + v.max_chars + ' characters' : k + ' ' + v.shown + ' of ' + v.total).join(', '))
        + '. The full result is available from the API with a higher <code>limit</code> / <code>max_text</code>.</div>';
    }

    // ── Stats ──
    html += '<details open><summary><b>Phase Stats</b> ('+esc(r.stats.phases.length)+' phases, '+esc(r.stats.total_us)+' µs total)</summary>';
    html += '<table class="data-table" style="margin-top:8px;"><thead><tr><th>Phase</th><th>Duration (µs)</th><th>Items</th></tr></thead><tbody>';
    for (const p of r.stats.phases) {
      html += '<tr><td>'+esc(p.phase)+'</td><td>'+esc(p.duration_us)+'</td><td>'+esc(p.item_count)+'</td></tr>';
    }
    html += '</tbody></table></details>';

    // ── Knowledge IR ──
    html += '<details style="margin-top:8px;"><summary><b>Knowledge IR</b> ('+esc(r.ir.entities.length)+' entities, '+esc(r.ir.facts.length)+' facts, '+esc(r.ir.temporal.length)+' temporal)</summary>';
    if (r.ir.entities.length) {
      html += '<table class="data-table" style="margin-top:8px;"><thead><tr><th>Entity</th><th>Class</th><th>Properties</th><th>Confidence</th></tr></thead><tbody>';
      for (const e of r.ir.entities) {
        html += '<tr><td><b>'+esc(e.name)+'</b></td><td>'+esc(e.type_hint||'?')+'</td><td>'+esc(JSON.stringify(e.mentions||[]).substring(0,120))+'</td><td>'+esc((e.confidence||0).toFixed(2))+'</td></tr>';
      }
      html += '</tbody></table>';
    }
    if (r.ir.facts.length) {
      html += '<div style="margin-top:6px;font-size:11px;color:var(--muted);">Facts: '+esc(r.ir.facts.map(f=>f.statement).join('; '))+'</div>';
    }
    html += '</details>';

    // ── Ontology ──
    html += '<details style="margin-top:8px;"><summary><b>Ontology Proposals</b> ('+esc(r.ontology.classes.length)+' classes, '+esc(r.ontology.properties.length)+' properties, '+esc(r.ontology.relationships.length)+' relationships)</summary>';
    if (r.ontology.classes.length) {
      html += '<div style="margin-top:6px;"><b>Classes:</b> '+esc(r.ontology.classes.map(c=>c.name+' (parent: '+(c.parent||'none')+')').join(', '))+'</div>';
    }
    if (r.ontology.properties.length) {
      html += '<div style="margin-top:4px;"><b>Properties:</b> '+esc(r.ontology.properties.map(p=>p.name+' ('+p.value_type+')').join(', '))+'</div>';
    }
    if (r.ontology.relationships.length) {
      html += '<div style="margin-top:4px;"><b>Relationships:</b> '+esc(r.ontology.relationships.map(r=>r.name+' ('+(r.domain||'?')+' → '+(r.range||'?')+')').join(', '))+'</div>';
    }
    html += '</details>';

    // ── Resolution ──
    html += '<details style="margin-top:8px;"><summary><b>Entity Resolution</b> ('+esc(r.resolution.stats.total_entities)+' total: '+esc(r.resolution.stats.matched_count)+' matched, '+esc(r.resolution.stats.ambiguous_count)+' ambiguous, '+esc(r.resolution.stats.unmatched_count)+' unmatched)</summary>';
    if (r.resolution.matched.length) {
      html += '<div style="margin-top:6px;font-size:11px;color:var(--green);">✓ Matched: '+esc(r.resolution.matched.map(m=>m.entity_name+' → '+m.matched_koid).join(', '))+'</div>';
    }
    if (r.resolution.ambiguous.length) {
      html += '<div style="margin-top:4px;font-size:11px;color:var(--orange);">⚠ Ambiguous: '+esc(r.resolution.ambiguous.map(m=>m.entity_name).join(', '))+'</div>';
    }
    if (r.resolution.unmatched.length) {
      html += '<div style="margin-top:4px;font-size:11px;color:var(--red);">✗ Unmatched: '+esc(r.resolution.unmatched.map(m=>m.entity_name).join(', '))+'</div>';
    }
    html += '</details>';

    // ── Commit Plan ──
    html += '<details style="margin-top:8px;"><summary><b>Commit Plan</b> ('+esc(r.commit_plan.stats.total_actions)+' actions: '+esc(r.commit_plan.stats.creates)+' create, '+esc(r.commit_plan.stats.updates)+' update, '+esc(r.commit_plan.stats.skips)+' skip, '+esc(r.commit_plan.stats.needs_review)+' review, '+esc(r.commit_plan.stats.total_conflicts)+' conflicts)</summary>';
    if (r.commit_plan.actions.length) {
      html += '<table class="data-table" style="margin-top:8px;"><thead><tr><th>Kind</th><th>Entity</th><th>Details</th></tr></thead><tbody>';
      for (const a of r.commit_plan.actions) {
        const k = actionKind(a);
        const p = actionPayload(a);
        let detail = '';
        if (k === 'CreateKO') detail = 'class: '+(p.class_name||'?')+', props: '+JSON.stringify(p.properties||[]);
        else if (k === 'UpdateKO') detail = 'koid: '+(p.koid||'?')+', conflicts: '+(p.conflicts||[]).length;
        else if (k === 'Skip') detail = p.reason||'';
        else if (k === 'NeedsReview') detail = (p.reason||'')+', conflicts: '+(p.conflicts||[]).length;
        html += '<tr><td>'+esc(badgeFor(k))+'</td><td><b>'+esc(p.entity_name)+'</b></td><td style="font-size:10px;">'+detail+'</td></tr>';
      }
      html += '</tbody></table>';
    }
    html += '</details>';

    // ── Evidence Trail ──
    html += '<details style="margin-top:8px;"><summary><b>Evidence Trail</b> ('+esc(r.evidence_trail.nodes.length)+' nodes)</summary>';
    if (r.evidence_trail.nodes.length) {
      for (const n of r.evidence_trail.nodes) {
        html += '<div style="margin-top:6px;padding:8px;background:var(--bg);border-radius:5px;border-left:3px solid var(--accent);"><span class="badge badge-purple">'+esc(n.phase)+'</span> <span style="font-size:11px;">'+esc(n.step)+'</span>';
        if (n.entities && n.entities.length) {
          html += '<div style="margin-top:4px;font-size:10px;color:var(--muted);">Entities: '+esc(n.entities.join(', '))+'</div>';
        }
        if (n.source && n.source.length) {
          html += '<div style="margin-top:2px;font-size:10px;color:var(--muted);">Evidence items: '+esc(n.source.length)+'</div>';
        }
        html += '</div>';
      }
    }
    html += '</details>';

    // ── Chunks ──
    html += '<details style="margin-top:8px;"><summary><b>Embedded Chunks</b> ('+esc(r.embedded_chunks.length)+' chunks)</summary>';
    if (r.embedded_chunks.length) {
      html += '<table class="data-table" style="margin-top:8px;"><thead><tr><th>#</th><th>Structure</th><th>Heading Path</th><th>Preview</th><th>Embedding Dims</th></tr></thead><tbody>';
      for (const ec of r.embedded_chunks) {
        const ch = ec.chunk || ec;
        const hp = (ch.structure && ch.structure.heading_path || []).join(' › ');
        html += '<tr><td>'+esc(ch.position ? ch.position.chunk_index : 0)+'</td><td>'+esc(ch.structure ? ch.structure.source_type : '?')+'</td><td style="font-size:10px;">'+hp+'</td><td style="font-size:10px;">'+esc((ch.text||'').substring(0,80))+'...</td><td>'+esc((ec.embedding||[]).length)+'</td></tr>';
      }
      html += '</tbody></table>';
    }
    html += '</details>';

    html += '</div>'; // card
    el.innerHTML = html;
    el.scrollIntoView({behavior:'smooth'});
  } catch(e) { el.innerHTML = '<div class="error-text">Compile failed: '+esc(e.message)+'</div>'; }
}

// ═══════════════════════════════════════════
// Startup
// ═══════════════════════════════════════════
document.addEventListener('DOMContentLoaded', () => {
  showLogin();
  document.getElementById('login-pass').addEventListener('keydown', e => { if(e.key==='Enter') doLogin(); });
});

// ═══════════════════════════════════════════
// Panel: Data Sources (/api/v1/sources*)
// ═══════════════════════════════════════════
const sourcesState = { merges: [], names: [], fingerprints: null, activeVersion: null, suggestions: [],
  identities: {}, links: [], assertions: [] };
// What the owner has accepted in the preview: keys, links and assertions ride along with
// every preview and publish, so the draft the reviewer sees is the draft that is saved.
// Only declarations about the sources being previewed are sent: a leftover about another source
// (or a published one) would be rejected or, worse, written into a draft it does not belong to.
function declaredBody(names) {
  const mine = ref => names.some(n => ref.startsWith(n + '.'));
  const b = {};
  const ids = Object.fromEntries(Object.entries(sourcesState.identities).filter(([c]) => mine(c)));
  const links = sourcesState.links.filter(l => mine(l.from) && mine(l.to));
  const asserts = sourcesState.assertions.filter(a => mine(a.from));
  if (Object.keys(ids).length) b.identities = ids;
  if (links.length) b.relationships = links;
  if (asserts.length) b.assertions = asserts;
  return b;
}
function pct(x) { return Math.round(Number(x) * 100) + '%'; }
function sourcesBody(extra) {
  const t = document.getElementById('sources-tenant').value;
  return Object.assign(t ? {tenant: t} : {}, extra);
}
function sourcesMsg(text) { document.getElementById('sources-msg').textContent = text; }
function describeDiff(d) {
  const parts = [];
  if ((d.added_containers || []).length) parts.push('added ' + d.added_containers.join(', '));
  if ((d.removed_containers || []).length) parts.push('removed ' + d.removed_containers.join(', '));
  (d.changed || []).forEach(c => parts.push(c.name + ': +' + (c.added_fields || []).length + ' -' + (c.removed_fields || []).length + ' fields, ' + (c.type_changes || []).length + ' retyped'
    + ((c.added_indexes || []).length + (c.removed_indexes || []).length + (c.redefined_indexes || []).length ? ', indexes +' + (c.added_indexes || []).length + ' -' + (c.removed_indexes || []).length + ' ~' + (c.redefined_indexes || []).length : '')
    + ((c.added_checks || []).length + (c.removed_checks || []).length + (c.redefined_checks || []).length ? ', checks +' + (c.added_checks || []).length + ' -' + (c.removed_checks || []).length + ' ~' + (c.redefined_checks || []).length : '')
    + ((c.property_changes || []).length ? ', ' + c.property_changes.length + ' storage setting(s) changed' : '')));
  return parts.join('; ');
}
function renderSourceCard(s) {
  const last = s.last;
  const names = last ? (last.container_names || []).slice(0, 100) : [];
  return '<div class="card" style="margin-bottom:10px;">'
    + '<label style="float:right;font-size:11px;"><input type="checkbox" class="src-pick" data-name="'+esc(s.name)+'"> include in ontology</label>'
    + '<h4>'+esc(s.name)+' <span class="badge badge-cyan">'+esc(s.kind)+'</span> <span style="font-size:11px;color:var(--muted);">'+esc(s.tenant ? '@' + s.tenant : '')+'</span></h4>'
    + '<div style="font-size:11px;color:var(--muted);margin:4px 0;">'+esc(last ? last.containers + (last.containers === 1 ? ' container' : ' containers') + (last.truncated ? ' (list truncated)' : '') + ' · schema ' + String(last.fingerprint).slice(0, 10) : 'not discovered yet')+'</div>'
    + '<button class="btn btn-pri btn-sm" data-click="discoverSource" data-name="'+esc(s.name)+'">Discover</button>'
    + '<div style="margin:8px 0;font-size:11px;">'
    + names.map(n => '<label style="display:inline-block;margin-right:12px;"><input type="checkbox" class="imp-pick" data-source="'+esc(s.name)+'" data-name="'+esc(n)+'"> '+esc(n)+'</label>').join('')
    + '</div>'
    + '<button class="btn btn-sec btn-sm" data-click="importSource" data-name="'+esc(s.name)+'">Import checked tables</button>'
    + '</div>';
}
async function loadSourcesPanel() {
  const list = document.getElementById('sources-list');
  try {
    const t = document.getElementById('sources-tenant').value;
    const rows = (await api('/api/v1/sources' + (t ? '?tenant=' + encodeURIComponent(t) : ''))).data || [];
    list.innerHTML = rows.length ? rows.map(renderSourceCard).join('') : '<div class="empty-state">No sources configured — see docs/SOURCES.md</div>';
  } catch (e) {
    list.innerHTML = '<div class="card" style="border-color:var(--red);">'+esc(e.message)+'</div>';
  }
}
async function discoverSource(name) {
  const out = document.getElementById('sources-result');
  out.innerHTML = '<div class="loading">Reading the schema...</div>';
  try {
    const d = (await apiPost('/api/v1/sources/discover', sourcesBody({source: name}))).data;
    out.innerHTML = '<div class="card">'+esc(d.source + ': ' + d.containers + ' containers, ' + d.references + ' references' + (d.truncated ? ' (truncated)' : '') + (d.changed ? ' — schema changed since the last run' : ' — unchanged'))+'<div style="font-size:11px;color:var(--muted);">'+esc(describeDiff(d.diff))+'</div></div>';
    loadSourcesPanel();
  } catch (e) {
    out.innerHTML = '<div class="card" style="border-color:var(--red);">'+esc(e.message)+'</div>';
  }
}
function renderDeclared(p) {
  const st = sourcesState;
  let h = '<h4 style="margin-top:14px;">Keys — which rows are the same row</h4>'
    + '<div style="font-size:11px;color:var(--muted);">A key makes a re-imported row update in place and keep a history. A key that is not unique or has gaps is refused at import, never guessed.</div>';
  const ids = p.identities || [];
  const pub = p.published || {identities: {}, links: [], assertions: []};
  if (ids.length) {
    h += '<table style="font-size:12px;margin-top:6px;"><thead><tr><th>Container</th><th>Key</th><th>Confidence</th><th>Why</th><th></th></tr></thead><tbody>';
    ids.forEach(s => {
      const on = JSON.stringify(st.identities[s.container] || []) === JSON.stringify(s.columns || []);
      h += '<tr><td class="mono" style="font-size:10px;">'+esc(s.container)+'</td><td class="mono">'+esc((s.columns || []).join(', '))+'</td>'
        + '<td><span class="badge badge-cyan">'+esc(pct(s.confidence))+'</span>';
      if (s.declared) h += ' <span class="badge badge-green">declared by the source</span>';
      h += '</td><td style="font-size:10px;color:var(--muted);">'+esc((s.evidence || []).join('; '))+'</td>'
        + '<td>';
      if (s.published) h += '<span class="badge badge-green">published</span>';
      else h += '<button class="btn btn-sec btn-sm" data-click="toggleIdentity" data-container="'+esc(s.container)+'" data-columns="'+esc(JSON.stringify(s.columns || []))+'">'+esc(on ? 'Remove' : 'Use as key')+'</button>';
      h += '</td></tr>';
    });
    h += '</tbody></table>';
  } else {
    h += '<div style="font-size:11px;margin-top:6px;">No key could be proposed for these sources.</div>';
  }
  h += '<h4 style="margin-top:14px;">Links — which rows relate</h4>'
    + '<div style="font-size:11px;color:var(--muted);">An accepted link becomes a relationship between the rows at import (walk it with TRAVERSE). A reference to no row, or to several, gets no edge.</div>';
  pub.links.forEach(l => {
    h += '<div style="font-size:12px;margin-top:4px;"><span class="badge badge-green">published</span> <b>'+esc(l.name)+'</b> <span class="mono" style="font-size:10px;">'+esc(l.from)+' → '+esc(l.to)+'</span></div>';
  });
  st.links.forEach((l, i) => {
    h += '<div style="font-size:12px;margin-top:4px;"><span class="badge badge-orange">pending</span> <b>'+esc(l.name)+'</b> <span class="mono" style="font-size:10px;">'+esc(l.from)+' → '+esc(l.to)+'</span> <button class="btn btn-sec btn-sm" data-click="removeLink" data-index="'+esc(i)+'">Remove</button></div>';
  });
  (p.links || []).forEach((l, i) => {
    h += '<div style="font-size:12px;margin-top:4px;"><b>'+esc(l.name)+'</b> <span class="mono" style="font-size:10px;">'+esc(l.from)+' → '+esc(l.to)+'</span> <span class="badge badge-cyan">'+esc(pct(l.confidence))+'</span> <span style="font-size:10px;color:var(--muted);">'+esc((l.evidence || []).join('; '))+'</span> <button class="btn btn-sec btn-sm" data-click="acceptLink" data-index="'+esc(i)+'">Accept</button></div>';
  });
  if (!st.links.length && !pub.links.length && !(p.links || []).length) h += '<div style="font-size:11px;margin-top:6px;">No link could be proposed.</div>';
  h += '<h4 style="margin-top:14px;">Authority — who is believed when sources disagree</h4>'
    + '<div style="font-size:11px;color:var(--muted);">Rows of a container become claims about one thing (for example an Account) named by its key column. The <b>lowest rank wins</b>; equal ranks leave the disagreement open.</div>';
  pub.assertions.forEach(a => {
    h += '<div style="font-size:12px;margin-top:4px;"><span class="badge badge-green">published</span> <span class="mono" style="font-size:10px;">'+esc(a.from)+'</span> asserts <b>'+esc(a.class)+'</b> '+esc(Object.entries(a.properties || {}).map(([k, v]) => k + ' → ' + v).join(', '))+' <span class="badge badge-purple">rank '+esc(a.rank)+'</span></div>';
  });
  if (pub.assertions.length || pub.links.length || Object.keys(pub.identities).length) h += '<div style="font-size:10px;color:var(--muted);margin-top:4px;">Published items cannot be removed here; change or remove them under Ontology → Advanced.</div>';
  st.assertions.forEach((a, i) => {
    h += '<div style="font-size:12px;margin-top:4px;"><span class="badge badge-orange">pending</span> <span class="mono" style="font-size:10px;">'+esc(a.from)+'</span> asserts <b>'+esc(a.class)+'</b> '+esc(Object.entries(a.properties).map(([k, v]) => k + ' → ' + v).join(', '))+' <span class="badge badge-purple">rank '+esc(a.rank)+'</span> <button class="btn btn-sec btn-sm" data-click="removeAssertion" data-index="'+esc(i)+'">Remove</button></div>';
  });
  h += '<div style="margin-top:6px;font-size:11px;">'
    + '<input id="as-from" placeholder="source.container.keycolumn" aria-label="Key column of the claim" size="30"> '
    + '<input id="as-class" placeholder="Class (e.g. Account)" aria-label="Class the claim is about" size="16"> '
    + '<input id="as-props" placeholder="column=property, column=property" aria-label="Columns and the properties they assert" size="30"> '
    + '<input id="as-rank" type="number" min="0" value="100" aria-label="Rank (lowest wins)" style="width:70px;"> '
    + '<button class="btn btn-sec btn-sm" data-click="addAssertion">Add</button></div>';
  return h;
}
function toggleIdentity(el) {
  const c = el.dataset.container, cols = JSON.parse(el.dataset.columns);
  if (JSON.stringify(sourcesState.identities[c] || []) === JSON.stringify(cols)) delete sourcesState.identities[c];
  else sourcesState.identities[c] = cols;
  return previewSources();
}
function acceptLink(index) {
  const l = (sourcesState.lastPreview.links || [])[index];
  if (!l) { sourcesMsg('That suggestion is no longer current — preview again.'); return; }
  sourcesState.links.push({name: l.name, from: l.from, to: l.to});
  return previewSources();
}
function removeLink(index) { sourcesState.links.splice(index, 1); return previewSources(); }
function addAssertion() {
  const v = id => document.getElementById(id).value.trim();
  const props = {};
  for (const part of v('as-props').split(',')) {
    const [a, b] = part.split('=').map(x => (x || '').trim());
    if (a && b) props[a] = b;
  }
  const rank = v('as-rank') === '' ? NaN : Number(v('as-rank'));
  if (!v('as-from') || !v('as-class') || !Object.keys(props).length || !Number.isInteger(rank) || rank < 0) {
    sourcesMsg('An assertion needs a key column (source.container.column), a class, at least one column=property pair, and a whole-number rank.');
    return;
  }
  sourcesState.assertions.push({from: v('as-from'), class: v('as-class'), properties: props, rank: rank});
  return previewSources().then(() => {
    // the server ignores what it cannot honour: do not keep showing it as declared
    const why = ((sourcesState.lastPreview || {}).warnings || []).find(w => /assertion ignored/.test(w));
    if (why) sourcesState.assertions.pop();
    if (why) return previewSources().then(() => { sourcesMsg(why); });
  });
}
function removeAssertion(index) { sourcesState.assertions.splice(index, 1); return previewSources(); }
function renderSourcesPreview(p) {
  return '<div class="card"><h4>Proposed ontology</h4>'
    + '<table style="font-size:12px;border-collapse:collapse;">'
    + (p.classes || []).map(c => '<tr><td style="padding:2px 8px;"><input type="checkbox" class="cls-pick" data-class="'+esc(c.name)+'" data-ref="'+esc((c.sources || [])[0])+'"></td><td>'+esc(c.name)+'</td><td><span class="badge '+esc(c.status === 'new' ? 'badge-green' : 'badge-orange')+'">'+esc(c.status === 'new' ? 'new' : 'possible duplicate')+'</span></td><td class="mono" style="font-size:10px;">'+esc((c.sources || []).join(', '))+'</td></tr>').join('')
    + '</table>'
    + '<div style="font-size:11px;margin-top:8px;">Adds: '+esc((p.adds || []).join(', ') || 'none')+'</div>'
    + '<div style="font-size:11px;">Refreshed: '+esc((p.refreshed || []).join(', ') || 'none')+'</div>'
    + '<div style="font-size:11px;">Left untouched: '+esc((p.untouched || []).join(', ') || 'none')+'</div>'
    + (p.warnings || []).map(w => '<div style="font-size:11px;color:var(--orange);">'+esc(w)+'</div>').join('')
    + ((p.suggestions || []).length
      ? '<h4 style="margin-top:12px;">Suggested merges</h4><div style="font-size:11px;color:var(--muted);">Proposals only — nothing changes until you apply one.</div>'
        + p.suggestions.map((s, i) => '<div class="card" style="margin:6px 0;"><b>'+esc(s.a)+'</b> ~ <b>'+esc(s.b)+'</b> <span class="badge badge-cyan">'+esc(pct(s.confidence))+'</span>'
          + (s.evidence || []).map(e => '<div style="font-size:11px;">'+esc(e)+'</div>').join('')
          + (s.properties || []).map(pr => '<div class="mono" style="font-size:11px;">'+esc(pr.a_field)+' = '+esc(pr.b_field)+' ('+esc(pct(pr.confidence))+')</div>').join('')
          + '<button class="btn btn-sec btn-sm" data-click="applySuggestion" data-index="'+esc(i)+'">Apply this merge</button></div>').join('')
      : '')
    + renderDeclared(p)
    + '<div style="margin-top:10px;"><button class="btn btn-sec btn-sm" data-click="mergeSelectedClasses">Merge the two checked classes</button> <button class="btn btn-pri btn-sm" data-click="applySourceOntology">Publish</button></div>'
    + '</div>';
}
async function previewSources() {
  const names = [...document.querySelectorAll('.src-pick:checked')].map(e => e.dataset.name);
  if (!names.length) { sourcesMsg('Check at least one source first.'); return; }
  const out = document.getElementById('sources-preview');
  // a re-preview redraws the section: keep what the owner was in the middle of typing or picking
  const typed = Object.fromEntries(['as-from', 'as-class', 'as-props', 'as-rank'].map(id => [id, (document.getElementById(id) || {}).value]));
  const picked = [...document.querySelectorAll('.cls-pick:checked')].map(e => e.dataset.class);
  try {
    const p = (await apiPost('/api/v1/sources/ontology', sourcesBody(Object.assign({sources: names, merges: sourcesState.merges, preview: true}, declaredBody(names))))).data;
    Object.assign(sourcesState, {names: names, fingerprints: p.fingerprints, activeVersion: p.active_version, suggestions: p.suggestions || [], lastPreview: p});
    out.innerHTML = renderSourcesPreview(p);
    Object.entries(typed).forEach(([id, v]) => { const e = document.getElementById(id); if (e && v !== undefined && v !== '' && id !== 'as-rank') e.value = v; });
    document.querySelectorAll('.cls-pick').forEach(e => { if (picked.includes(e.dataset.class)) e.checked = true; });
    sourcesMsg('Preview ready — nothing has been written.');
  } catch (e) {
    out.innerHTML = '';
    sourcesMsg(e.message);
  }
}
function mergeSelectedClasses() {
  const picked = [...document.querySelectorAll('.cls-pick:checked')];
  if (picked.length !== 2) { sourcesMsg('Check exactly two classes to merge.'); return; }
  sourcesState.merges.push([picked[0].dataset.ref, picked[1].dataset.ref]);
  return previewSources();
}
function applySuggestion(index) {
  const s = sourcesState.suggestions[index];
  if (!s) { sourcesMsg('That suggestion is no longer current — preview again.'); return; }
  sourcesState.merges.push({a: s.a, b: s.b, properties: (s.properties || []).map(pr => [pr.a_field, pr.b_field])});
  return previewSources();
}
async function applySourceOntology() {
  if (!sourcesState.fingerprints) { sourcesMsg('Preview first.'); return; }
  const body = sourcesBody(Object.assign({sources: sourcesState.names, merges: sourcesState.merges, preview: false, fingerprints: sourcesState.fingerprints}, declaredBody(sourcesState.names)));
  if (sourcesState.activeVersion !== null && sourcesState.activeVersion !== undefined) body.expected_version = sourcesState.activeVersion;
  try {
    const r = (await apiPost('/api/v1/sources/ontology', body)).data;
    sourcesMsg(r.saved ? 'Published — ontology version ' + r.version + '.' : 'Nothing to change — the ontology already matches.');
    sourcesState.merges = [];
    sourcesState.fingerprints = null;
    // what was just published is now the ontology's, not pending: the next preview reports it
    if (r.saved) { sourcesState.identities = {}; sourcesState.links = []; sourcesState.assertions = []; }
    if (r.saved && document.getElementById('panel-ontology')) loadOntologyPanel(true);
  } catch (e) {
    sourcesMsg(/schema changed/.test(e.message) ? 'The schema changed since the preview — preview again.' : e.message);
  }
}
async function importSource(name) {
  const picked = [...document.querySelectorAll('.imp-pick:checked')].filter(e => e.dataset.source === name).map(e => e.dataset.name);
  if (!picked.length) { sourcesMsg('Check at least one table to import.'); return; }
  const out = document.getElementById('sources-result');
  out.innerHTML = '<div class="loading">Importing...</div>';
  try {
    const r = (await apiPost('/api/v1/sources/import', sourcesBody({source: name, containers: picked}))).data;
    const lines = Object.keys(r.imported).map(c => c + ': ' + r.imported[c] + ' written, ' + r.unchanged[c] + ' unchanged' + (r.truncated.indexOf(c) >= 0 ? ' (row cap reached)' : ''));
    r.skipped.forEach(s => lines.push(s.container + ': skipped — ' + s.reason));
    out.innerHTML = '<div class="card">'+lines.map(l => '<div>'+esc(l)+'</div>').join('')+'</div>';
    sourcesMsg('Import finished.');
  } catch (e) {
    out.innerHTML = '<div class="card" style="border-color:var(--red);">'+esc(e.message)+'</div>';
  }
}
// Panel: Ask (grounded answers; POST /api/v1/chat)
// ═══════════════════════════════════════════
async function loadAskPanel() {
  const off = document.getElementById('ask-disabled'), form = document.getElementById('ask-form'), model = document.getElementById('ask-model');
  try {
    const st = (await api('/api/v1/chat/status')).data;
    off.style.display = st.enabled ? 'none' : '';
    form.style.display = st.enabled ? '' : 'none';
    model.textContent = st.enabled ? 'Model: ' + st.model + (st.reachable === false ? ' — endpoint not reachable' : '') : '';
  } catch (e) {
    off.textContent = 'Could not read the chat status: ' + e.message;
    off.style.display = ''; form.style.display = 'none'; model.textContent = '';
  }
}
function renderAskResult(r) {
  const ok = r.grounded && !r.refused;
  return '<div class="card" id="ask-answer">'
    + '<span class="badge '+esc(ok ? 'badge-green' : 'badge-orange')+'">'+esc(ok ? 'grounded' : 'refused')+'</span>'
    + '<div style="margin-top:8px;white-space:pre-wrap;">'+esc(r.answer)+'</div>'
    + '<details style="margin-top:10px;font-size:11px;"><summary style="cursor:pointer;color:var(--muted);">How I answered</summary>'
    + '<div style="margin-top:6px;">Query: <code>'+esc(r.query || 'none (the model gave no usable query)')+'</code></div>'
    + '<div>Facts from: '+esc(r.context_source)+' · model '+esc(r.model)+' · '+esc(r.ms)+' ms · '+esc((r.retrieved || []).length)+' fact(s)</div>'
    + '<table style="margin-top:6px;font-size:11px;border-collapse:collapse;">'
    + (r.retrieved || []).map((koid, i) => '<tr><td class="mono" style="padding:2px 8px 2px 0;"><a href="#" data-koid="'+esc(koid)+'" data-click="openInInspector" style="color:var(--accent);">'+esc(koid.substring(0, 16))+'...</a></td><td>'+esc((r.statements || [])[i])+'</td></tr>').join('')
    + '</table></details></div>';
}
async function askQuestion() {
  const box = document.getElementById('ask-question'), btn = document.getElementById('ask-submit'), out = document.getElementById('ask-result');
  if (btn.disabled) return;
  const question = box.value.trim();
  if (!question) { out.innerHTML = '<div class="empty-state">Type a question first.</div>'; return; }
  const tenant = document.getElementById('ask-tenant').value;
  btn.disabled = true;
  out.innerHTML = '<div class="loading">Thinking...</div>';
  try {
    const r = (await apiPost('/api/v1/chat', tenant ? {question, tenant} : {question})).data;
    out.innerHTML = renderAskResult(r);
  } catch (e) {
    const msg = e.message === 'Failed to fetch' ? 'Could not reach the server.' : e.message;
    out.innerHTML = '<div class="card" style="border-color:var(--red);">'+esc(msg)+'</div>';
  } finally {
    btn.disabled = false;
  }
}
document.getElementById('ask-question').addEventListener('keydown', e => { if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') askQuestion(); });

// ═══════════════════════════════════════════
// Event delegation — the page has NO inline handlers or inline scripts, so the
// CSP can say script-src 'self'. Markup names an action with data-click /
// data-change / data-input; values travel in data-* attributes (never in code).
// ACTIONS is the closed allowlist: a name that is not a key does nothing, and
// a unit test fails the build if the markup uses a name that is missing here.
// An action returning false cancels the default (links).
// ═══════════════════════════════════════════
const ACTIONS = Object.assign(Object.create(null), {
  doLogin: el => doLogin(),
  toggleNav: el => toggleNav(),
  closeNav: el => closeNav(),
  cycleTheme: el => cycleTheme(),
  setTenantContext: el => setTenantContext(el),
  closePalette: el => closePalette(),
  paletteRun: el => runPaletteItem(Number(el.dataset.index)),
  sortResults: el => sortResults(el),
  copyResults: el => copyResults(el),
  downloadResults: el => downloadResults(el),
  saveQuery: el => saveQuery(),
  loadQuery: el => loadQuery(el),
  deleteSaved: el => deleteSaved(el),
  clearHistory: el => clearHistory(),
  dismissToast: el => el.parentElement.remove(),
  skipToContent: el => { const c = document.getElementById('panel-content'); c.setAttribute('tabindex', '-1'); c.focus(); },
  logout: el => logout(),
  runQuery: el => runQuery(),
  explainQuery: el => explainQuery(),
  streamQuery: el => streamQuery(),
  loadGraph: el => { graphDocView = null; return loadGraph(); },
  loadGraphAll: el => loadGraphAll(),
  renderExplorerTree: el => renderExplorerTree(),
  saveOntology: el => saveOntology(),
  loadOntologyPanel: el => loadOntologyPanel(true),
  discoverOntology: el => discoverOntology(),
  discoverOntologySave: el => discoverOntology(true),
  inspectorSearch: el => inspectorSearch(),
  profileRun: el => profileRun(),
  profileExplain: el => profileExplain(),
  deployConnector: el => deployConnector(),
  ingestDocument: el => ingestDocument(),
  createUser: el => createUser(),
  loadTimeline: el => loadTimeline(),
  loadProvenance: el => loadProvenance(),
  verifyProvenance: el => verifyProvenance(),
  loadProgramDebugger: el => loadProgramDebugger(),
  loadBenchmarks: el => loadBenchmarks(),
  showBenchmarkDeployForm: el => showBenchmarkDeployForm(),
  deployBenchmark: el => deployBenchmark(),
  adminCreateBackup: el => adminCreateBackup(),
  loadAuditReport: el => loadAuditReport(),
  loadComplianceReport: el => loadComplianceReport(),
  downloadAuditExport: el => downloadAuditExport(),
  closeDetail: el => el.parentElement.parentElement.remove(),
  exploreType: el => exploreType(el.dataset.type),
  exploreTypeTenant: el => exploreTypeTenant(el.dataset.type, el.dataset.tenant),
  openInInspector: el => openInInspector(el.dataset.koid),
  inspectorLoadKOID: el => inspectorLoadKOID(el.dataset.koid),
  adminVerifyBackup: el => adminVerifyBackup(el.dataset.name),
  adminRestoreBackup: el => adminRestoreBackup(el.dataset.name),
  setUserRoles: el => setUserRoles(el.dataset.sub),
  loadProgramDeps: el => loadProgramDeps(el.dataset.koid),
  runBenchmark: el => runBenchmark(el.dataset.koid),
  compileDocument: el => compileDocument(el.dataset.koid),
  saveDocument: el => saveDocument(el.dataset.koid),
  removeDocumentKnowledge: el => removeDocumentKnowledge(el.dataset.koid),
  viewDocumentGraph: el => viewDocumentGraph(el.dataset.koid),
  graphKind: el => graphKind(el.dataset.kind),
  graphPage: el => graphPage(Number(el.dataset.dir)),
  graphPickDocument: el => graphPickDocument(el.value),
  browseDocument: el => browseDocument(el.dataset.koid),
  browseKind: el => browseKind(el.dataset.kind),
  browseSearch: el => browseSearch(),
  browsePage: el => browsePage(Number(el.dataset.dir)),
  browseClose: el => { browse.koid = null; return renderBrowse(); },
  loadSourcesPanel: el => loadSourcesPanel(),
  discoverSource: el => discoverSource(el.dataset.name),
  previewSourceOntology: el => previewSources(),
  mergeSelectedClasses: el => mergeSelectedClasses(),
  applySuggestion: el => applySuggestion(Number(el.dataset.index)),
  toggleIdentity: el => toggleIdentity(el),
  acceptLink: el => acceptLink(Number(el.dataset.index)),
  removeLink: el => removeLink(Number(el.dataset.index)),
  addAssertion: el => addAssertion(),
  removeAssertion: el => removeAssertion(Number(el.dataset.index)),
  resolveClass: el => resolveClass(),
  applySourceOntology: el => applySourceOntology(),
  importSource: el => importSource(el.dataset.name),
  askQuestion: el => askQuestion(),
  signInSso: el => location.href = '/api/login/sso',
});
function dispatchAction(attr, ev) {
  const el = ev.target instanceof Element ? ev.target.closest('[' + attr + ']') : null;
  if (!el) return;
  const fn = ACTIONS[el.getAttribute(attr)];
  if (fn && fn(el, ev) === false) ev.preventDefault();
}
document.addEventListener('click', ev => dispatchAction('data-click', ev));
document.addEventListener('change', ev => dispatchAction('data-change', ev));
document.addEventListener('input', ev => dispatchAction('data-input', ev));


// SSO callback hand-off: the IdP redirect carries the session in the URL
// fragment (never sent to a server or a Referer); the query-string read only
// keeps a bookmarked pre-fragment hand-off working. Runs only when the SSO
// button was rendered, i.e. OIDC is licensed and configured.
window.addEventListener('load', () => {
  if (!document.querySelector('[data-sso-button]')) return;
  const q = new URLSearchParams(location.hash ? location.hash.slice(1) : location.search);
  const t = q.get('token');
  if (!t) return;
  authToken = t;
  currentUser = q.get('user') || 'sso';
  document.getElementById('user-badge').textContent = currentUser + (currentUser === 'admin' ? ' (admin)' : '');
  document.getElementById('login-overlay').style.display = 'none';
  document.getElementById('app').style.display = 'flex';
  initApp();
});
