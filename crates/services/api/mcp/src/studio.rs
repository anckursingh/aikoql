//! Aikoql Studio — The Knowledge OS Desktop.
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
<title>Aikoql Studio</title>
<script src="https://cdn.jsdelivr.net/npm/vis-network@9.1.2/dist/vis-network.min.js"></script>
<script type="module">
import {EditorView, basicSetup} from "https://esm.sh/codemirror@6.0.1"
import {sql, PostgreSQL} from "https://esm.sh/@codemirror/lang-sql@6.2.0"
import {oneDark} from "https://esm.sh/@codemirror/theme-one-dark@6.1.2"
import {autocompletion} from "https://esm.sh/@codemirror/autocomplete@6.0.0"

// ── aikoql keyword completions ──
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
  // aikoql: types
  {label:"aikoql:program",type:"type",detail:"Program KO"},
  {label:"aikoql:workflow",type:"type",detail:"Workflow KO"},
  {label:"aikoql:policy",type:"type",detail:"Policy KO"},
  {label:"aikoql:agent",type:"type",detail:"Agent KO"},
  {label:"aikoql:trigger",type:"type",detail:"Trigger KO"},
  {label:"aikoql:connector",type:"type",detail:"Connector KO"},
  {label:"aikoql:benchmark",type:"type",detail:"Benchmark KO"},
  {label:"aikoql:view",type:"type",detail:"Materialized view KO"},
  {label:"aikoql:report",type:"type",detail:"Report KO"},
  {label:"aikoql:ontology",type:"type",detail:"Ontology KO"},
  {label:"aikoql:memory",type:"type",detail:"Agent memory"},
  {label:"aikoql:query",type:"type",detail:"Saved query KO"},
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
          const hist = JSON.parse(localStorage.getItem('aikoql-query-history')||'[]');
          if (q && !hist.includes(q)) { hist.unshift(q); if (hist.length>50) hist.pop(); localStorage.setItem('aikoql-query-history',JSON.stringify(hist)); }
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
    <h1>Aikoql Studio</h1>
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
    <div class="logo">🧠<span class="logo-text">aikoql</span></div>
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
      <button data-panel="timeline" title="Timeline — MVCC time travel">
        <span class="ico">⏳</span><span class="lbl">Timeline</span>
      </button>
      <button data-panel="provenance" title="Provenance — cryptographic audit chain">
        <span class="ico">🔗</span><span class="lbl">Provenance</span>
      </button>
      <button data-panel="debugger" title="Program Debugger">
        <span class="ico">🐛</span><span class="lbl">Debugger</span>
      </button>
      <button data-panel="benchmarks" title="Benchmark Center">
        <span class="ico">⏱</span><span class="lbl">Benchmarks</span>
      </button>
      <button data-panel="profiler" title="Query Profiler">
        <span class="ico">📊</span><span class="lbl">Profiler</span>
      </button>
      <button data-panel="providers" title="Provider Manager">
        <span class="ico">🔌</span><span class="lbl">Providers</span>
      </button>
      <button data-panel="documents" title="Document Explorer">
        <span class="ico">📄</span><span class="lbl">Documents</span>
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
            <span>Aikoql Query</span>
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

      <!-- ── Query Profiler ── -->
      <div class="panel" id="panel-profiler">
        <div class="card">
          <h3>📊 Query Profiler</h3>
          <p style="font-size:11px;color:var(--muted);margin-bottom:12px;">Run aikoql queries and examine execution plans. Measure, compare, optimize.</p>
          <div style="margin-bottom:12px;">
            <textarea id="profiler-query" style="width:100%;height:80px;padding:10px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:12px;font-family:var(--font-mono);resize:vertical;" placeholder="MATCH Employee RETURN name, salary"></textarea>
          </div>
          <div style="display:flex;gap:8px;margin-bottom:12px;">
            <button class="btn btn-pri" onclick="profileRun()">▶ Profile</button>
            <button class="btn btn-sec btn-sm" onclick="profileExplain()">🔍 Explain KO</button>
            <input type="text" id="profiler-koid-input" placeholder="KOID for EXPLAIN..." style="flex:1;padding:6px 8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:11px;font-family:var(--font-mono);" />
          </div>
        </div>
        <div id="profiler-results"></div>
      </div>

      <!-- ── Provider Manager ── -->
      <div class="panel" id="panel-providers">
        <div class="card" style="margin-bottom:12px;">
          <h3>🔌 Provider Manager</h3>
          <p style="font-size:11px;color:var(--muted);">Connectors bridge aikoql to external data systems. Deploy, monitor, and sync.</p>
        </div>
        <div id="providers-list"><div class="loading">Loading connectors...</div></div>
        <div class="card" style="margin-top:12px;">
          <h4>Deploy New Connector</h4>
          <div style="display:flex;gap:8px;margin-top:8px;flex-wrap:wrap;">
            <input type="text" id="connector-name" placeholder="Connector name" style="padding:6px 8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:11px;flex:1;min-width:120px;" />
            <input type="text" id="connector-plugin" placeholder="Plugin type (e.g. postgres)" style="padding:6px 8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:11px;flex:1;min-width:120px;" />
            <button class="btn btn-pri btn-sm" onclick="deployConnector()">Deploy</button>
          </div>
          <div id="connector-deploy-result" style="margin-top:8px;"></div>
        </div>
      </div>

      <!-- ── Document Explorer ── -->
      <div class="panel" id="panel-documents">
        <div class="card" style="margin-bottom:12px;">
          <h3>📄 Document Explorer</h3>
          <p style="font-size:11px;color:var(--muted);">Ingest documents → Knowledge Objects. Upload a PDF, DOCX, HTML, or TXT file to create queryable knowledge.</p>
        </div>
        <div class="card" style="margin-bottom:12px;">
          <h4>Upload Document</h4>
          <div style="display:flex;gap:8px;margin-top:8px;flex-wrap:wrap;align-items:center;">
            <input type="file" id="doc-file-input" style="padding:6px 8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:11px;flex:1;min-width:200px;" />
            <button class="btn btn-pri btn-sm" onclick="ingestDocument()">Ingest</button>
          </div>
          <div id="doc-ingest-result" style="margin-top:8px;"></div>
        </div>
        <div id="documents-list"><div class="loading">Loading documents...</div></div>
        <div id="doc-compile-result" style="margin-top:12px;"></div>
      </div>

      <!-- ── Administration ── -->
      <div class="panel" id="panel-admin">
        <div class="admin-grid" id="admin-grid"><div class="loading">Loading dashboard...</div></div>
      </div>

      <!-- ── Timeline — MVCC Time Travel ── -->
      <div class="panel" id="panel-timeline">
        <div class="card">
          <h3>⏳ Timeline — MVCC Time Travel</h3>
          <p style="font-size:11px;color:var(--muted);margin-bottom:12px;">Every mutation is a versioned KnowledgeEvent. Enter a KOID to see its full version history.</p>
          <div style="display:flex;gap:8px;margin-bottom:12px;">
            <input type="text" id="timeline-koid-input" placeholder="KOID hex..." style="flex:1;padding:8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:12px;font-family:var(--font-mono);" />
            <button class="btn btn-pri" onclick="loadTimeline()">Load Timeline</button>
          </div>
          <div id="timeline-result"><div class="empty-state">Enter a KOID to view its version timeline.</div></div>
        </div>
      </div>

      <!-- ── Provenance — Cryptographic Audit Chain ── -->
      <div class="panel" id="panel-provenance">
        <div class="card">
          <h3>🔗 Provenance — Cryptographic Audit Chain</h3>
          <p style="font-size:11px;color:var(--muted);margin-bottom:12px;">git log for knowledge. Every version is SHA-256 chained. Independently verifiable.</p>
          <div style="display:flex;gap:8px;margin-bottom:12px;">
            <input type="text" id="provenance-koid-input" placeholder="KOID hex..." style="flex:1;padding:8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:12px;font-family:var(--font-mono);" />
            <button class="btn btn-pri" onclick="loadProvenance()">Trace Provenance</button>
            <button class="btn btn-sec" onclick="verifyProvenance()">🔐 Prove Integrity</button>
          </div>
          <div id="provenance-result"><div class="empty-state">Enter a KOID to trace its provenance chain.</div></div>
        </div>
      </div>

      <!-- ── Program Debugger ── -->
      <div class="panel" id="panel-debugger">
        <div class="card">
          <h3>🐛 Program Debugger</h3>
          <p style="font-size:11px;color:var(--muted);margin-bottom:12px;">Inspect aikoql programs: source code, compiled plan, execution stats, dependency graph.</p>
          <div style="display:flex;gap:8px;margin-bottom:12px;">
            <select id="debugger-program-select" style="flex:1;padding:8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:12px;font-family:var(--font);">
              <option value="">Select a program...</option>
            </select>
            <button class="btn btn-pri" onclick="loadProgramDebugger()">Inspect</button>
          </div>
          <div id="debugger-result"><div class="empty-state">Select a deployed program to inspect.</div></div>
        </div>
      </div>

      <!-- ── Benchmark Center ── -->
      <div class="panel" id="panel-benchmarks">
        <div class="card">
          <h3>⏱ Benchmark Center</h3>
          <p style="font-size:11px;color:var(--muted);margin-bottom:12px;">Versioned, replayable performance benchmarks as Knowledge Objects.</p>
          <div style="display:flex;gap:8px;margin-bottom:12px;">
            <button class="btn btn-pri" onclick="loadBenchmarks()">🔄 Refresh</button>
            <button class="btn btn-sec btn-sm" onclick="showBenchmarkDeployForm()">+ New Benchmark</button>
          </div>
          <div id="benchmark-deploy-form" style="display:none;margin-bottom:12px;">
            <input type="text" id="bm-name" placeholder="Benchmark name" style="width:100%;padding:6px;margin-bottom:6px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:11px;font-family:var(--font);" />
            <textarea id="bm-query" placeholder="Target aikoql query" style="width:100%;min-height:60px;padding:6px;margin-bottom:6px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:11px;font-family:var(--font-mono);"></textarea>
            <div style="display:flex;gap:6px;align-items:center;margin-bottom:6px;">
              <span style="font-size:11px;color:var(--muted);">Iterations:</span>
              <input type="number" id="bm-iterations" value="100" min="1" style="width:80px;padding:4px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:11px;" />
              <span style="font-size:11px;color:var(--muted);">Warmup:</span>
              <input type="number" id="bm-warmup" value="10" min="0" style="width:80px;padding:4px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:11px;" />
            </div>
            <button class="btn btn-pri btn-sm" onclick="deployBenchmark()">Deploy Benchmark</button>
            <span id="bm-deploy-msg" style="margin-left:8px;font-size:11px;"></span>
          </div>
          <div id="benchmarks-result"><div class="loading">Loading benchmarks...</div></div>
        </div>
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
async function apiPost(url, body) {
  const sep = url.includes('?') ? '&' : '?';
  const full = url + (authToken ? sep + 'token=' + encodeURIComponent(authToken) : '');
  const res = await fetch(full, {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify(body||{})});
  if (!res.ok) {
    let msg = 'HTTP ' + res.status;
    try { const b = await res.json(); if (b.error) msg = b.error; } catch(_) {}
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
  const titles = { query:'Query Editor', graph:'Knowledge Graph', explorer:'Knowledge Explorer', schema:'Schema Explorer', ontology:'Ontology', inspector:'KO Inspector', admin:'Administration', timeline:'Timeline', provenance:'Provenance', debugger:'Program Debugger', benchmarks:'Benchmark Center', profiler:'Query Profiler', providers:'Provider Manager', documents:'Document Explorer' };
  document.getElementById('panel-title').textContent = titles[name] || name;
  document.getElementById('panel-breadcrumb').textContent = '';
  if (name === 'graph') initGraph();
  if (name === 'explorer') loadExplorerTree();
  if (name === 'schema') loadSchemaPanel();
  if (name === 'ontology') loadOntologyPanel();
  if (name === 'admin') loadAdminPanel();
  if (name === 'providers') loadProviders();
  if (name === 'debugger') loadDebuggerPrograms();
  if (name === 'benchmarks') loadBenchmarks();
  if (name === 'documents') loadDocuments();
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
    const count = info.count||0;
    if (count === 0) return; // skip empty types — prevents "No objects" dead-end click
    const tenantList = (info.tenants||[]).join(', ');
    html += '<div class="tree-item type-node" onclick="exploreType(\''+typeName.replace(/'/g,"\\'")+'\')" data-type="'+typeName+'">';
    html += '<span style="color:'+colorFor(typeName)+'">●</span> '+typeName;
    html += '<span class="count">'+count+'</span>';
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

  // Semantic (embedding enrichment)
  if (node.semantic) {
    h += '<div class="section-title">Semantic</div>';
    if (node.semantic.embedding_model) h += '<div class="kv-row"><span class="kv-key">Embedding Model</span><span class="kv-val">'+node.semantic.embedding_model+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Embedding Dims</span><span class="kv-val">'+(node.semantic.embedding_dims||0)+'</span></div>';
    if (node.semantic.summary) h += '<div class="kv-row"><span class="kv-key">Summary</span><span class="kv-val">'+node.semantic.summary+'</span></div>';
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
    const [health, metrics, backups, compliance] = await Promise.all([
      api('/health').catch(() => ({status:'offline'})),
      api('/api/v1/metrics-info').then(r => r.data).catch(() => ({})),
      api('/api/v1/backups').then(r => r.data).catch(() => ({backups:[]})),
      api('/api/v1/compliance').then(r => r.data).catch(() => null)
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
    // Compliance card (new)
    if (compliance) {
      const cs = compliance.field_crypto_summary || {};
      html += '<div class="card"><h3>Encryption</h3>';
      html += '<div class="kv-row"><span class="kv-key">Enabled</span><span class="kv-val">'+(compliance.encryption_enabled?'✅ Yes':'⬜ No')+'</span></div>';
      html += '<div class="kv-row"><span class="kv-key">Policies</span><span class="kv-val">'+(compliance.policies_registered||0)+'</span></div>';
      html += '<div class="kv-row"><span class="kv-key">Tenant Keys</span><span class="kv-val">'+(cs.tenant_keys||0)+'</span></div>';
      html += '<div style="font-size:10px;color:var(--muted);margin-top:4px;">Policy types: '+(compliance.policy_types||[]).join(', ')||'none'+'</div>';
      html += '</div>';
    }
    // Backups card (upgraded: actions + correct columns)
    html += '<div class="card"><h3>Backups & Recovery</h3>';
    html += '<div style="display:flex;gap:8px;margin-bottom:10px;">';
    html += '<button class="btn btn-pri btn-sm" onclick="adminCreateBackup()">📦 Create Backup</button>';
    html += '</div>';
    html += '<div id="admin-backup-result" style="margin-bottom:8px;"></div>';
    const blist = backups.backups || [];
    if (blist.length > 0) {
      html += '<table><thead><tr><th>Name</th><th>Objects</th><th>Journal</th><th>Actions</th></tr></thead><tbody>';
      blist.forEach(b => {
        const meta = b.meta || {};
        html += '<tr><td class="mono" style="font-size:10px;">'+b.name+'</td><td>'+(meta.object_count||'?')+'</td><td>'+(meta.journal_seq||'?')+'</td>';
        html += '<td>';
        html += '<button class="btn btn-sec btn-sm" onclick="adminVerifyBackup(\''+b.name+'\')" style="margin-right:4px;">Verify</button>';
        html += '<button class="btn btn-sec btn-sm" onclick="adminRestoreBackup(\''+b.name+'\')">Restore</button>';
        html += '</td></tr>';
      });
      html += '</tbody></table>';
    } else { html += '<div style="color:var(--muted);font-size:11px;">No backups found.</div>'; }
    html += '</div>';
    // Audit card
    html += '<div class="card"><h3>Audit</h3>';
    html += '<div style="font-size:11px;color:var(--muted);margin-bottom:8px;">SHA-256 audit chain. Every mutation is cryptographically verifiable.</div>';
    html += '<button class="btn btn-sec btn-sm" onclick="loadAuditReport()">View Audit Report</button> ';
    html += '<button class="btn btn-sec btn-sm" onclick="loadComplianceReport()">Compliance Detail</button>';
    html += '<div id="admin-audit-result" style="margin-top:8px;"></div>';
    html += '</div>';
    document.getElementById('admin-grid').innerHTML = html;
  } catch(e) { document.getElementById('admin-grid').innerHTML = '<div class="error-text">Failed to load admin dashboard: '+e.message+'</div>'; }
}
async function adminCreateBackup() {
  const div = document.getElementById('admin-backup-result');
  div.innerHTML = '<span style="color:var(--muted);">Creating backup...</span>';
  try {
    const data = await apiPost('/api/v1/backup');
    const d = data.data || data;
    div.innerHTML = '<span style="color:var(--green);">Backup created: '+d.backup+' ('+d.object_count+' objects, verified: '+d.verified+')</span>';
    setTimeout(() => loadAdminPanel(), 1500);
  } catch(e) { div.innerHTML = '<span style="color:var(--red);">'+e.message+'</span>'; }
}
async function adminVerifyBackup(name) {
  const div = document.getElementById('admin-backup-result');
  div.innerHTML = '<span style="color:var(--muted);">Verifying '+name+'...</span>';
  try {
    const data = await apiPost('/api/v1/verify-backup', {backup: name});
    const d = data.data || data;
    div.innerHTML = '<span style="color:'+(d.verified?'var(--green)':'var(--red)')+';">Backup '+name+': '+(d.verified?'✅ Verified':'❌ Invalid')+' (seq '+d.expected_journal_seq+')</span>';
  } catch(e) { div.innerHTML = '<span style="color:var(--red);">'+e.message+'</span>'; }
}
async function adminRestoreBackup(name) {
  const div = document.getElementById('admin-backup-result');
  if (!confirm('Restore backup "'+name+'"? This will replace the current database.')) return;
  div.innerHTML = '<span style="color:var(--orange);">Restoring from '+name+'... server may restart.</span>';
  try {
    const data = await apiPost('/api/v1/restore', {backup: name});
    const d = data.data || data;
    div.innerHTML = '<span style="color:var(--green);">Restored from '+name+'. Recovery point: seq '+(d.recovery_point?.journal_seq||'?')+'</span>';
    setTimeout(() => loadAdminPanel(), 2000);
  } catch(e) { div.innerHTML = '<span style="color:var(--red);">'+e.message+'</span>'; }
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
    const versions = data.versions || data.events || [];
    if (!versions.length) { div.innerHTML = '<div class="empty-state">No versions found for this KOID.</div>'; return; }
    let h = '<div style="font-size:11px;color:var(--muted);margin-bottom:8px;">'+versions.length+' versions</div>';
    h += '<div class="timeline-track">';
    versions.forEach((v, i) => {
      const ts = v.timestamp || v.commit_ts || '?';
      const ver = v.version !== undefined ? 'v' + v.version : '';
      const src = v.source || v.mutation_source || '?';
      h += '<div class="timeline-dot" style="margin-bottom:8px;">';
      h += '<span class="badge badge-cyan">' + ver + '</span> ';
      h += '<span style="font-size:11px;">' + ts + '</span> ';
      h += '<span style="font-size:10px;color:var(--muted);">by ' + src + '</span>';
      h += '</div>';
    });
    h += '</div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
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
    const versions = data.versions || data.events || [];
    if (!versions.length) { div.innerHTML = '<div class="empty-state">No provenance data for this KOID.</div>'; return; }
    let h = '<div class="provenance-chain">';
    versions.forEach((v, i) => {
      h += '<div class="provenance-node" style="border-left:2px solid var(--cyan);padding-left:12px;margin-bottom:12px;">';
      h += '<div style="font-size:12px;font-weight:600;">#' + (i + 1) + ' — v' + (v.version || '?') + '</div>';
      h += '<div style="font-size:11px;color:var(--muted);">' + (v.timestamp || v.commit_ts || '?') + '</div>';
      h += '<div style="font-size:10px;color:var(--muted);">Source: ' + (v.source || v.mutation_source || '?') + '</div>';
      if (v.audit_hash) h += '<div style="font-size:10px;font-family:var(--font-mono);color:var(--green);">SHA-256: ' + v.audit_hash.substring(0, 16) + '...</div>';
      h += '</div>';
    });
    h += '</div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}

async function verifyProvenance() {
  const div = document.getElementById('provenance-result');
  div.innerHTML = '<div class="loading">Verifying audit chain...</div>';
  try {
    const proof = await api('/api/v1/prove');
    const data = proof.data || proof;
    div.innerHTML = '<div class="card" style="border-left:3px solid var(--green);padding:12px;">' +
      '<div style="font-size:14px;font-weight:600;color:var(--green);">✅ Audit Chain Verified</div>' +
      '<div style="font-size:11px;color:var(--muted);margin-top:4px;">Journal seq: ' + (data.journal_seq || '?') + '</div>' +
      '<div style="font-size:10px;font-family:var(--font-mono);color:var(--muted);margin-top:4px;">Head: ' + ((data.head_audit_hash||'').substring(0,32)) + '...</div>' +
      '<div style="font-size:10px;color:var(--muted);margin-top:4px;">Events: ' + ((data.events||[]).length) + ' in chain</div>' +
      '</div>';
  } catch(e) { div.innerHTML = '<div class="error-text">Chain verification failed: '+e.message+'</div>'; }
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
      sel.innerHTML += '<option value="'+p.koid+'">'+p.name+' (v'+p.version+')</option>';
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
    let h = '<div class="card"><h3>'+prog.name+'</h3>';
    h += '<div class="kv-row"><span class="kv-key">KOID</span><span class="kv-val mono">'+prog.koid+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Language</span><span class="kv-val">'+ (prog.language||'aikoql') +'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Version</span><span class="kv-val">'+ (prog.version||'?') +'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Lifecycle</span><span class="kv-val">'+ (prog.lifecycle||'?') +'</span></div>';
    h += '</div>';
    // Show source.
    if (prog.body) {
      h += '<div class="card" style="margin-top:8px;"><h3>Source Code</h3>';
      h += '<pre style="background:var(--bg);padding:10px;border-radius:5px;font-size:12px;font-family:var(--font-mono);overflow-x:auto;">'+prog.body+'</pre>';
      h += '</div>';
    }
    // Execution stats.
    h += '<div class="card" style="margin-top:8px;"><h3>Execution Stats</h3>';
    try {
      const stats = await api('/api/v1/execution-stats');
      const s = stats.data || stats;
      h += '<div class="kv-row"><span class="kv-key">Programs Executed</span><span class="kv-val">'+(s.programs_executed||0)+'</span></div>';
      h += '<div class="kv-row"><span class="kv-key">Total Rows</span><span class="kv-val">'+(s.total_rows_returned||0)+'</span></div>';
      h += '<div class="kv-row"><span class="kv-key">Cache Hit %</span><span class="kv-val">'+(s.cache_hit_pct||0).toFixed(1)+'%</span></div>';
    } catch(e) { h += '<div style="color:var(--muted);font-size:11px;">Stats unavailable</div>'; }
    h += '</div>';
    // Show dependencies.
    h += '<div class="card" style="margin-top:8px;"><h3>Dependencies</h3>';
    h += '<button class="btn btn-sec btn-sm" onclick="loadProgramDeps(\''+prog.koid+'\')">Show Dependency Graph</button>';
    h += '<div id="debugger-deps" style="margin-top:8px;"></div>';
    h += '</div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}

async function loadProgramDeps(koid) {
  const div = document.getElementById('debugger-deps');
  div.innerHTML = '<div class="loading">Loading...</div>';
  try {
    const data = await api('/api/v1/trace/' + encodeURIComponent(koid));
    const versions = (data.data||data).versions || (data.data||data).events || [];
    div.innerHTML = '<div style="font-size:11px;color:var(--muted);">'+versions.length+' versions in trace chain. Program DAG is stored as DEPENDS_ON relationships.</div>';
  } catch(e) { div.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
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
      h += '<td><strong>'+b.name+'</strong></td>';
      h += '<td style="font-family:var(--font-mono);font-size:11px;">'+(b.target_query||'?')+'</td>';
      h += '<td>'+(b.iterations||0)+'</td>';
      h += '<td><span class="badge badge-cyan">'+b.lifecycle+'</span></td>';
      h += '<td><button class="btn btn-sec btn-sm" onclick="runBenchmark(\''+b.koid+'\')">▶ Run</button></td>';
      h += '</tr>';
    });
    h += '</tbody></table>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}

async function runBenchmark(koid) {
  const div = document.getElementById('benchmarks-result');
  div.innerHTML = '<div class="loading">Running benchmark '+koid+'...</div>';
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
    h += '<h3>Benchmark: ' + name + '</h3>';
    h += '<div class="kv-row"><span class="kv-key">Iterations</span><span class="kv-val">' + iterations + '</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Total Time</span><span class="kv-val">' + elapsed + 'ms</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Throughput</span><span class="kv-val">' + opsPerSec + ' ops/s</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Avg Latency</span><span class="kv-val">' + (elapsed / iterations).toFixed(2) + 'ms</span></div>';
    h += '</div>';
    h += '<button class="btn btn-sec btn-sm" style="margin-top:8px;" onclick="loadBenchmarks()">← Back to Benchmarks</button>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
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
    const url = '/api/v1/deploy-benchmark' + (authToken ? '?token='+encodeURIComponent(authToken) : '');
    const res = await fetch(url, {method:'POST', headers:{'Content-Type':'application/json'}, body});
    const data = await res.json();
    if (data.koid) {
      msg.innerHTML = '<span style="color:var(--green);">Deployed: '+data.koid+'</span>';
      document.getElementById('bm-name').value = '';
      document.getElementById('bm-query').value = '';
      setTimeout(() => loadBenchmarks(), 500);
    } else { msg.innerHTML = '<span style="color:var(--red);">'+ (data.error||'Failed') +'</span>'; }
  } catch(e) { msg.innerHTML = '<span style="color:var(--red);">'+e.message+'</span>'; }
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
    h += '<div style="font-size:11px;color:var(--muted);margin-bottom:8px;">' + rows.length + ' rows in ' + (t1-t0).toFixed(1) + 'ms</div>';
    if (rows.length > 0) {
      h += '<table><thead><tr>';
      Object.keys(rows[0]).forEach(k => h += '<th>'+k+'</th>');
      h += '</tr></thead><tbody>';
      rows.slice(0,50).forEach(r => {
        h += '<tr>';
        Object.values(r).forEach(v => h += '<td style="font-size:10px;font-family:var(--font-mono);max-width:200px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;">'+(v===null?'∅':String(v).substring(0,80))+'</td>');
        h += '</tr>';
      });
      h += '</tbody></table>';
      if (rows.length > 50) h += '<div style="font-size:10px;color:var(--muted);margin-top:4px;">Showing 50 of '+rows.length+' rows.</div>';
    }
    h += '</div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
}
async function profileExplain() {
  const koid = document.getElementById('profiler-koid-input').value.trim();
  const div = document.getElementById('profiler-results');
  if (!koid) { div.innerHTML = '<div class="error-text">Enter a KOID for EXPLAIN.</div>'; return; }
  div.innerHTML = '<div class="loading">Explaining...</div>';
  try {
    const data = await api('/api/v1/explain/' + encodeURIComponent(koid));
    const d = data.data || data;
    let h = '<div class="card"><h4>EXPLAIN — '+d.origin+'</h4>';
    h += '<div class="kv-row"><span class="kv-key">KOID</span><span class="kv-val mono">'+d.koid+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Version</span><span class="kv-val">'+d.version+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Source</span><span class="kv-val mono" style="font-size:10px;">'+(d.source||'').substring(0,200)+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Confidence</span><span class="kv-val">'+d.confidence+'</span></div>';
    h += '<div class="kv-row"><span class="kv-key">Verified</span><span class="kv-val">'+d.verified+'</span></div>';
    if (d.evidence && d.evidence.length > 0) {
      h += '<div style="margin-top:8px;font-size:11px;font-weight:600;">Evidence ('+d.evidence.length+'):</div>';
      d.evidence.forEach(e => h += '<div class="kv-row"><span class="kv-key">'+e.rel_type+'</span><span class="kv-val mono">'+e.target+'</span></div>');
    }
    h += '</div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
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
    let h = '<div class="card"><h4>Connectors ('+connectors.length+')</h4><table><thead><tr><th>KOID</th><th>Name</th><th>Plugin</th><th>Status</th></tr></thead><tbody>';
    connectors.forEach(c => {
      const lc = c.lifecycle || '?';
      const badge = lc === 'active' ? 'badge-green' : lc === 'draft' ? 'badge-cyan' : 'badge-muted';
      h += '<tr><td class="mono" style="font-size:10px;">'+c.koid.substring(0,16)+'...</td><td>'+c.name+'</td><td>'+ (c.plugin||'?') +'</td><td><span class="badge '+badge+'">'+lc+'</span></td></tr>';
    });
    h += '</tbody></table></div>';
    div.innerHTML = h;
  } catch(e) { div.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
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
    msg.innerHTML = '<span style="color:var(--green);">Deployed: '+d.koid+'</span>';
    document.getElementById('connector-name').value = '';
    document.getElementById('connector-plugin').value = '';
    setTimeout(() => loadProviders(), 500);
  } catch(e) { msg.innerHTML = '<span style="color:var(--red);">'+e.message+'</span>'; }
}

// ═══════════════════════════════════════════
// Document Explorer (MRFC-0050 Phase D0)
// ═══════════════════════════════════════════
async function loadDocuments() {
  const el = document.getElementById('documents-list');
  try {
    const d = await api('/api/v1/list-documents');
    const docs = (d.documents || d.data?.documents || []);
    if (!docs.length) { el.innerHTML = '<div class="empty-state">No documents ingested yet. Upload one above.</div>'; return; }
    let html = '<table class="data-table"><thead><tr><th>KOID</th><th>Filename</th><th>Type</th><th>Size</th><th>Status</th><th>SHA-256</th><th>Actions</th></tr></thead><tbody>';
    for (const doc of docs) {
      const sz = doc.size_bytes < 1024 ? doc.size_bytes+' B' : doc.size_bytes < 1048576 ? (doc.size_bytes/1024).toFixed(1)+' KB' : (doc.size_bytes/1048576).toFixed(1)+' MB';
      const badge = doc.status === 'ingested' ? '<span class="badge badge-ok">ingested</span>' : '<span class="badge">'+doc.status+'</span>';
      html += '<tr><td class="mono"><a href="#" onclick="switchPanel(\'inspector\');document.getElementById(\'inspector-search-input\').value=\''+doc.koid+'\';inspectorSearch();return false;" style="color:var(--accent);">'+doc.koid.substring(0,16)+'...</a></td><td>'+doc.filename+'</td><td>'+doc.mime_type+'</td><td>'+sz+'</td><td>'+badge+'</td><td class="mono" style="font-size:9px;">'+doc.sha256.substring(0,12)+'...</td><td><button class="btn btn-pri btn-sm" onclick="compileDocument(\''+doc.koid+'\')">Compile</button></td></tr>';
    }
    html += '</tbody></table>';
    el.innerHTML = html;
  } catch(e) { el.innerHTML = '<div class="error-text">'+e.message+'</div>'; }
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
    const sz = d.size_bytes < 1024 ? d.size_bytes+' B' : (d.size_bytes/1024).toFixed(1)+' KB';
    msg.innerHTML = '<span style="color:var(--green);">✓ Ingested: '+d.koid+' ('+d.status+', '+sz+', SHA-256: '+d.sha256.substring(0,16)+'...)</span>';
    fileInput.value = '';
    setTimeout(() => loadDocuments(), 500);
  } catch(e) { msg.innerHTML = '<span style="color:var(--red);">'+e.message+'</span>'; }
}

async function compileDocument(koid) {
  const el = document.getElementById('doc-compile-result');
  el.innerHTML = '<div class="loading">Compiling document... (running D3→D9 pipeline)</div>';
  try {
    const raw = await apiPost('/api/v1/documents/compile', {koid});
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
      return '<span class="badge">'+kind+'</span>';
    };

    let html = '<div class="card" style="margin-bottom:12px;"><h4>🔬 Compilation Results</h4>';

    // ── Stats ──
    html += '<details open><summary><b>Phase Stats</b> ('+r.stats.phases.length+' phases, '+r.stats.total_us+' µs total)</summary>';
    html += '<table class="data-table" style="margin-top:8px;"><thead><tr><th>Phase</th><th>Duration (µs)</th><th>Items</th></tr></thead><tbody>';
    for (const p of r.stats.phases) {
      html += '<tr><td>'+p.phase+'</td><td>'+p.duration_us+'</td><td>'+p.item_count+'</td></tr>';
    }
    html += '</tbody></table></details>';

    // ── Knowledge IR ──
    html += '<details style="margin-top:8px;"><summary><b>Knowledge IR</b> ('+r.ir.entities.length+' entities, '+r.ir.facts.length+' facts, '+r.ir.temporal.length+' temporal)</summary>';
    if (r.ir.entities.length) {
      html += '<table class="data-table" style="margin-top:8px;"><thead><tr><th>Entity</th><th>Class</th><th>Properties</th><th>Confidence</th></tr></thead><tbody>';
      for (const e of r.ir.entities) {
        html += '<tr><td><b>'+e.name+'</b></td><td>'+(e.type_hint||'?')+'</td><td>'+JSON.stringify(e.mentions||[]).substring(0,120)+'</td><td>'+(e.confidence||0).toFixed(2)+'</td></tr>';
      }
      html += '</tbody></table>';
    }
    if (r.ir.facts.length) {
      html += '<div style="margin-top:6px;font-size:11px;color:var(--muted);">Facts: '+r.ir.facts.map(f=>f.statement).join('; ')+'</div>';
    }
    html += '</details>';

    // ── Ontology ──
    html += '<details style="margin-top:8px;"><summary><b>Ontology Proposals</b> ('+r.ontology.classes.length+' classes, '+r.ontology.properties.length+' properties, '+r.ontology.relationships.length+' relationships)</summary>';
    if (r.ontology.classes.length) {
      html += '<div style="margin-top:6px;"><b>Classes:</b> '+r.ontology.classes.map(c=>c.name+' (parent: '+(c.parent||'none')+')').join(', ')+'</div>';
    }
    if (r.ontology.properties.length) {
      html += '<div style="margin-top:4px;"><b>Properties:</b> '+r.ontology.properties.map(p=>p.name+' ('+p.value_type+')').join(', ')+'</div>';
    }
    if (r.ontology.relationships.length) {
      html += '<div style="margin-top:4px;"><b>Relationships:</b> '+r.ontology.relationships.map(r=>r.name+' ('+(r.domain||'?')+' → '+(r.range||'?')+')').join(', ')+'</div>';
    }
    html += '</details>';

    // ── Resolution ──
    html += '<details style="margin-top:8px;"><summary><b>Entity Resolution</b> ('+r.resolution.stats.total_entities+' total: '+r.resolution.stats.matched_count+' matched, '+r.resolution.stats.ambiguous_count+' ambiguous, '+r.resolution.stats.unmatched_count+' unmatched)</summary>';
    if (r.resolution.matched.length) {
      html += '<div style="margin-top:6px;font-size:11px;color:var(--green);">✓ Matched: '+r.resolution.matched.map(m=>m.entity_name+' → '+m.matched_koid).join(', ')+'</div>';
    }
    if (r.resolution.ambiguous.length) {
      html += '<div style="margin-top:4px;font-size:11px;color:var(--orange);">⚠ Ambiguous: '+r.resolution.ambiguous.map(m=>m.entity_name).join(', ')+'</div>';
    }
    if (r.resolution.unmatched.length) {
      html += '<div style="margin-top:4px;font-size:11px;color:var(--red);">✗ Unmatched: '+r.resolution.unmatched.map(m=>m.entity_name).join(', ')+'</div>';
    }
    html += '</details>';

    // ── Commit Plan ──
    html += '<details style="margin-top:8px;"><summary><b>Commit Plan</b> ('+r.commit_plan.stats.total_actions+' actions: '+r.commit_plan.stats.creates+' create, '+r.commit_plan.stats.updates+' update, '+r.commit_plan.stats.skips+' skip, '+r.commit_plan.stats.needs_review+' review, '+r.commit_plan.stats.total_conflicts+' conflicts)</summary>';
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
        html += '<tr><td>'+badgeFor(k)+'</td><td><b>'+p.entity_name+'</b></td><td style="font-size:10px;">'+detail+'</td></tr>';
      }
      html += '</tbody></table>';
    }
    html += '</details>';

    // ── Evidence Trail ──
    html += '<details style="margin-top:8px;"><summary><b>Evidence Trail</b> ('+r.evidence_trail.nodes.length+' nodes)</summary>';
    if (r.evidence_trail.nodes.length) {
      for (const n of r.evidence_trail.nodes) {
        html += '<div style="margin-top:6px;padding:8px;background:var(--bg);border-radius:5px;border-left:3px solid var(--accent);"><span class="badge badge-purple">'+n.phase+'</span> <span style="font-size:11px;">'+n.step+'</span>';
        if (n.entities && n.entities.length) {
          html += '<div style="margin-top:4px;font-size:10px;color:var(--muted);">Entities: '+n.entities.join(', ')+'</div>';
        }
        if (n.source && n.source.length) {
          html += '<div style="margin-top:2px;font-size:10px;color:var(--muted);">Evidence items: '+n.source.length+'</div>';
        }
        html += '</div>';
      }
    }
    html += '</details>';

    // ── Chunks ──
    html += '<details style="margin-top:8px;"><summary><b>Embedded Chunks</b> ('+r.embedded_chunks.length+' chunks)</summary>';
    if (r.embedded_chunks.length) {
      html += '<table class="data-table" style="margin-top:8px;"><thead><tr><th>#</th><th>Structure</th><th>Heading Path</th><th>Preview</th><th>Embedding Dims</th></tr></thead><tbody>';
      for (const ec of r.embedded_chunks) {
        const ch = ec.chunk || ec;
        const hp = (ch.structure && ch.structure.heading_path || []).join(' › ');
        html += '<tr><td>'+(ch.position ? ch.position.chunk_index : 0)+'</td><td>'+(ch.structure ? ch.structure.source_type : '?')+'</td><td style="font-size:10px;">'+hp+'</td><td style="font-size:10px;">'+(ch.text||'').substring(0,80)+'...</td><td>'+(ec.embedding||[]).length+'</td></tr>';
      }
      html += '</tbody></table>';
    }
    html += '</details>';

    html += '</div>'; // card
    el.innerHTML = html;
    el.scrollIntoView({behavior:'smooth'});
  } catch(e) { el.innerHTML = '<div class="error-text">Compile failed: '+e.message+'</div>'; }
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
