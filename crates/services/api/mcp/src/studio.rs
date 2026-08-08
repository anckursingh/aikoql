//! Mnemosyne Studio — The Knowledge OS Desktop.
//!
//! Served at `/studio`. Single-page application: sidebar navigation + 6 panels.
//! Every panel uses the existing REST API — zero new backend endpoints.
//!
//! ponytail: one HTML file, no framework, no build step, no npm.
//! CodeMirror 6 via esm.sh CDN (3 imports, ESM, cached).

pub const STUDIO_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Mnemosyne Studio</title>
<script src="https://cdn.jsdelivr.net/npm/vis-network@9.1.2/dist/vis-network.min.js"></script>
<script type="module">
import {EditorView, basicSetup} from "https://esm.sh/codemirror@6.0.1"
import {sql, PostgreSQL} from "https://esm.sh/@codemirror/lang-sql@6.2.0"
import {oneDark} from "https://esm.sh/@codemirror/theme-one-dark@6.1.2"
import {autocompletion} from "https://esm.sh/@codemirror/autocomplete@6.0.0"

// ── AIKOQL keyword completions ──
const AIKOQL_KEYWORDS = [
  // Statements
  {label:"CREATE",type:"keyword",detail:"Create a Knowledge Object",boost:3},
  {label:"MATCH",type:"keyword",detail:"Query Knowledge Objects",boost:3},
  {label:"RETURN",type:"keyword",detail:"Return clause (required)",boost:4},
  {label:"TRAVERSE",type:"keyword",detail:"Walk relationships",boost:2},
  {label:"UPDATE",type:"keyword",detail:"Update a KO",boost:2},
  {label:"DELETE",type:"keyword",detail:"Delete/deactivate a KO",boost:1},
  {label:"INGEST",type:"keyword",detail:"Ingest external data",boost:1},
  // Clauses
  {label:"WHERE",type:"keyword",detail:"Filter condition",boost:2},
  {label:"AND",type:"keyword",detail:"Logical AND",boost:1},
  {label:"OR",type:"keyword",detail:"Logical OR",boost:1},
  {label:"NOT",type:"keyword",detail:"Logical NOT",boost:1},
  {label:"ORDER BY",type:"keyword",detail:"Sort results",boost:1},
  {label:"LIMIT",type:"keyword",detail:"Max results",boost:1},
  {label:"OFFSET",type:"keyword",detail:"Skip results",boost:1},
  {label:"DEPENDS_ON",type:"keyword",detail:"Dependency relationship",boost:1},
  {label:"CONTAINS",type:"keyword",detail:"Container relationship",boost:1},
  {label:"USES",type:"keyword",detail:"Usage relationship",boost:1},
  {label:"GOVERNED_BY",type:"keyword",detail:"Governance relationship",boost:1},
  {label:"DEPTH",type:"keyword",detail:"Traversal depth",boost:1},
  {label:"FUSION",type:"keyword",detail:"Search fusion mode (exact/vector/text/hybrid)",boost:1},
  {label:"INBOUND",type:"keyword",detail:"Inbound direction",boost:1},
  {label:"OUTBOUND",type:"keyword",detail:"Outbound direction",boost:1},
  {label:"BOTH",type:"keyword",detail:"Both directions",boost:1},
  // Operators
  {label:"==",type:"operator",detail:"Equal",boost:1},
  {label:"!=",type:"operator",detail:"Not equal",boost:1},
  {label:">",type:"operator",detail:"Greater than",boost:1},
  {label:"<",type:"operator",detail:"Less than",boost:1},
  // Lifecycle
  {label:"ACTIVE",type:"keyword",detail:"Active lifecycle"},
  {label:"DRAFT",type:"keyword",detail:"Draft lifecycle"},
  {label:"ARCHIVED",type:"keyword",detail:"Archived lifecycle"},
  // Values
  {label:"true",type:"keyword",detail:"Boolean"},
  {label:"false",type:"keyword",detail:"Boolean"},
  {label:"null",type:"keyword",detail:"Null"},
  // mnemosyne: types
  {label:"mnemosyne:program",type:"type",detail:"Program KO"},
  {label:"mnemosyne:workflow",type:"type",detail:"Workflow KO"},
  {label:"mnemosyne:policy",type:"type",detail:"Policy KO"},
  {label:"mnemosyne:agent",type:"type",detail:"Agent KO"},
  {label:"mnemosyne:trigger",type:"type",detail:"Trigger KO"},
  {label:"mnemosyne:connector",type:"type",detail:"Connector KO"},
  {label:"mnemosyne:benchmark",type:"type",detail:"Benchmark KO"},
  {label:"mnemosyne:view",type:"type",detail:"Materialized view KO"},
  {label:"mnemosyne:report",type:"type",detail:"Report KO"},
  {label:"mnemosyne:ontology",type:"type",detail:"Ontology KO"},
  {label:"mnemosyne:memory",type:"type",detail:"Agent memory"},
  {label:"mnemosyne:query",type:"type",detail:"Saved query KO"},
];
// Dynamic type names — populated from /api/schema after login.
window.__schemaTypes = [];
function aiokoqlCompletions(context) {
  const word = context.matchBefore(/[\w:]*/);
  if (!word || (word.from === word.to && !context.explicit)) return null;
  const text = word.text.toLowerCase();
  let options = AIKOQL_KEYWORDS.filter(k => k.label.toLowerCase().startsWith(text));
  // Add dynamic type names from schema (lower priority).
  for (const t of (window.__schemaTypes || [])) {
    if (t.toLowerCase().startsWith(text) && !options.find(o => o.label === t)) {
      options.push({label: t, type: "type", detail: "Knowledge Object type", boost: 0});
    }
  }
  if (!options.length) return null;
  return {from: word.from, options, filter: false};
}

// Expose init function globally so non-module JS can call it after login.
window.__initCodeMirror = function() {
  const el = document.getElementById('query-editor');
  if (!el || window.__cmView) return;
  window.__cmView = new EditorView({
    doc: '',
    extensions: [
      basicSetup,
      sql({ dialect: PostgreSQL }),
      oneDark,
      autocompletion({ override: [aiokoqlCompletions] }),
      EditorView.updateListener.of(update => {
        if (update.docChanged) {
          const q = update.state.doc.toString().trim();
          const hist = JSON.parse(localStorage.getItem('mnemosyne-query-history')||'[]');
          if (q && !hist.includes(q)) { hist.unshift(q); if (hist.length>50) hist.pop(); localStorage.setItem('mnemosyne-query-history',JSON.stringify(hist)); }
        }
      })
    ],
    parent: el
  });
  // Ctrl+Enter → run query (capture phase to beat CodeMirror's internal handlers).
  window.__cmView.dom.addEventListener('keydown', e => {
    if (e.ctrlKey && e.key === 'Enter') { e.preventDefault(); e.stopPropagation(); window.runQuery(); }
  }, true);
};
window.__cmSetValue = function(v) {
  if (!window.__cmView) return;
  const tx = window.__cmView.state.update({changes:{from:0,to:window.__cmView.state.doc.length,insert:v}});
  window.__cmView.dispatch(tx);
};
window.__cmGetValue = function() {
  return window.__cmView ? window.__cmView.state.doc.toString() : '';
};
</script>
<style>
:root {
  --bg: #0a0a12;
  --panel: #13131f;
  --border: #252538;
  --text: #c8c8d4;
  --muted: #5c5c78;
  --accent: #6cc7f0;
  --green: #5cd89c;
  --red: #f0556a;
  --purple: #b89ae0;
  --orange: #f0a060;
  --yellow: #f0d060;
  --font: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif;
  --mono: 'Cascadia Code', 'Fira Code', 'JetBrains Mono', 'Consolas', monospace;
}
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: var(--font); background: var(--bg); color: var(--text); height: 100vh; display: flex; overflow: hidden; }

/* ── Login ── */
#login-overlay { position: fixed; inset: 0; background: rgba(6,6,14,0.97); display: flex; align-items: center; justify-content: center; z-index: 200; }
.login-card { background: var(--panel); border: 1px solid var(--border); border-radius: 12px; padding: 40px 44px; width: 380px; text-align: center; box-shadow: 0 0 80px rgba(108,199,240,0.06); }
.login-card h1 { font-size: 22px; font-weight: 700; color: var(--accent); margin-bottom: 4px; letter-spacing: -0.5px; }
.login-card .sub { color: var(--muted); font-size: 12px; margin-bottom: 28px; }
.login-card input { width: 100%; padding: 11px 14px; margin-bottom: 10px; background: var(--bg); border: 1px solid var(--border); color: var(--text); border-radius: 6px; font-size: 13px; font-family: var(--font); }
.login-card input:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px rgba(108,199,240,0.12); }
.login-card button { width: 100%; padding: 11px; background: var(--accent); color: #0a0a12; border: none; border-radius: 6px; cursor: pointer; font-weight: 600; font-size: 13px; font-family: var(--font); }
.login-card button:hover { background: #7dd4f5; }
.login-card .err { color: var(--red); font-size: 12px; margin-top: 10px; min-height: 18px; }
.login-card .hint { color: var(--muted); font-size: 11px; margin-top: 16px; line-height: 1.6; }

/* ── Layout ── */
#app { display: none; height: 100vh; width: 100vw; }
#sidebar { width: 52px; min-width: 52px; background: var(--panel); border-right: 1px solid var(--border); display: flex; flex-direction: column; align-items: center; padding-top: 10px; z-index: 10; transition: width 0.18s ease; overflow: hidden; }
#sidebar:hover { width: 190px; }
#sidebar .logo { font-size: 18px; margin-bottom: 6px; opacity: 0.9; flex-shrink: 0; }
#sidebar .logo-text { display: none; font-size: 13px; font-weight: 700; color: var(--accent); white-space: nowrap; }
#sidebar:hover .logo-text { display: inline; margin-left: 10px; }
#sidebar nav { flex: 1; display: flex; flex-direction: column; gap: 2px; width: 100%; padding: 0 4px; }
#sidebar nav button { display: flex; align-items: center; width: 100%; padding: 10px 12px; background: none; border: none; color: var(--muted); cursor: pointer; font-size: 12px; font-family: var(--font); border-radius: 6px; white-space: nowrap; text-align: left; transition: all 0.1s; }
#sidebar nav button:hover { background: rgba(108,199,240,0.08); color: var(--text); }
#sidebar nav button.active { background: rgba(108,199,240,0.14); color: var(--accent); }
#sidebar nav button .ico { font-size: 16px; width: 24px; text-align: center; flex-shrink: 0; }
#sidebar nav button .lbl { margin-left: 10px; opacity: 0; transition: opacity 0.12s; }
#sidebar:hover nav button .lbl { opacity: 1; }
#sidebar .user-area { padding: 10px 0; border-top: 1px solid var(--border); width: 100%; text-align: center; flex-shrink: 0; }
#sidebar .user-badge { font-size: 10px; color: var(--muted); cursor: pointer; white-space: nowrap; }
#sidebar:hover .user-badge { font-size: 11px; }

/* ── Main ── */
#main { flex: 1; display: flex; flex-direction: column; overflow: hidden; }
#panel-header { padding: 10px 20px; border-bottom: 1px solid var(--border); display: flex; align-items: center; gap: 10px; min-height: 44px; background: var(--panel); }
#panel-header h2 { font-size: 13px; font-weight: 600; color: var(--accent); letter-spacing: -0.3px; }
#panel-header .breadcrumb { font-size: 11px; color: var(--muted); }
#panel-content { flex: 1; overflow-y: auto; padding: 16px 20px; }
#statusbar { padding: 4px 16px; border-top: 1px solid var(--border); font-size: 10px; color: var(--muted); display: flex; justify-content: space-between; background: var(--panel); min-height: 22px; }
#statusbar span { display: flex; align-items: center; gap: 4px; }
#statusbar .dot { width: 6px; height: 6px; border-radius: 50%; display: inline-block; }
#statusbar .dot.ok { background: var(--green); }

/* ── Shared components ── */
.panel { display: none; }
.panel.active { display: block; }
.card { background: var(--panel); border: 1px solid var(--border); border-radius: 8px; padding: 16px; margin-bottom: 12px; }
.card h3 { font-size: 12px; font-weight: 600; color: var(--text); margin-bottom: 10px; }
.input-row { display: flex; gap: 6px; margin-bottom: 8px; }
.input-row input, .input-row select { flex: 1; padding: 7px 10px; background: var(--bg); border: 1px solid var(--border); color: var(--text); border-radius: 5px; font-size: 12px; font-family: var(--font); }
.input-row input:focus, .input-row select:focus { outline: none; border-color: var(--accent); }
.btn { padding: 7px 14px; border: none; border-radius: 5px; cursor: pointer; font-weight: 600; font-size: 11px; font-family: var(--font); white-space: nowrap; }
.btn-pri { background: var(--accent); color: #0a0a12; }
.btn-pri:hover { background: #7dd4f5; }
.btn-sec { background: #2a2a3c; color: var(--text); }
.btn-sec:hover { background: #35354c; }
.btn-danger { background: var(--red); color: #fff; }
.btn-sm { padding: 4px 10px; font-size: 10px; }
.badge { display: inline-block; padding: 2px 8px; border-radius: 10px; font-size: 10px; font-weight: 600; }
.badge-cyan { background: rgba(108,199,240,0.15); color: var(--accent); }
.badge-purple { background: rgba(184,154,224,0.15); color: var(--purple); }
.badge-green { background: rgba(92,216,156,0.15); color: var(--green); }
.badge-red { background: rgba(240,85,106,0.15); color: var(--red); }
.badge-orange { background: rgba(240,160,96,0.15); color: var(--orange); }
table { width: 100%; border-collapse: collapse; font-size: 11px; }
th { background: rgba(108,199,240,0.06); color: var(--accent); padding: 6px 10px; text-align: left; font-weight: 600; position: sticky; top: 0; }
td { padding: 5px 10px; border-bottom: 1px solid var(--border); max-width: 200px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
tr:hover td { background: rgba(108,199,240,0.03); }
.empty-state { color: var(--muted); text-align: center; padding: 40px 20px; font-size: 12px; font-style: italic; }
.loading { color: var(--muted); font-size: 12px; padding: 20px; }
.error-text { color: var(--red); font-size: 12px; }
.mono { font-family: var(--mono); font-size: 11px; }
.pre-box { background: var(--bg); border: 1px solid var(--border); border-radius: 5px; padding: 12px; font-family: var(--mono); font-size: 11px; white-space: pre-wrap; word-break: break-all; max-height: 300px; overflow-y: auto; color: var(--text); }
.kv-row { display: flex; padding: 3px 0; border-bottom: 1px solid rgba(37,37,56,0.5); font-size: 11px; }
.kv-key { color: var(--muted); width: 100px; flex-shrink: 0; }
.kv-val { color: var(--text); word-break: break-all; }

/* ── Graph panel ── */
#graph-area { height: calc(100vh - 130px); position: relative; border-radius: 8px; overflow: hidden; border: 1px solid var(--border); }
#graph-viz { width: 100%; height: 100%; }
.graph-toolbar { position: absolute; top: 8px; left: 8px; z-index: 5; display: flex; gap: 4px; flex-wrap: wrap; }
.graph-toolbar input, .graph-toolbar select { padding: 5px 8px; background: rgba(19,19,31,0.95); border: 1px solid var(--border); color: var(--text); border-radius: 4px; font-size: 11px; font-family: var(--font); }
.graph-toolbar input { width: 240px; }
.graph-toolbar input:focus, .graph-toolbar select:focus { outline: none; border-color: var(--accent); }
.legend-box { position: absolute; bottom: 8px; left: 8px; background: rgba(19,19,31,0.94); padding: 6px 10px; border-radius: 5px; font-size: 10px; max-height: 280px; overflow-y: auto; z-index: 5; }
.legend-item { display: flex; align-items: center; margin: 2px 0; gap: 5px; }
.legend-dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; }

/* ── Query panel ── */
.query-editor-label { font-size: 10px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.5px; margin-bottom: 6px; display: flex; justify-content: space-between; align-items: center; }
#query-editor { border: 1px solid #3a3a52; border-radius: 8px; overflow: hidden; }
#query-editor:focus-within { border-color: var(--accent); box-shadow: 0 0 0 3px rgba(108,199,240,0.12); }
#query-editor .cm-editor { background: #0d0d1a; border-radius: 8px; }
#query-editor .cm-editor .cm-scroller { font-family: var(--mono); font-size: 13px; line-height: 1.6; }
#query-editor .cm-editor .cm-content { padding: 8px 0; }
#query-editor .cm-editor .cm-gutters { background: #0a0a18; border-right: 1px solid #252540; color: #4a4a60; }
#query-editor .cm-editor .cm-activeLineGutter { background: rgba(108,199,240,0.06); }
.query-toolbar { display: flex; gap: 6px; margin-top: 10px; align-items: center; }
#query-results { margin-top: 12px; max-height: 400px; overflow-y: auto; }

/* ── Explorer panel ── */
.explorer-layout { display: flex; gap: 12px; height: calc(100vh - 130px); }
.explorer-tree { width: 280px; min-width: 280px; background: var(--panel); border: 1px solid var(--border); border-radius: 8px; overflow-y: auto; padding: 8px 0; }
.explorer-tree .tree-item { padding: 5px 12px 5px 20px; cursor: pointer; font-size: 11px; color: var(--text); display: flex; align-items: center; gap: 6px; border-left: 2px solid transparent; }
.explorer-tree .tree-item:hover { background: rgba(108,199,240,0.05); }
.explorer-tree .tree-item.selected { background: rgba(108,199,240,0.1); border-left-color: var(--accent); color: var(--accent); }
.explorer-tree .tree-item.type-node { font-weight: 600; padding-left: 12px; font-size: 12px; }
.explorer-tree .tree-item .count { margin-left: auto; color: var(--muted); font-size: 10px; }
.explorer-tree .tree-header { padding: 6px 12px; font-size: 10px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.5px; }
.explorer-tree .filter-bar { padding: 6px 8px; }
.explorer-tree .filter-bar input { width: 100%; padding: 5px 8px; background: var(--bg); border: 1px solid var(--border); color: var(--text); border-radius: 4px; font-size: 11px; font-family: var(--font); }
.explorer-tree .filter-bar input:focus { outline: none; border-color: var(--accent); }
.explorer-detail { flex: 1; overflow-y: auto; }

/* ── Inspector panel ── */
.inspector-layout { display: flex; gap: 12px; height: calc(100vh - 130px); }
.inspector-search { width: 300px; min-width: 300px; }
.inspector-search input { width: 100%; padding: 8px 10px; background: var(--bg); border: 1px solid var(--border); color: var(--text); border-radius: 6px; font-size: 12px; font-family: var(--font); margin-bottom: 8px; }
.inspector-search input:focus { outline: none; border-color: var(--accent); }
.inspector-search .suggestions { max-height: 300px; overflow-y: auto; }
.inspector-search .suggest-item { padding: 6px 10px; cursor: pointer; font-size: 11px; border-radius: 4px; display: flex; justify-content: space-between; }
.inspector-search .suggest-item:hover { background: rgba(108,199,240,0.08); }
.inspector-detail { flex: 1; overflow-y: auto; }
.section-title { font-size: 11px; font-weight: 600; color: var(--accent); margin: 16px 0 6px; padding-bottom: 4px; border-bottom: 1px solid var(--border); text-transform: uppercase; letter-spacing: 0.5px; }
.section-title:first-child { margin-top: 0; }

/* ── Admin panel ── */
.admin-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(340px, 1fr)); gap: 12px; }
.metric-big { font-size: 28px; font-weight: 700; color: var(--accent); }
.metric-label { font-size: 11px; color: var(--muted); }
</style>
</head>
<body>

<!-- ── Login ── -->
<div id="login-overlay">
  <div class="login-card">
    <h1>Mnemosyne Studio</h1>
    <div class="sub">The Knowledge OS Desktop</div>
    <input type="text" id="login-user" placeholder="Username" value="admin" autocomplete="off" />
    <input type="password" id="login-pass" placeholder="Password" value="admin" />
    <button onclick="doLogin()">Sign In</button>
    <div class="err" id="login-error"></div>
    <div class="hint">Default credentials: admin / admin<br>Read-only: user / user</div>
  </div>
</div>

<!-- ── App ── -->
<div id="app">
  <div id="sidebar">
    <div class="logo">🧠<span class="logo-text">Mnemosyne</span></div>
    <nav>
      <button class="active" data-panel="query" title="Query Editor">
        <span class="ico">⌨</span><span class="lbl">Query Editor</span>
      </button>
      <button data-panel="graph" title="Knowledge Graph">
        <span class="ico">🕸</span><span class="lbl">Knowledge Graph</span>
      </button>
      <button data-panel="explorer" title="Knowledge Explorer">
        <span class="ico">📁</span><span class="lbl">Explorer</span>
      </button>
      <button data-panel="schema" title="Schema Explorer">
        <span class="ico">🏷</span><span class="lbl">Schema</span>
      </button>
      <button data-panel="inspector" title="KO Inspector">
        <span class="ico">🔬</span><span class="lbl">Inspector</span>
      </button>
      <button data-panel="ontology" title="Ontology">
        <span class="ico">🦉</span><span class="lbl">Ontology</span>
      </button>
      <button data-panel="admin" title="Administration">
        <span class="ico">⚙</span><span class="lbl">Admin</span>
      </button>
    </nav>
    <div class="user-area">
      <div class="user-badge" onclick="logout()" id="user-badge">―</div>
    </div>
  </div>
  <div id="main">
    <div id="panel-header">
      <h2 id="panel-title">Query Editor</h2>
      <span class="breadcrumb" id="panel-breadcrumb"></span>
    </div>
    <div id="panel-content">

      <!-- ── Query Editor ── -->
      <div class="panel active" id="panel-query">
        <div class="card">
          <div class="query-editor-label">
            <span>AIKOQL Query</span>
            <span>Ctrl+Enter to run · comma-separated properties · SQL-style highlighting</span>
          </div>
          <div id="query-editor"></div>
          <div class="query-toolbar">
            <button class="btn btn-pri" onclick="runQuery()">▶ Run</button>
            <button class="btn btn-sec" onclick="explainQuery()">🔍 Explain Plan</button>
            <button class="btn btn-sec btn-sm" onclick="streamQuery()" title="Stream results in chunks">📡 Stream</button>
            <select id="query-tenant" style="padding:5px 8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:11px;font-family:var(--font);">
              <option value="">Default tenant</option>
            </select>
          </div>
        </div>
        <div id="query-results"></div>
      </div>

      <!-- ── Knowledge Graph ── -->
      <div class="panel" id="panel-graph">
        <div id="graph-area">
          <div class="graph-toolbar">
            <input type="text" id="graph-search" placeholder="KOID hex or type name..." />
            <select id="graph-tenant" onchange="loadGraph()"><option value="">All tenants</option></select>
            <button class="btn btn-pri btn-sm" onclick="loadGraph()">Search</button>
            <button class="btn btn-sec btn-sm" onclick="loadGraphAll()">View All</button>
          </div>
          <div id="graph-viz"></div>
          <div class="legend-box" id="legend"></div>
        </div>
      </div>

      <!-- ── Knowledge Explorer ── -->
      <div class="panel" id="panel-explorer">
        <div class="explorer-layout">
          <div class="explorer-tree">
            <div class="filter-bar"><input type="text" id="explorer-filter" placeholder="Filter types..." oninput="renderExplorerTree()" /></div>
            <div id="explorer-tree-content"><div class="empty-state">Loading types...</div></div>
          </div>
          <div class="explorer-detail" id="explorer-detail">
            <div class="empty-state">Select a type from the tree to browse its objects.</div>
          </div>
        </div>
      </div>

      <!-- ── Schema Explorer ── -->
      <div class="panel" id="panel-schema">
        <div id="schema-content"><div class="loading">Loading schema...</div></div>
      </div>

      <!-- ── Ontology ── -->
      <div class="panel" id="panel-ontology">
        <div class="card">
          <h3>Ontology Definition</h3>
          <p style="font-size:11px;color:var(--muted);margin-bottom:12px;">Each ontology defines classes, relationships, property types, and source mappings for semantic querying across PostgreSQL, Neo4j, MongoDB, and Knowledge Objects.</p>
          <div id="ontology-status"><div class="loading">Loading ontology...</div></div>
          <div id="ontology-classes"></div>
          <div id="ontology-relationships"></div>
          <div id="ontology-mappings"></div>
        </div>
        <div class="card">
          <h3>Create / Replace Ontology</h3>
          <p style="font-size:11px;color:var(--muted);margin-bottom:8px;">Paste an ontology definition as YAML-like JSON. Existing ontology will be replaced.</p>
          <textarea id="ontology-yaml-input" style="width:100%;min-height:240px;background:var(--bg);color:var(--text);border:1px solid var(--border);border-radius:5px;padding:10px;font-size:11px;font-family:'JetBrains Mono',monospace;" placeholder='{
  "namespace": "enterprise",
  "version": "1.0",
  "classes": [
    {"name": "Person", "parent": null, "description": "A human being"},
    {"name": "Employee", "parent": "Person", "description": "Employed person"},
    {"name": "Department", "parent": null, "description": "Organizational unit"}
  ],
  "relationships": [
    {"name": "belongsTo", "domain": "Employee", "range": "Department", "cardinality": "1:N"}
  ],
  "property_defs": [
    {"name": "name", "type": "Text", "required": true},
    {"name": "dept", "type": "Text", "required": false}
  ],
  "mappings": [
    {"source": "postgres", "physical_type": "employees", "class": "Employee", "property_map": {"employee_id": "name", "department": "dept"}},
    {"source": "mongodb", "physical_type": "employee", "class": "Employee", "property_map": {"emp_name": "name", "dept_name": "dept"}}
  ]
}'></textarea>
          <button class="btn btn-pri" onclick="saveOntology()" style="margin-top:8px;">Save Ontology</button>
          <button class="btn btn-sec" onclick="discoverOntology()" style="margin-top:8px;margin-left:4px;">🔍 Auto-Discover</button>
          <span id="ontology-save-msg" style="margin-left:12px;font-size:11px;"></span>
        </div>
      </div>

      <!-- ── KO Inspector ── -->
      <div class="panel" id="panel-inspector">
        <div class="inspector-layout">
          <div class="inspector-search">
            <input type="text" id="inspector-search-input" placeholder="Search by KOID, type, or keyword..." oninput="inspectorSearch()" />
            <div class="suggestions" id="inspector-suggestions"></div>
          </div>
          <div class="inspector-detail" id="inspector-detail">
            <div class="empty-state">Enter a KOID or search to inspect a Knowledge Object.</div>
          </div>
        </div>
      </div>

      <!-- ── Administration ── -->
      <div class="panel" id="panel-admin">
        <div class="admin-grid" id="admin-grid"><div class="loading">Loading dashboard...</div></div>
      </div>

    </div>
    <div id="statusbar">
      <span id="status-left"><span class="dot ok"></span> Connected</span>
      <span id="status-center"></span>
      <span id="status-right"></span>
    </div>
  </div>
</div>

<script>
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

function colorFor(type) { if(!typeColors[type]) typeColors[type]=COLORS[colorIdx++%COLORS.length]; return typeColors[type]; }

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
      authToken = data.token; currentUser = u;
      document.getElementById('login-overlay').style.display = 'none';
      document.getElementById('app').style.display = 'flex';
      document.getElementById('user-badge').textContent = u + (u==='admin'?' (admin)':'');
      initApp();
    } else { document.getElementById('login-error').textContent = data.error || 'Login failed'; }
  } catch(e) { document.getElementById('login-error').textContent = 'Connection error: ' + e.message; }
}
function logout() { if (graphNetwork) { graphNetwork.destroy(); graphNetwork = null; } showLogin(); }
document.getElementById('login-pass').addEventListener('keydown', e => { if(e.key==='Enter') doLogin(); });

// ═══════════════════════════════════════════
// API helper
// ═══════════════════════════════════════════
async function api(url) {
  const sep = url.includes('?') ? '&' : '?';
  const full = url + (authToken ? sep + 'token=' + encodeURIComponent(authToken) : '');
  const res = await fetch(full);
  if (!res.ok) {
    let msg = 'HTTP ' + res.status;
    try { const body = await res.json(); if (body.error) msg = body.error; } catch(_) {}
    throw new Error(msg);
  }
  return res.json();
}

// ═══════════════════════════════════════════
// Navigation
// ═══════════════════════════════════════════
function switchPanel(name) {
  activePanel = name;
  document.querySelectorAll('#sidebar nav button').forEach(b => b.classList.toggle('active', b.dataset.panel === name));
  document.querySelectorAll('.panel').forEach(p => p.classList.remove('active'));
  const panel = document.getElementById('panel-' + name);
  if (panel) panel.classList.add('active');
  const titles = { query:'Query Editor', graph:'Knowledge Graph', explorer:'Knowledge Explorer', schema:'Schema Explorer', ontology:'Ontology', inspector:'KO Inspector', admin:'Administration' };
  document.getElementById('panel-title').textContent = titles[name] || name;
  document.getElementById('panel-breadcrumb').textContent = '';
  if (name === 'graph') initGraph();
  if (name === 'explorer') loadExplorerTree();
  if (name === 'schema') loadSchemaPanel();
  if (name === 'ontology') loadOntologyPanel();
  if (name === 'admin') loadAdminPanel();
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
    document.getElementById('status-left').innerHTML = '<span class="dot ok"></span> ' + (h.status||'ok');
    document.getElementById('status-right').textContent = 'uptime ' + (h.uptime_seconds||0).toFixed(0) + 's';
  } catch(e) { document.getElementById('status-left').innerHTML = '<span class="dot" style="background:var(--red)"></span> Offline'; }
}
setInterval(updateStatus, 15000);

// ═══════════════════════════════════════════
// Init
// ═══════════════════════════════════════════
function initApp() {
  updateStatus();
  loadTenants();
  // Init CodeMirror 6 on the query editor div.
  if (window.__initCodeMirror) window.__initCodeMirror();
}

// Note: Ctrl+Enter query execution is handled inside CodeMirror's contentDOM listener
// (see window.__initCodeMirror). Ctrl+S save feedback is below.
function loadTenants() {
  api('/api/schema').then(d => {
    if (!d.schema) return;
    // Populate dynamic type names for CodeMirror autocomplete.
    window.__schemaTypes = Object.keys(d.schema);
    const tenants = new Set();
    Object.values(d.schema).forEach(info => { if (info.tenants) info.tenants.forEach(t => tenants.add(t)); });
    const opts = '<option value="">Default tenant</option>' + [...tenants].map(t => '<option value="'+t+'">'+t+'</option>').join('');
    document.getElementById('query-tenant').innerHTML = opts;
    document.getElementById('graph-tenant').innerHTML = '<option value="">All tenants</option>' + [...tenants].map(t => '<option>'+t+'</option>').join('');
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
  try {
    const data = await api(url);
    if (data.error) { resDiv.innerHTML = '<div class="error-text">'+data.error+'</div>'; return; }
    if (data.created) { resDiv.innerHTML = '<div class="card"><span class="badge badge-green">Created</span> '+data.created+' v'+data.version+'</div>'; return; }
    if (data.results && data.results.length > 0) {
      const keys = new Set();
      data.results.forEach(r => { Object.keys(r).forEach(k => { if (k !== 'properties') keys.add(k); }); if (r.properties) Object.keys(r.properties).forEach(k => keys.add(k)); });
      let html = '<div class="card"><table><thead><tr>';
      [...keys].forEach(k => html += '<th>'+k+'</th>');
      html += '</tr></thead><tbody>';
      data.results.forEach(row => {
        html += '<tr>';
        [...keys].forEach(k => {
          let v = row[k];
          if (v === undefined && row.properties) v = row.properties[k];
          const sv = v !== undefined && v !== null ? String(v) : '';
          html += '<td title="'+sv.replace(/"/g,'&quot;')+'">'+sv.substring(0,80)+'</td>';
        });
        html += '</tr>';
      });
      html += '</tbody></table></div>';
      html += '<div style="font-size:10px;color:var(--muted);margin-top:4px;">'+data.results.length+' row'+(data.results.length!==1?'s':'')+'</div>';
      resDiv.innerHTML = html;
    } else { resDiv.innerHTML = '<div class="empty-state">Query returned 0 results.</div>'; }
  } catch(e) { resDiv.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}
window.runQuery = runQuery;
async function explainQuery() {
  const query = window.__cmGetValue().trim();
  if (!query) return;
  const resDiv = document.getElementById('query-results');
  resDiv.innerHTML = '<div class="loading">Explaining...</div>';
  try {
    const data = await api('/api/explain?query=' + encodeURIComponent(query));
    if (data.error) { resDiv.innerHTML = '<div class="error-text">'+data.error+'</div>'; return; }
    let html = '<div class="card"><h3>Query Plan — '+(data.operator_count||0)+' operators</h3>';
    html += '<div class="mono" style="color:var(--muted);margin-bottom:8px;">'+data.query+'</div>';
    html += '<table><thead><tr><th>#</th><th>Operator</th></tr></thead><tbody>';
    (data.operators||[]).forEach((op,i) => { html += '<tr><td style="color:var(--muted);">'+(i+1)+'</td><td class="mono">'+op+'</td></tr>'; });
    html += '</tbody></table></div>';
    resDiv.innerHTML = html;
  } catch(e) { resDiv.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}
async function streamQuery() {
  const query = window.__cmGetValue().trim();
  if (!query) return;
  const resDiv = document.getElementById('query-results');
  resDiv.innerHTML = '<div class="loading">Streaming (chunks of 100)...</div>';
  let chunks = [];
  // ponytail: streaming via REST fallback — call /api/aikoql repeatedly with LIMIT/OFFSET.
  // True MCP notifications/notify streaming requires a WebSocket or SSE transport on the REST side.
  // For now: run the query and chunk the results client-side.
  try {
    const data = await api('/api/aikoql?query=' + encodeURIComponent(query));
    if (data.error) { resDiv.innerHTML = '<div class="error-text">'+data.error+'</div>'; return; }
    if (!data.results || data.results.length === 0) { resDiv.innerHTML = '<div class="empty-state">0 results.</div>'; return; }
    const all = data.results;
    const CHUNK = 100;
    for (let i = 0; i < all.length; i += CHUNK) {
      chunks.push(all.slice(i, i + CHUNK));
    }
    let html = '';
    chunks.forEach((chunk, ci) => {
      html += '<div class="card" style="border-left: 2px solid var(--accent);"><h3>Chunk '+(ci+1)+'/'+chunks.length+' ('+chunk.length+' rows)</h3>';
      const keys = new Set();
      chunk.forEach(r => { Object.keys(r).forEach(k => { if (k !== 'properties') keys.add(k); }); if (r.properties) Object.keys(r.properties).forEach(k => keys.add(k)); });
      html += '<table><thead><tr>';
      [...keys].forEach(k => html += '<th>'+k+'</th>');
      html += '</tr></thead><tbody>';
      chunk.forEach(row => {
        html += '<tr>';
        [...keys].forEach(k => {
          let v = row[k]; if (v === undefined && row.properties) v = row.properties[k];
          html += '<td>'+(v !== undefined && v !== null ? String(v).substring(0,60) : '')+'</td>';
        });
        html += '</tr>';
      });
      html += '</tbody></table></div>';
    });
    html += '<div style="font-size:10px;color:var(--muted);margin-top:4px;">'+all.length+' total rows in '+chunks.length+' chunk'+(chunks.length!==1?'s':'')+'</div>';
    resDiv.innerHTML = html;
  } catch(e) { resDiv.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}
// Save query on Ctrl+S (CodeMirror auto-saves on change; this is for user feedback)
document.addEventListener('keydown', e => {
  if (e.ctrlKey && e.key === 's' && activePanel === 'query') {
    e.preventDefault();
    const q = window.__cmGetValue().trim();
    if (!q) return;
    document.getElementById('status-center').textContent = 'Query auto-saved (' + q.split('\n').length + ' lines)';
    setTimeout(() => { document.getElementById('status-center').textContent = ''; }, 1500);
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
    edges: { arrows:{to:{enabled:true,scaleFactor:0.5}}, smooth:{type:'continuous',roundness:0.3}, font:{size:9,color:'#888',strokeWidth:0}, width:1.5 },
    nodes: { shape:'dot', font:{size:11,color:'#d0d0d0',face:'sans-serif',strokeWidth:1,strokeColor:'#13131f'}, borderWidth:1.5, shadow:{enabled:true,size:6} },
    interaction: { hover:true, navigationButtons:true, keyboard:true },
  });
  graphNetwork.on('click', p => { if(p.nodes.length>0) inspectNode(p.nodes[0]); });
  graphNetwork.on('doubleClick', p => { if(p.nodes.length>0) { document.getElementById('graph-search').value=p.nodes[0]; loadGraph(); } });
  graphNetwork.on('hoverNode', p => showGraphTooltip(p.node));
  graphNetwork.on('blurNode', () => { const tt = document.getElementById('graph-tooltip'); if(tt) tt.style.display='none'; });
}
function loadGraphAll() { document.getElementById('graph-search').value=''; document.getElementById('graph-tenant').value=''; loadGraph(); }
async function loadGraph() {
  initGraph();
  const koid = document.getElementById('graph-search').value.trim();
  const tenant = document.getElementById('graph-tenant').value;
  let url = '/api/graph';
  let params = [];
  if (koid) params.push('koid='+encodeURIComponent(koid));
  if (tenant) params.push('tenant='+encodeURIComponent(tenant));
  if (params.length) url += '?'+params.join('&');
  try {
    const data = await api(url);
    graphRawData = data;
    typeColors={}; colorIdx=0;
    graphNodes.clear(); graphEdges.clear();
    const nodes = data.nodes.map(n => ({
      id: n.koid, label: (n.label||n.type_name)+(n.tenant?'\n@'+n.tenant:''),
      title: '<b>'+n.type_name+'</b>'+(n.tenant?' @'+n.tenant:'')+'\n'+n.koid,
      color: {background:colorFor(n.type_name),border:(n.tenant?'#b89ae0':'#252538'),highlight:{background:colorFor(n.type_name),border:'#fff'}},
      size: n.size||20, borderWidth: n.tenant?2.5:(n.edge_count>2?3:1.5),
      font: {size:Math.max(9,Math.min(13,(n.size||20)/2+3))},
    }));
    graphNodes.add(nodes);
    const edges = (data.edges||[]).map(e => ({from:e.source,to:e.target,label:e.rel_type,arrows:'to',color:{color:'#3a3a5c',highlight:'#6cc7f0'},font:{size:9,color:'#5c5c78'},width:1.2}));
    graphEdges.add(edges);
    // Legend
    let lh = '';
    for (const [t,c] of Object.entries(typeColors)) { const cnt = data.nodes.filter(n=>n.type_name===t).length; lh += '<div class="legend-item"><div class="legend-dot" style="background:'+c+'"></div>'+t+' ('+cnt+')</div>'; }
    document.getElementById('legend').innerHTML = lh||'<div style="color:var(--muted)">No types</div>';
    document.getElementById('status-center').textContent = data.nodes.length+' objects, '+data.edges.length+' relationships';
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
  let h = '<div style="color:var(--accent);font-weight:600;">'+node.type_name+'</div>';
  if (node.tenant) h += '<div style="color:var(--purple);font-size:10px;">@'+node.tenant+'</div>';
  if (node.key_props) node.key_props.slice(0,5).forEach(p => h += '<div style="color:var(--muted);">'+p.key+': '+JSON.stringify(p.value)+'</div>');
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
    detail.innerHTML = '<div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:6px;"><span style="font-weight:600;color:var(--accent);">'+node.type_name+'</span><button class="btn btn-sec btn-sm" onclick="this.parentElement.parentElement.remove()">✕</button></div>' + h;
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
  } catch(e) { document.getElementById('explorer-tree-content').innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}
function renderExplorerTree() {
  if (!explorerData || !explorerData.schema) return;
  const filter = (document.getElementById('explorer-filter').value || '').toLowerCase();
  const types = Object.entries(explorerData.schema).filter(([t]) => !filter || t.toLowerCase().includes(filter));
  let html = '';
  types.forEach(([typeName, info]) => {
    const tenantList = (info.tenants||[]).join(', ');
    html += '<div class="tree-item type-node" onclick="exploreType(\''+typeName.replace(/'/g,"\\'")+'\')" data-type="'+typeName+'">';
    html += '<span style="color:'+colorFor(typeName)+'">●</span> '+typeName;
    html += '<span class="count">'+(info.count||0)+'</span>';
    html += '</div>';
    if (info.tenants && info.tenants.length > 0) {
      info.tenants.forEach(t => {
        html += '<div class="tree-item" style="padding-left:32px;font-size:10px;" onclick="exploreTypeTenant(\''+typeName.replace(/'/g,"\\'")+'\',\''+t.replace(/'/g,"\\'")+'\')">@'+t+'</div>';
      });
    }
  });
  document.getElementById('explorer-tree-content').innerHTML = html || '<div class="empty-state">No types found.</div>';
}
async function exploreType(typeName) {
  document.getElementById('explorer-detail').innerHTML = '<div class="loading">Loading '+typeName+'...</div>';
  try {
    const data = await api('/api/graph?type=' + encodeURIComponent(typeName));
    if (!data.nodes || data.nodes.length === 0) {
      document.getElementById('explorer-detail').innerHTML = '<div class="empty-state">No objects of type '+typeName+' found.</div>';
      return;
    }
    let html = '<div class="card"><h3>'+typeName+' ('+data.nodes.length+' objects)</h3>';
    html += '<table><thead><tr><th>KOID</th><th>Label</th><th>Tenant</th><th>Version</th><th>Edges</th></tr></thead><tbody>';
    data.nodes.forEach(n => {
      html += '<tr>';
      html += '<td class="mono"><a href="#" onclick="switchPanel(\'inspector\');document.getElementById(\'inspector-search-input\').value=\''+n.koid+'\';inspectorSearch();return false;" style="color:var(--accent);">'+n.koid.substring(0,20)+'...</a></td>';
      html += '<td>'+(n.label||n.type_name)+'</td>';
      html += '<td>'+(n.tenant?'<span class="badge badge-purple">@'+n.tenant+'</span>':'―')+'</td>';
      html += '<td>'+(n.version||'?')+'</td>';
      html += '<td>'+(n.edge_count||0)+'</td>';
      html += '</tr>';
    });
    html += '</tbody></table></div>';
    document.getElementById('explorer-detail').innerHTML = html;
    // Highlight selected
    document.querySelectorAll('#explorer-tree-content .tree-item').forEach(el => el.classList.remove('selected'));
    const sel = document.querySelector('#explorer-tree-content .tree-item[data-type="'+typeName+'"]');
    if (sel) sel.classList.add('selected');
  } catch(e) { document.getElementById('explorer-detail').innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}
async function exploreTypeTenant(typeName, tenant) {
  document.getElementById('explorer-detail').innerHTML = '<div class="loading">Loading '+typeName+' @'+tenant+'...</div>';
  try {
    const data = await api('/api/graph?type='+encodeURIComponent(typeName)+'&tenant='+encodeURIComponent(tenant));
    let html = '<div class="card"><h3>'+typeName+' <span class="badge badge-purple">@'+tenant+'</span> ('+(data.nodes||[]).length+' objects)</h3>';
    if (data.nodes && data.nodes.length > 0) {
      html += '<table><thead><tr><th>KOID</th><th>Label</th><th>Version</th><th>Edges</th></tr></thead><tbody>';
      data.nodes.forEach(n => html += '<tr><td class="mono" style="font-size:10px;">'+n.koid+'</td><td>'+(n.label||'')+'</td><td>'+(n.version||'?')+'</td><td>'+(n.edge_count||0)+'</td></tr>');
      html += '</tbody></table>';
    }
    html += '</div>';
    document.getElementById('explorer-detail').innerHTML = html;
  } catch(e) { document.getElementById('explorer-detail').innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}

// ═══════════════════════════════════════════
// Panel: Schema Explorer
// ═══════════════════════════════════════════
async function loadSchemaPanel() {
  try {
    const data = await api('/api/schema');
    if (!data.schema) { document.getElementById('schema-content').innerHTML = '<div class="empty-state">No schema data available.</div>'; return; }
    let html = '<div style="margin-bottom:12px;font-size:12px;color:var(--accent);">'+Object.keys(data.schema).length+' types</div>';
    for (const [typeName, info] of Object.entries(data.schema)) {
      html += '<div class="card">';
      html += '<h3 style="display:flex;justify-content:space-between;align-items:center;">';
      html += '<span><span style="color:'+colorFor(typeName)+'">●</span> '+typeName+'</span>';
      html += '<span class="badge badge-cyan">'+(info.count||0)+' objects</span>';
      html += '</h3>';
      if (info.tenants && info.tenants.length > 0) html += '<div style="margin-bottom:6px;">'+info.tenants.map(t => '<span class="badge badge-purple">@'+t+'</span>').join(' ')+'</div>';
      if (info.properties && info.properties.length > 0) {
        html += '<div style="font-size:11px;"><span style="color:var(--muted);">Properties: </span>';
        html += info.properties.map(p => '<code style="background:var(--bg);padding:1px 6px;border-radius:3px;font-size:10px;color:var(--green);">'+p+'</code>').join(' ');
        html += '</div>';
      } else { html += '<div style="font-size:11px;color:var(--muted);">No properties discovered yet.</div>'; }
      if (info.relationship_types && info.relationship_types.length > 0) {
        html += '<div style="font-size:11px;margin-top:4px;"><span style="color:var(--muted);">Relationships: </span>';
        html += info.relationship_types.map(r => '<span class="badge badge-orange">'+r+'</span>').join(' ');
        html += '</div>';
      }
      html += '</div>';
    }
    document.getElementById('schema-content').innerHTML = html || '<div class="empty-state">No types found. Create KOs to populate the schema.</div>';
  } catch(e) { document.getElementById('schema-content').innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}

// ═══════════════════════════════════════════
// Shared: render node detail (used by Inspector + Graph)
// ═══════════════════════════════════════════
function renderNodeDetail(node) {
  let h = '';
  // Header badges
  if (node.tenant) h += '<span class="badge badge-purple">@'+node.tenant+'</span> ';
  if (node.lifecycle) {
    const st = typeof node.lifecycle === 'object' ? node.lifecycle.state : node.lifecycle;
    const cls = st==='active'?'badge-green':st==='archived'?'badge-red':st==='draft'?'badge-orange':'badge-cyan';
    h += '<span class="badge '+cls+'">'+st+'</span> ';
  }
  if (node.security && node.security.classification) {
    h += '<span class="badge badge-red">'+node.security.classification+'</span> ';
  }
  h += '<div style="margin-top:6px;"></div>';

  // Identity
  h += '<div class="section-title">Identity</div>';
  h += '<div class="kv-row"><span class="kv-key">KOID</span><span class="kv-val mono" style="font-size:10px;">'+node.koid+'</span></div>';
  h += '<div class="kv-row"><span class="kv-key">Type</span><span class="kv-val">'+node.type_name+'</span></div>';
  h += '<div class="kv-row"><span class="kv-key">Version</span><span class="kv-val">'+(node.version||'?')+' (schema v'+(node.schema_version||1)+')</span></div>';

  // Lifecycle
  if (node.lifecycle) {
    h += '<div class="section-title">Lifecycle</div>';
    if (typeof node.lifecycle === 'object') {
      h += '<div class="kv-row"><span class="kv-key">State</span><span class="kv-val">'+node.lifecycle.state+'</span></div>';
      if (node.lifecycle.origin) h += '<div class="kv-row"><span class="kv-key">Origin</span><span class="kv-val">'+node.lifecycle.origin+'</span></div>';
    }
    h += '<div class="kv-row"><span class="kv-key">Edges</span><span class="kv-val">'+(node.edge_count||0)+'</span></div>';
  }

  // Security
  if (node.security) {
    h += '<div class="section-title">Security</div>';
    h += '<div class="kv-row"><span class="kv-key">Owner</span><span class="kv-val">'+(node.security.owner||'?')+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Classification</span><span class="kv-val">'+(node.security.classification||'none')+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">ACL Entries</span><span class="kv-val">'+(node.security.acl_count||0)+'</span></div>';
  }

  // Properties
  if (node.properties && Object.keys(node.properties).length > 0) {
    h += '<div class="section-title">Properties</div>';
    for (const [k,v] of Object.entries(node.properties)) {
      h += '<div class="kv-row"><span class="kv-key">'+k+'</span><span class="kv-val">'+JSON.stringify(v)+'</span></div>';
    }
  }

  // Relationships
  if (node.relationships && node.relationships.length > 0) {
    h += '<div class="section-title">Relationships ('+node.relationships.length+')</div>';
    node.relationships.forEach(r => {
      h += '<div class="kv-row"><span class="kv-key">'+r.direction+'</span><span class="kv-val"><span class="badge badge-orange">'+r.type+'</span> → <span class="mono" style="font-size:10px;">'+(r.target||'').substring(0,20)+'...</span></span></div>';
    });
  }

  // Extensions
  if (node.extensions && Object.keys(node.extensions).length > 0) {
    h += '<div class="section-title">Extensions</div>';
    for (const [k,v] of Object.entries(node.extensions)) {
      h += '<div class="kv-row"><span class="kv-key">'+k+'</span><span class="kv-val">'+JSON.stringify(v)+'</span></div>';
    }
  }

  // Events & Audit
  if (node.event_refs) h += '<div class="section-title">Audit</div><div class="kv-row"><span class="kv-key">Events</span><span class="kv-val">'+node.event_refs+' journal references</span></div>';

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
      } catch(e) { sug.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
      return;
    }
    // Search by type name
    sug.innerHTML = '<div class="loading">Searching...</div>';
    try {
      const data = await api('/api/graph?type='+encodeURIComponent(q));
      if (data.nodes && data.nodes.length > 0) {
        let html = '';
        data.nodes.slice(0, 30).forEach(n => {
          html += '<div class="suggest-item" onclick="inspectorLoadKOID(\''+n.koid+'\')">';
          html += '<span><span style="color:'+colorFor(n.type_name)+'">●</span> '+n.type_name+'</span>';
          html += '<span class="mono" style="font-size:10px;color:var(--muted);">'+n.koid.substring(0,16)+'...</span>';
          html += '</div>';
        });
        if (data.nodes.length > 30) html += '<div style="font-size:10px;color:var(--muted);padding:4px 10px;">+ '+(data.nodes.length-30)+' more (narrow your search)</div>';
        sug.innerHTML = html;
      } else { sug.innerHTML = '<div class="empty-state">Nothing matches "'+q+'".</div>'; }
    } catch(e) { sug.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
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
      document.getElementById('inspector-suggestions').innerHTML = '<div style="font-size:10px;color:var(--green);padding:6px 10px;">✓ '+node.type_name+'</div>';
    } else { document.getElementById('inspector-detail').innerHTML = '<div class="empty-state">Object not found.</div>'; }
  } catch(e) { document.getElementById('inspector-detail').innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}

// ═══════════════════════════════════════════
// Panel: Ontology
// ═══════════════════════════════════════════
async function loadOntologyPanel() {
  try {
    // Fetch existing ontology via the aikoql endpoint.
    const data = await api('/api/aikoql?query=MATCH%20Ontology%20RETURN%20*');
    const statusDiv = document.getElementById('ontology-status');
    const classesDiv = document.getElementById('ontology-classes');
    const relsDiv = document.getElementById('ontology-relationships');
    const mapsDiv = document.getElementById('ontology-mappings');

    if (!data.results || data.results.length === 0) {
      statusDiv.innerHTML = '<div class="empty-state">No ontology defined yet. Use the form below to create one.</div>';
      classesDiv.innerHTML = ''; relsDiv.innerHTML = ''; mapsDiv.innerHTML = '';
      return;
    }
    const ko = data.results[0];
    const props = ko.properties || {};
    statusDiv.innerHTML = '<div style="color:var(--green);margin-bottom:8px;">✓ Ontology loaded — <b>'+ (props.namespace||'?') +'</b> v'+ (props.version||'?') +'</div>';

    // Render classes
    if (props.classes && Array.isArray(props.classes)) {
      let h = '<h3>Classes ('+props.classes.length+')</h3><div class="ontology-grid">';
      props.classes.forEach(c => {
        const cm = typeof c === 'object' ? c : {};
        h += '<div class="card" style="padding:8px 12px;"><span style="color:var(--accent);font-weight:600;">'+ (cm.name||'?') +'</span>';
        if (cm.parent) h += ' <span style="color:var(--muted);">extends</span> <span style="color:var(--green);">'+cm.parent+'</span>';
        if (cm.description) h += '<div style="font-size:10px;color:var(--muted);">'+cm.description+'</div>';
        h += '</div>';
      });
      h += '</div>';
      classesDiv.innerHTML = h;
    }

    // Render relationships
    if (props.relationships && Array.isArray(props.relationships)) {
      let h = '<h3>Relationships ('+props.relationships.length+')</h3><table><thead><tr><th>Name</th><th>Domain</th><th>Range</th><th>Card.</th></tr></thead><tbody>';
      props.relationships.forEach(r => {
        h += '<tr><td style="color:var(--accent);">'+ (r.name||'?') +'</td><td>'+ (r.domain||'*') +'</td><td>'+ (r.range||'*') +'</td><td><span class="badge badge-purple">'+ (r.cardinality||'?') +'</span></td></tr>';
      });
      h += '</tbody></table>';
      relsDiv.innerHTML = h;
    }

    // Render mappings
    if (props.mappings && Array.isArray(props.mappings)) {
      let h = '<h3>Mappings ('+props.mappings.length+')</h3><table><thead><tr><th>Source</th><th>Physical Type</th><th>Class</th><th>Property Map</th></tr></thead><tbody>';
      props.mappings.forEach(m => {
        const pm = typeof m.property_map === 'object' ? m.property_map : {};
        const pmStr = Object.entries(pm).map(([k,v]) => k+' → '+v).join(', ');
        h += '<tr><td><span class="badge badge-cyan">'+ (m.source||'?') +'</span></td><td class="mono" style="font-size:10px;">'+ (m.physical_type||'?') +'</td><td>'+ (m.class||'?') +'</td><td style="font-size:10px;color:var(--muted);">'+pmStr+'</td></tr>';
      });
      h += '</tbody></table>';
      mapsDiv.innerHTML = h;
    }
  } catch(e) {
    document.getElementById('ontology-status').innerHTML = '<div class="error-text">'+e.message+'</div>';
  }
}

async function discoverOntology() {
  const msg = document.getElementById('ontology-save-msg');
  msg.innerHTML = '<span style="color:var(--muted);">Discovering from all stored KOs...</span>';
  try {
    const url = '/api/v1/discover-ontology' + (authToken ? '?token=' + encodeURIComponent(authToken) : '');
    const res = await fetch(url, { method: 'POST' });
    const data = await res.json();
    if (data.error) { msg.innerHTML = '<span style="color:var(--red);">Error: '+data.error+'</span>'; return; }
    const d = data.data || data;
    msg.innerHTML = '<span style="color:var(--green);">✓ Discovered: '+d.classes+' classes, '+d.relationships+' relationships, '+d.mappings+' mappings from '+(d.types_discovered||[]).length+' types.</span>';
    loadOntologyPanel();
  } catch(e) { msg.innerHTML = '<span style="color:var(--red);">'+e.message+'</span>'; }
}

async function saveOntology() {
  const msg = document.getElementById('ontology-save-msg');
  msg.innerHTML = '<span style="color:var(--muted);">Saving...</span>';
  try {
    const raw = document.getElementById('ontology-yaml-input').value.trim();
    if (!raw) { msg.innerHTML = '<span style="color:var(--red);">Please paste an ontology definition.</span>'; return; }
    const def = JSON.parse(raw);
    // Validate required fields
    if (!def.namespace) { msg.innerHTML = '<span style="color:var(--red);">Missing "namespace" field.</span>'; return; }
    if (!def.classes) { msg.innerHTML = '<span style="color:var(--red);">Missing "classes" array.</span>'; return; }

    // Build the ontology KO and save via CREATE-style aikoql.
    // Since CREATE doesn't support nested structures, use the /api/v1/remember endpoint.
    const payload = {
      type_name: "Ontology",
      properties: def,
      subject: "admin"
    };
    const url = '/api/v1/remember' + (authToken ? '?token=' + encodeURIComponent(authToken) : '');
    const res = await fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload)
    });
    if (!res.ok) {
      const err = await res.json().catch(() => ({}));
      msg.innerHTML = '<span style="color:var(--red);">Error: '+ (err.error || 'HTTP '+res.status) +'</span>';
      return;
    }
    const result = await res.json();
    msg.innerHTML = '<span style="color:var(--green);">✓ Ontology saved — reload to apply.</span>';
    // Reload the display
    setTimeout(() => loadOntologyPanel(), 500);
  } catch(e) {
    msg.innerHTML = '<span style="color:var(--red);">'+e.message+'</span>';
  }
}

// ═══════════════════════════════════════════
// Panel: Administration
// ═══════════════════════════════════════════
async function loadAdminPanel() {
  try {
    const [health, metrics, backups] = await Promise.all([
      api('/health').catch(() => ({status:'offline'})),
      api('/api/v1/metrics-info').then(r => r.data).catch(() => ({})),
      api('/api/v1/backups').then(r => r.data).catch(() => ({backups:[]}))
    ]);
    let html = '';
    // Health card
    html += '<div class="card"><h3>System Health</h3>';
    html += '<div class="metric-big">'+(health.status||'?')+'</div>';
    html += '<div class="metric-label">uptime '+(health.uptime_seconds||0).toFixed(0)+'s</div>';
    html += '<div style="margin-top:8px;font-size:11px;color:var(--muted);">Journal seq: '+(metrics.journal_seq||'?')+' · Objects: '+(metrics.total_objects||'?')+' · Types: '+(Object.keys(metrics.by_type||{}).length||'?')+'</div>';
    html += '</div>';
    // Metrics card
    html += '<div class="card"><h3>Database Metrics</h3>';
    html += '<div class="kv-row"><span class="kv-key">Objects</span><span class="kv-val">'+(metrics.total_objects||0)+'</span></div>';
    html += '<div class="kv-row"><span class="kv-key">Active</span><span class="kv-val">'+(metrics.active_objects||0)+'</span></div>';
    html += '<div class="kv-row"><span class="kv-key">Journal Seq</span><span class="kv-val mono">'+(metrics.journal_seq||0)+'</span></div>';
    html += '<div class="kv-row"><span class="kv-key">Uptime</span><span class="kv-val">'+(metrics.uptime_seconds||0).toFixed(0)+'s</span></div>';
    if (metrics.by_type) {
      html += '<div style="margin-top:6px;font-size:11px;">';
      for (const [t,c] of Object.entries(metrics.by_type)) html += '<span class="badge badge-cyan" style="margin:1px;">'+t+': '+c+'</span> ';
      html += '</div>';
    }
    html += '</div>';
    // Backups card
    html += '<div class="card"><h3>Backups</h3>';
    const blist = backups.backups || [];
    if (blist.length > 0) {
      html += '<table><thead><tr><th>Name</th><th>Size</th><th>Created</th></tr></thead><tbody>';
      blist.forEach(b => html += '<tr><td>'+b.name+'</td><td>'+(b.size_bytes||'?')+'</td><td>'+(b.created_at||'')+'</td></tr>');
      html += '</tbody></table>';
    } else { html += '<div style="color:var(--muted);font-size:11px;">No backups found. Create one via CLI or API.</div>'; }
    html += '</div>';
    // Audit card
    html += '<div class="card"><h3>Audit & Compliance</h3>';
    html += '<div style="font-size:11px;color:var(--muted);margin-bottom:8px;">SHA-256 audit chain. Every mutation is cryptographically verifiable.</div>';
    html += '<button class="btn btn-sec btn-sm" onclick="loadAuditReport()">View Audit Report</button> ';
    html += '<button class="btn btn-sec btn-sm" onclick="loadComplianceReport()">Compliance Report</button>';
    html += '<div id="admin-audit-result" style="margin-top:8px;"></div>';
    html += '</div>';
    document.getElementById('admin-grid').innerHTML = html;
  } catch(e) { document.getElementById('admin-grid').innerHTML = '<div class="error-text">Failed to load admin dashboard: '+e.message+'</div>'; }
}
async function loadAuditReport() {
  const div = document.getElementById('admin-audit-result');
  div.innerHTML = '<div class="loading">Loading...</div>';
  try {
    const data = await api('/api/v1/audit');
    div.innerHTML = '<div class="pre-box">'+JSON.stringify(data, null, 2)+'</div>';
  } catch(e) { div.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}
async function loadComplianceReport() {
  const div = document.getElementById('admin-audit-result');
  div.innerHTML = '<div class="loading">Loading...</div>';
  try {
    const data = await api('/api/v1/compliance');
    div.innerHTML = '<div class="pre-box">'+JSON.stringify(data, null, 2)+'</div>';
  } catch(e) { div.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}

// ═══════════════════════════════════════════
// Startup
// ═══════════════════════════════════════════
document.addEventListener('DOMContentLoaded', () => {
  showLogin();
  document.getElementById('login-pass').addEventListener('keydown', e => { if(e.key==='Enter') doLogin(); });
});
</script>
</body>
</html>"##;
