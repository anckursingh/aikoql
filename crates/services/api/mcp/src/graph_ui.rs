//! Graph Browser UI — Neo4j-style visualization with query runner + auth.
//!
//! Served at `/ui`. Features: tenant-filtered graph, interactive vis-network,
//! aikoql query editor with results table, login/auth with role-based access.

pub const GRAPH_UI_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Aikoql Graph Browser</title>
<script src="https://cdn.jsdelivr.net/npm/vis-network@9.1.2/dist/vis-network.min.js"></script>
<style>
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; background: #0f0f14; color: #e0e0e0; height: 100vh; display: flex; }

/* Login overlay */
#login-overlay { position: fixed; inset: 0; background: rgba(5,5,10,0.95); display: flex; align-items: center; justify-content: center; z-index: 100; }
#login-box { background: #1a1a24; border: 1px solid #3a3a4a; border-radius: 8px; padding: 30px 36px; width: 340px; text-align: center; }
#login-box h2 { color: #8be9fd; margin-bottom: 20px; font-size: 18px; }
#login-box input { width: 100%; padding: 10px 14px; margin-bottom: 10px; background: #0f0f18; border: 1px solid #3a3a4a; color: #e0e0e0; border-radius: 4px; font-size: 13px; }
#login-box input:focus { outline: none; border-color: #8be9fd; }
#login-box button { width: 100%; padding: 10px; background: #8be9fd; color: #0f0f14; border: none; border-radius: 4px; cursor: pointer; font-weight: 600; }
#login-box .error { color: #ff5555; font-size: 12px; margin-top: 8px; }
#login-box .hint { color: #6272a4; font-size: 11px; margin-top: 12px; }

/* Sidebar */
#sidebar { width: 380px; min-width: 380px; background: #1a1a24; border-right: 1px solid #2a2a3a; display: flex; flex-direction: column; overflow: hidden; }
#sidebar h2 { padding: 12px 16px; font-size: 14px; background: #222230; border-bottom: 1px solid #2a2a3a; color: #8be9fd; display: flex; justify-content: space-between; align-items: center; }
#sidebar h2 span { font-size: 10px; color: #6272a4; }
#toolbar { padding: 10px 14px; border-bottom: 1px solid #2a2a3a; display: flex; flex-direction: column; gap: 5px; }
#toolbar input, #toolbar select { padding: 6px 10px; background: #0f0f18; border: 1px solid #3a3a4a; color: #e0e0e0; border-radius: 4px; font-size: 12px; }
#toolbar input:focus, #toolbar select:focus { outline: none; border-color: #8be9fd; }
#toolbar .btn-row { display: flex; gap: 4px; }
#toolbar button { flex: 1; padding: 5px 8px; border: none; border-radius: 4px; cursor: pointer; font-weight: 600; font-size: 11px; }
.btn-primary { background: #8be9fd; color: #0f0f14; }
.btn-secondary { background: #44475a; color: #e0e0e0; }
.btn-danger { background: #ff5555; color: #fff; }

/* Tabs */
#tabs { display: flex; border-bottom: 1px solid #2a2a3a; }
#tabs button { flex: 1; padding: 8px; background: none; border: none; color: #6272a4; cursor: pointer; font-size: 12px; border-bottom: 2px solid transparent; }
#tabs button.active { color: #8be9fd; border-bottom-color: #8be9fd; }

/* Tab content */
.tab-content { display: none; flex: 1; overflow-y: auto; }
.tab-content.active { display: flex; flex-direction: column; }

/* Inspector tab */
#info { padding: 12px 14px; font-size: 12px; overflow-y: auto; flex: 1; }
#info .tenant-badge { display: inline-block; background: #bd93f9; color: #0f0f14; padding: 2px 8px; border-radius: 3px; font-size: 10px; font-weight: 600; margin-bottom: 6px; margin-right: 4px; }
#info h3 { font-size: 14px; color: #f0f0f0; margin: 8px 0 4px; }
#info .prop-row { display: flex; padding: 2px 0; border-bottom: 1px solid #1e1e2a; }
#info .prop-key { color: #6272a4; width: 85px; flex-shrink: 0; }
#info .prop-val { color: #f8f8f2; word-break: break-all; }
#info .empty { color: #44475a; font-style: italic; padding: 20px 0; text-align: center; }

/* Query tab */
#query-panel { padding: 10px 14px; flex: 1; display: flex; flex-direction: column; }
#query-panel textarea { flex: 1; min-height: 120px; background: #0f0f18; border: 1px solid #3a3a4a; color: #e0e0e0; border-radius: 4px; padding: 10px; font-family: 'Consolas', 'Monaco', monospace; font-size: 12px; resize: vertical; }
#query-panel textarea:focus { outline: none; border-color: #8be9fd; }
#query-run-btn { margin-top: 8px; padding: 8px; background: #50fa7b; color: #0f0f14; border: none; border-radius: 4px; cursor: pointer; font-weight: 600; font-size: 12px; }
#query-results { margin-top: 8px; overflow-y: auto; flex: 1; font-size: 11px; }
#query-results table { width: 100%; border-collapse: collapse; }
#query-results th { background: #222230; color: #8be9fd; padding: 4px 8px; text-align: left; font-size: 11px; position: sticky; top: 0; }
#query-results td { padding: 3px 8px; border-bottom: 1px solid #1e1e28; max-width: 160px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
#query-results .error { color: #ff5555; padding: 8px; }

/* Stats bar */
#stats { padding: 6px 14px; border-top: 1px solid #2a2a3a; font-size: 11px; color: #6272a4; display: flex; justify-content: space-between; }
#user-badge { color: #bd93f9; cursor: pointer; }

/* Graph area */
#graph-container { flex: 1; position: relative; }
#graph { width: 100%; height: 100%; }
.legend { position: absolute; bottom: 8px; left: 8px; background: rgba(26,26,36,0.92); padding: 5px 10px; border-radius: 4px; font-size: 10px; max-height: 280px; overflow-y: auto; }
.legend-item { display: flex; align-items: center; margin: 2px 0; }
.legend-dot { width: 8px; height: 8px; border-radius: 50%; margin-right: 5px; flex-shrink: 0; }
.tooltip-box { position: absolute; top: 8px; right: 8px; background: rgba(26,26,36,0.95); border: 1px solid #3a3a4a; border-radius: 6px; padding: 8px 12px; font-size: 11px; max-width: 260px; display: none; z-index: 10; }
.tooltip-box .tt-type { color: #8be9fd; font-weight: 600; }
.tooltip-box .tt-tenant { color: #bd93f9; font-size: 10px; }
.tooltip-box .tt-prop { color: #6272a4; }
</style>
</head>
<body>

<!-- Login overlay -->
<div id="login-overlay">
  <div id="login-box">
    <h2>aikoql</h2>
    <input type="text" id="login-user" placeholder="Username" value="admin" />
    <input type="password" id="login-pass" placeholder="Password" value="admin" />
    <button onclick="doLogin()">Sign In</button>
    <div class="error" id="login-error"></div>
    <div class="hint">Default: admin / admin (full access)<br>Read-only: user / user</div>
  </div>
</div>

<!-- Main layout (hidden until login) -->
<div id="main-layout" style="display:none;height:100vh;width:100vw;display:none;">
<div id="sidebar">
  <h2>Aikoql Graph <span id="version-tag"></span></h2>
  <div id="toolbar">
    <input type="text" id="koid-input" placeholder="KOID hex or type name..." />
    <select id="tenant-select" onchange="loadGraph()">
      <option value="">All tenants</option>
    </select>
    <div class="btn-row">
      <button class="btn-primary" onclick="loadGraph()">Search</button>
      <button class="btn-secondary" onclick="loadAll()">View All</button>
    </div>
  </div>
  <div id="tabs">
    <button class="active" onclick="switchTab('inspector')">Inspector</button>
    <button onclick="switchTab('query')">aikoql</button>
    <button onclick="switchTab('schema')">Schema</button>
  </div>
  <div id="tab-inspector" class="tab-content active">
    <div id="info">
      <p class="empty">Click a node to inspect it.<br>Double-click to re-center.</p>
    </div>
  </div>
  <div id="tab-query" class="tab-content">
    <div id="query-panel">
      <textarea id="query-text" placeholder="MATCH Person RETURN *&#10;CREATE Note body == &quot;hello&quot;&#10;MATCH note WHERE body == &quot;hello&quot; RETURN *">MATCH Person RETURN *</textarea>
      <button id="query-run-btn" onclick="runQuery()">▶ Run Query</button>
      <button style="margin-top:4px;padding:6px;background:#44475a;color:#e0e0e0;border:none;border-radius:4px;cursor:pointer;font-size:11px;width:100%;" onclick="explainQuery()">🔍 Explain Plan</button>
      <div id="query-results"></div>
    </div>
  </div>
  <div id="tab-schema" class="tab-content">
    <div id="schema-panel" style="padding:10px 14px;flex:1;overflow-y:auto;font-size:12px;">
      <p class="empty">Loading schema...</p>
    </div>
  </div>
  <div id="stats">
    <span id="stats-left"></span>
    <span id="user-badge" onclick="logout()"></span>
    <span id="stats-right"></span>
  </div>
</div>
<div id="graph-container">
  <div id="graph"></div>
  <div class="legend" id="legend"></div>
  <div class="tooltip-box" id="tooltip"></div>
</div>
</div>

<script>
// ---- Auth state ----
let authToken = null;
let currentUser = null;

function showLogin() {
  document.getElementById('login-overlay').style.display = 'flex';
  document.getElementById('main-layout').style.display = 'none';
  authToken = null;
  currentUser = null;
}

async function doLogin() {
  const u = document.getElementById('login-user').value.trim();
  const p = document.getElementById('login-pass').value.trim();
  try {
    const res = await fetch('/api/login', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({username: u, password: p})
    });
    const data = await res.json();
    if (data.token) {
      authToken = data.token;
      currentUser = u;
      document.getElementById('login-overlay').style.display = 'none';
      document.getElementById('main-layout').style.display = 'flex';
      document.getElementById('user-badge').textContent = u + (u === 'admin' ? ' (admin)' : '');
      initGraph();
      loadAll();
    } else {
      document.getElementById('login-error').textContent = data.error || 'Login failed';
    }
  } catch(e) {
    document.getElementById('login-error').textContent = 'Connection error: ' + e.message;
  }
}

function logout() {
  showLogin();
  document.getElementById('login-user').value = '';
  document.getElementById('login-pass').value = '';
}

// ---- Tab switching ----
function switchTab(name) {
  document.querySelectorAll('#tabs button').forEach((b,i) => {
    b.classList.toggle('active', b.textContent.toLowerCase().includes(name));
  });
  document.querySelectorAll('.tab-content').forEach(t => t.classList.remove('active'));
  document.getElementById('tab-' + name).classList.add('active');
}

// ---- Graph ----
const COLORS = ['#8be9fd','#ff79c6','#50fa7b','#ffb86c','#bd93f9','#ff5555','#f1fa8c','#6be5c1','#ff92d0','#a6e3a1'];
let typeColors = {};
let colorIdx = 0;
function colorFor(type) { if(!typeColors[type]) typeColors[type]=COLORS[colorIdx++%COLORS.length]; return typeColors[type]; }

let network = null;
let nodesData = new vis.DataSet([]);
let edgesData = new vis.DataSet([]);
let currentData = null;

function initGraph() {
  if (network) return;
  const container = document.getElementById('graph');
  network = new vis.Network(container, {nodes: nodesData, edges: edgesData}, {
    physics: { solver: 'forceAtlas2Based', forceAtlas2Based: { gravitationalConstant: -35, centralGravity: 0.008, springLength: 160, springConstant: 0.03 } },
    edges: { arrows: { to: { enabled: true, scaleFactor: 0.5 } }, smooth: { type: 'continuous', roundness: 0.3 }, font: { size: 9, color: '#aaaaaa', strokeWidth: 0 }, width: 1.5 },
    nodes: { shape: 'dot', font: { size: 11, color: '#e8e8e8', face: 'sans-serif', strokeWidth: 1, strokeColor: '#1a1a24' }, borderWidth: 1.5, shadow: {enabled: true, size: 6} },
    interaction: { hover: true, navigationButtons: true, keyboard: true },
  });
  network.on('click', p => { if(p.nodes.length>0) loadNodeDetail(p.nodes[0]); else document.getElementById('info').innerHTML='<p class="empty">Click a node to inspect it.</p>'; });
  network.on('doubleClick', p => { if(p.nodes.length>0) { document.getElementById('koid-input').value=p.nodes[0]; loadGraph(); } });
  network.on('hoverNode', p => showTooltip(p.node));
  network.on('blurNode', () => document.getElementById('tooltip').style.display='none');
}

function showTooltip(nodeId) {
  if(!currentData) return;
  const node = currentData.nodes.find(n=>n.koid===nodeId);
  if(!node) return;
  const tt = document.getElementById('tooltip');
  let h = '<div class="tt-type">'+node.type_name+'</div>';
  if(node.tenant) h += '<div class="tt-tenant">@'+node.tenant+'</div>';
  if(node.key_props) for(const p of node.key_props.slice(0,5)) h += '<div class="tt-prop">'+p.key+': '+JSON.stringify(p.value)+'</div>';
  tt.innerHTML = h;
  tt.style.display = 'block';
}

function loadAll() { document.getElementById('koid-input').value=''; document.getElementById('tenant-select').value=''; loadGraph(); }

async function apiFetch(url) {
  const sep = url.includes('?') ? '&' : '?';
  return fetch(url + (authToken ? sep + 'token=' + encodeURIComponent(authToken) : ''));
}

async function loadGraph() {
  const koid = document.getElementById('koid-input').value.trim();
  const tenant = document.getElementById('tenant-select').value;
  let url = '/api/graph';
  let params = [];
  if(koid) params.push('koid='+encodeURIComponent(koid));
  if(tenant) params.push('tenant='+encodeURIComponent(tenant));
  if(params.length) url += '?'+params.join('&');
  try {
    const res = await apiFetch(url);
    const data = await res.json();
    currentData = data;
    renderGraph(data);
    updateTenantDropdown(data);
    document.getElementById('version-tag').textContent = data.nodes.length+' objects';
  } catch(e) {
    document.getElementById('info').innerHTML = '<p style="color:#ff5555;">Error: '+e.message+'</p>';
  }
}

function updateTenantDropdown(data) {
  const sel = document.getElementById('tenant-select');
  const cv = sel.value;
  sel.innerHTML = '<option value="">All tenants</option>';
  if(data.tenants) for(const t of data.tenants) sel.innerHTML += '<option value="'+t+'"'+(t===cv?' selected':'')+'>'+t+'</option>';
}

function renderGraph(data) {
  nodesData.clear(); edgesData.clear(); typeColors={}; colorIdx=0;
  const nodes = [];
  for(const n of data.nodes) {
    const sz = n.size || 20;
    // Label: type + tenant badge + name
    let lbl = n.label || n.type_name;
    if (n.tenant) lbl += '\n@' + n.tenant;
    nodes.push({
      id: n.koid, label: lbl,
      title: '<b>'+n.type_name+'</b>' + (n.tenant?' @'+n.tenant:'') + '\n'+n.koid,
      color: { background: colorFor(n.type_name), border: (n.tenant?'#bd93f9':'#1a1a24'), highlight:{background:colorFor(n.type_name),border:'#fff'} },
      size: sz, borderWidth: n.tenant?2.5:(n.edge_count>2?3:1.5),
      font: { size: Math.max(9, Math.min(13, sz/2+3)) },
    });
  }
  nodesData.add(nodes);
  const edg = [];
  for(const e of data.edges) edg.push({ from:e.source, to:e.target, label:e.rel_type, arrows:'to', color:{color:'#4a4a6a',highlight:'#8be9fd'}, font:{size:9,color:'#6272a4'}, width:1.2 });
  edgesData.add(edg);

  let lh = '';
  for(const [t,c] of Object.entries(typeColors)) { const cnt = data.nodes.filter(n=>n.type_name===t).length; lh += '<div class="legend-item"><div class="legend-dot" style="background:'+c+'"></div>'+t+' ('+cnt+')</div>'; }
  document.getElementById('legend').innerHTML = lh||'<div style="color:#44475a">(no types)</div>';
  document.getElementById('stats-left').textContent = data.nodes.length+' nodes';
  document.getElementById('stats-right').textContent = data.edges.length+' edges';
  if(network&&nodes.length>0) setTimeout(()=>network.fit({animation:{duration:400}}), 150);
}

async function loadNodeDetail(koid) {
  try {
    const res = await apiFetch('/api/graph?koid='+encodeURIComponent(koid)+'&detail=1');
    const data = await res.json();
    const node = data.nodes.find(n=>n.koid===koid);
    if(!node) { document.getElementById('info').innerHTML='<p>Node not found.</p>'; return; }
    let h = '';
    if(node.tenant) h += '<span class="tenant-badge">@'+node.tenant+'</span>';
    h += '<h3>'+node.type_name+'</h3>';
    h += '<div class="prop-row"><span class="prop-key">KOID</span><span class="prop-val" style="font-size:10px">'+node.koid+'</span></div>';
    h += '<div class="prop-row"><span class="prop-key">Version</span><span class="prop-val">'+(node.version||'?')+' (schema v'+(node.schema_version||1)+')</span></div>';
    h += '<div class="prop-row"><span class="prop-key">Edges</span><span class="prop-val">'+(node.edge_count||0)+'</span></div>';
    if(node.lifecycle) {
      const lc = typeof node.lifecycle === 'object' ? node.lifecycle.state : node.lifecycle;
      h += '<div class="prop-row"><span class="prop-key">Lifecycle</span><span class="prop-val">'+lc+'</span></div>';
      if(node.lifecycle.origin) h += '<div class="prop-row"><span class="prop-key">Origin</span><span class="prop-val">'+node.lifecycle.origin+'</span></div>';
    }
    if(node.tags && node.tags.length > 0) h += '<div class="prop-row"><span class="prop-key">Tags</span><span class="prop-val">'+node.tags.join(', ')+'</span></div>';
    if(node.security) {
      h += '<div class="prop-row"><span class="prop-key">Security</span><span class="prop-val">owner='+(node.security.owner||'?')+', classification='+(node.security.classification||'none')+', acl='+(node.security.acl_count||0)+' entries</span></div>';
    }
    if(node.relationships && node.relationships.length > 0) {
      h += '<h3>Relationships ('+node.relationships.length+')</h3>';
      for(const r of node.relationships) {
        h += '<div class="prop-row"><span class="prop-key">'+r.direction+'</span><span class="prop-val">['+r.type+'] → '+r.target.substring(0,16)+'...</span></div>';
      }
    }
    if(node.event_refs) h += '<div class="prop-row"><span class="prop-key">Events</span><span class="prop-val">'+node.event_refs+' refs</span></div>';
    if(node.extensions && Object.keys(node.extensions).length > 0) {
      h += '<h3>Extensions</h3>';
      for(const [k,v] of Object.entries(node.extensions)) h += '<div class="prop-row"><span class="prop-key">'+k+'</span><span class="prop-val">'+JSON.stringify(v)+'</span></div>';
    }
    if(node.properties) {
      h += '<h3>Properties</h3>';
      for(const [k,v] of Object.entries(node.properties)) h += '<div class="prop-row"><span class="prop-key">'+k+'</span><span class="prop-val">'+JSON.stringify(v)+'</span></div>';
    }
    h += '<p style="margin-top:8px;font-size:10px;"><a href="#" onclick="document.getElementById(\'koid-input\').value=\''+node.koid+'\';loadGraph();return false" style="color:#8be9fd;">Center in graph</a></p>';
    document.getElementById('info').innerHTML = h;
  } catch(e) { document.getElementById('info').innerHTML='<p style="color:#ff5555;">'+e.message+'</p>'; }
}

// ---- Aikoql Query Runner ----
async function runQuery() {
  const query = document.getElementById('query-text').value.trim();
  if(!query) return;
  const resDiv = document.getElementById('query-results');
  resDiv.innerHTML = '<div style="color:#6272a4;">Running...</div>';
  // Pass tenant filter from the dropdown so created objects are tagged.
  const tenant = document.getElementById('tenant-select').value;
  let url = '/api/aikoql?query=' + encodeURIComponent(query);
  if (tenant) url += '&tenant=' + encodeURIComponent(tenant);
  try {
    const res = await apiFetch(url);
    const data = await res.json();
    if (data.error) { resDiv.innerHTML = '<div class="error">'+data.error+'</div>'; return; }
    if (data.created) { resDiv.innerHTML = '<div style="color:#50fa7b;">Created: '+data.created+' (v'+data.version+')</div>'; loadGraph(); return; }
    if (data.results && data.results.length > 0) {
      const keys = Object.keys(data.results[0]).filter(k => k !== 'properties');
      if (data.results[0].properties) keys.push(...Object.keys(data.results[0].properties));
      let html = '<table><thead><tr>';
      for(const k of [...new Set(keys)]) html += '<th>'+k+'</th>';
      html += '</tr></thead><tbody>';
      for(const row of data.results) {
        html += '<tr>';
        for(const k of [...new Set(keys)]) {
          let v = row[k];
          if (v === undefined && row.properties) v = row.properties[k];
          html += '<td title="'+String(v||'')+'">'+(v !== undefined ? String(v).substring(0,60) : '')+'</td>';
        }
        html += '</tr>';
      }
      html += '</tbody></table>';
      resDiv.innerHTML = html;
    } else {
      resDiv.innerHTML = '<div style="color:#6272a4;">(0 results)</div>';
    }
  } catch(e) { resDiv.innerHTML = '<div class="error">'+e.message+'</div>'; }
}

// ---- Schema Browser ----
async function loadSchema() {
  const panel = document.getElementById('schema-panel');
  panel.innerHTML = '<div style="color:#6272a4;">Loading...</div>';
  try {
    const res = await apiFetch('/api/schema');
    const data = await res.json();
    if (data.error) { panel.innerHTML = '<div class="error">'+data.error+'</div>'; return; }
    let html = '<div style="color:#8be9fd;margin-bottom:8px;">'+data.total_types+' types</div>';
    for (const [typeName, info] of Object.entries(data.schema || {})) {
      html += '<div style="background:#1e1e2a;border-radius:4px;padding:8px 10px;margin-bottom:6px;">';
      html += '<div style="font-weight:600;color:#f8f8f2;margin-bottom:4px;">'+typeName+' <span style="color:#6272a4;font-weight:normal;">('+info.count+' objects)</span></div>';
      if (info.tenants && info.tenants.length > 0) {
        html += '<div style="color:#bd93f9;font-size:10px;margin-bottom:3px;">Tenants: '+info.tenants.join(', ')+'</div>';
      }
      if (info.properties && info.properties.length > 0) {
        html += '<div style="color:#6272a4;font-size:11px;">Properties: ';
        html += info.properties.map(p => '<span style="color:#50fa7b;">'+p+'</span>').join(', ');
        html += '</div>';
      } else {
        html += '<div style="color:#44475a;font-size:11px;">(no properties discovered)</div>';
      }
      html += '</div>';
    }
    panel.innerHTML = html || '<div class="empty">No types found. Create objects to populate the schema.</div>';
  } catch(e) { panel.innerHTML = '<div class="error">'+e.message+'</div>'; }
}

// ---- Query Explain ----
async function explainQuery() {
  const query = document.getElementById('query-text').value.trim();
  if (!query) return;
  const resDiv = document.getElementById('query-results');
  resDiv.innerHTML = '<div style="color:#6272a4;">Explaining...</div>';
  try {
    const res = await apiFetch('/api/explain?query=' + encodeURIComponent(query));
    const data = await res.json();
    if (data.error) { resDiv.innerHTML = '<div class="error">'+data.error+'</div>'; return; }
    let html = '<div style="color:#8be9fd;margin-bottom:4px;">Query Plan ('+data.operator_count+' operators)</div>';
    html += '<div style="color:#6272a4;font-size:11px;margin-bottom:6px;">'+data.query+'</div>';
    html += '<table><thead><tr><th>#</th><th>Operator</th></tr></thead><tbody>';
    data.operators.forEach((op, i) => { html += '<tr><td style="color:#6272a4;">'+(i+1)+'</td><td style="font-family:monospace;font-size:11px;">'+op+'</td></tr>'; });
    html += '</tbody></table>';
    resDiv.innerHTML = html;
  } catch(e) { resDiv.innerHTML = '<div class="error">'+e.message+'</div>'; }
}

// Init
document.addEventListener('DOMContentLoaded', function() {
  showLogin();
  document.getElementById('login-pass').addEventListener('keydown', e => { if(e.key==='Enter') doLogin(); });
  document.getElementById('koid-input').addEventListener('keydown', e => { if(e.key==='Enter') loadGraph(); });
  document.getElementById('query-text').addEventListener('keydown', e => { if(e.ctrlKey && e.key==='Enter') runQuery(); });
  // Load schema when switching to Schema tab.
  document.querySelectorAll('#tabs button').forEach(b => {
    if (b.textContent.includes('Schema')) b.addEventListener('click', loadSchema);
  });
});
</script>
</body>
</html>"##;
