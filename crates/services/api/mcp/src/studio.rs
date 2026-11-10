//! Aikoql Studio — The Knowledge OS Desktop.
//!
//! Served at `/studio`. Single-page application: sidebar navigation + 6 panels.
//! Every panel uses the existing REST API — zero new backend endpoints.
//!
//! ponytail: one HTML file, no framework. The two libraries (CodeMirror 6 +
//! the AIKOQL language, vis-network) are vendored under `assets/studio/`,
//! embedded in the binary and served same-origin — no CDN at runtime. The
//! bundles are committed; `scripts/build-studio-assets.sh` rebuilds them.

pub const STUDIO_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Aikoql Studio</title>
<script src="/studio/assets/vis-network.js"></script>
<script src="/studio/assets/editor.js"></script>
<style>
/* theme:start */
:root {
  --bg: #0a0a12;
  --panel: #13131f;
  --border: #252538;
  --border-strong: #3a3a52;
  --text: #c8c8d4;
  --text-strong: #ffffff;
  --muted: #8a8aa8;
  --accent: #6cc7f0;
  --accent-hover: #7dd4f5;
  --on-accent: #0a0a12;
  --on-danger: #ffffff;
  --green: #5cd89c;
  --red: #f0556a;
  --purple: #b89ae0;
  --orange: #f0a060;
  --yellow: #f0d060;
  --btn-sec: #2a2a3c;
  --btn-sec-hover: #35354c;
  --code-bg: #0d0d1a;
  --code-gutter: #0a0a18;
  --accent-rgb: 108, 199, 240;
  --purple-rgb: 184, 154, 224;
  --green-rgb: 92, 216, 156;
  --red-rgb: 240, 85, 106;
  --orange-rgb: 240, 160, 96;
  --overlay: rgba(6, 6, 14, 0.97);
  --scrim: rgba(4, 4, 10, 0.62);
  --glass: rgba(19, 19, 31, 0.95);
  color-scheme: dark;
  --font: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif;
  --mono: 'Cascadia Code', 'Fira Code', 'JetBrains Mono', 'Consolas', monospace;
}
:root[data-theme="light"] {
  --bg: #f4f6fb;
  --panel: #ffffff;
  --border: #d5d9e8;
  --border-strong: #b9bfd6;
  --text: #1b1f33;
  --text-strong: #000000;
  --muted: #565c78;
  --accent: #0b6ea3;
  --accent-hover: #095a86;
  --on-accent: #ffffff;
  --on-danger: #ffffff;
  --green: #167a4a;
  --red: #c22a41;
  --purple: #6a42c0;
  --orange: #a84f08;
  --yellow: #7a6200;
  --btn-sec: #e6e9f4;
  --btn-sec-hover: #d9ddec;
  --code-bg: #fbfcff;
  --code-gutter: #eef0f8;
  --accent-rgb: 11, 110, 163;
  --purple-rgb: 106, 66, 192;
  --green-rgb: 22, 122, 74;
  --red-rgb: 194, 42, 65;
  --orange-rgb: 168, 79, 8;
  --overlay: rgba(244, 246, 251, 0.97);
  --scrim: rgba(20, 24, 40, 0.45);
  --glass: rgba(255, 255, 255, 0.95);
  color-scheme: light;
}
@media (prefers-color-scheme: light) {
  :root:not([data-theme="dark"]) {
  --bg: #f4f6fb;
  --panel: #ffffff;
  --border: #d5d9e8;
  --border-strong: #b9bfd6;
  --text: #1b1f33;
  --text-strong: #000000;
  --muted: #565c78;
  --accent: #0b6ea3;
  --accent-hover: #095a86;
  --on-accent: #ffffff;
  --on-danger: #ffffff;
  --green: #167a4a;
  --red: #c22a41;
  --purple: #6a42c0;
  --orange: #a84f08;
  --yellow: #7a6200;
  --btn-sec: #e6e9f4;
  --btn-sec-hover: #d9ddec;
  --code-bg: #fbfcff;
  --code-gutter: #eef0f8;
  --accent-rgb: 11, 110, 163;
  --purple-rgb: 106, 66, 192;
  --green-rgb: 22, 122, 74;
  --red-rgb: 194, 42, 65;
  --orange-rgb: 168, 79, 8;
  --overlay: rgba(244, 246, 251, 0.97);
  --scrim: rgba(20, 24, 40, 0.45);
  --glass: rgba(255, 255, 255, 0.95);
  color-scheme: light;
  }
}
/* theme:end */
* { margin: 0; padding: 0; box-sizing: border-box; }
select { background: var(--bg); color: var(--text); border: 1px solid var(--border); border-radius: 5px; padding: 5px 8px; font: 12px var(--font); }
body { font-family: var(--font); background: var(--bg); color: var(--text); height: 100vh; display: flex; overflow: hidden; }

/* ── Login ── */
#login-overlay { position: fixed; inset: 0; background: var(--overlay); display: flex; align-items: center; justify-content: center; z-index: 200; }
.login-card { background: var(--panel); border: 1px solid var(--border); border-radius: 12px; padding: 40px 44px; width: 380px; text-align: center; box-shadow: 0 0 80px rgba(var(--accent-rgb),0.06); }
.login-card h1 { font-size: 22px; font-weight: 700; color: var(--accent); margin-bottom: 4px; letter-spacing: -0.5px; }
.login-card .sub { color: var(--muted); font-size: 12px; margin-bottom: 28px; }
.login-card input { width: 100%; padding: 11px 14px; margin-bottom: 10px; background: var(--bg); border: 1px solid var(--border); color: var(--text); border-radius: 6px; font-size: 13px; font-family: var(--font); }
.login-card input:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px rgba(var(--accent-rgb),0.12); }
.login-card button { width: 100%; padding: 11px; background: var(--accent); color: var(--on-accent); border: none; border-radius: 6px; cursor: pointer; font-weight: 600; font-size: 13px; font-family: var(--font); }
.login-card button:hover { background: var(--accent-hover); }
.login-card .err { color: var(--red); font-size: 12px; margin-top: 10px; min-height: 18px; }
.login-card .hint { color: var(--muted); font-size: 11px; margin-top: 16px; line-height: 1.6; }

/* ── Layout ── */
#app { display: none; height: 100vh; width: 100vw; }
#sidebar { width: 52px; min-width: 52px; background: var(--panel); border-right: 1px solid var(--border); display: flex; flex-direction: column; align-items: center; padding-top: 10px; z-index: 10; overflow: hidden; }
#sidebar:hover, #sidebar:has(:focus-visible) { width: 200px; }
#sidebar .logo { font-size: 18px; margin-bottom: 6px; opacity: 0.9; flex-shrink: 0; }
#sidebar .logo-text { display: none; font-size: 13px; font-weight: 700; color: var(--accent); white-space: nowrap; }
#sidebar:hover .logo-text, #sidebar:has(:focus-visible) .logo-text { display: inline; margin-left: 10px; }
#sidebar nav { flex: 1; display: flex; flex-direction: column; gap: 2px; width: 100%; padding: 0 4px; overflow-y: auto; min-height: 0; }
#sidebar nav button { display: flex; align-items: center; width: 100%; padding: 10px 12px; background: none; border: none; color: var(--muted); cursor: pointer; font-size: 12px; font-family: var(--font); border-radius: 6px; white-space: nowrap; text-align: left; transition: all 0.1s; }
#sidebar nav button:hover { background: rgba(var(--accent-rgb),0.08); color: var(--text); }
#sidebar nav button.active { background: rgba(var(--accent-rgb),0.14); color: var(--accent); }
#sidebar nav button .ico { font-size: 16px; width: 24px; text-align: center; flex-shrink: 0; }
#sidebar nav button .lbl { margin-left: 10px; opacity: 0; transition: opacity 0.12s; }
#sidebar:hover nav button .lbl, #sidebar:has(:focus-visible) nav button .lbl { opacity: 1; }
#sidebar .nav-group { display: none; font-size: 11px; font-weight: 600; letter-spacing: 0.6px; text-transform: uppercase; color: var(--muted); padding: 10px 12px 2px; white-space: nowrap; }
#sidebar:hover .nav-group, #sidebar:has(:focus-visible) .nav-group { display: block; }
#sidebar .user-area { padding: 10px 0; border-top: 1px solid var(--border); width: 100%; text-align: center; flex-shrink: 0; }
#sidebar .user-badge { font-size: 11px; color: var(--muted); cursor: pointer; white-space: nowrap; }
#sidebar:hover .user-badge, #sidebar:has(:focus-visible) .user-badge { font-size: 12px; }

/* ── Main ── */
#main { flex: 1; min-width: 0; display: flex; flex-direction: column; overflow: hidden; }
#panel-header { padding: 10px 20px; border-bottom: 1px solid var(--border); display: flex; align-items: center; gap: 10px; min-height: 44px; background: var(--panel); }
#panel-header h2 { font-size: 13px; font-weight: 600; color: var(--accent); letter-spacing: -0.3px; }
#panel-header .breadcrumb { font-size: 11px; color: var(--muted); }
#panel-content { flex: 1; overflow-y: auto; padding: 16px 20px; }
#statusbar { padding: 4px 16px; border-top: 1px solid var(--border); font-size: 11px; color: var(--muted); display: flex; justify-content: space-between; background: var(--panel); min-height: 22px; }
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
.btn-pri { background: var(--accent); color: var(--on-accent); }
.btn-pri:hover { background: var(--accent-hover); }
.btn-sec { background: var(--btn-sec); color: var(--text); }
.btn-sec:hover { background: var(--btn-sec-hover); }
.btn-danger { background: var(--red); color: var(--on-danger); }
.btn-sm { padding: 4px 10px; font-size: 11px; }
.badge { display: inline-block; padding: 2px 8px; border-radius: 10px; font-size: 11px; font-weight: 600; }
.badge-cyan { background: rgba(var(--accent-rgb),0.15); color: var(--accent); }
.badge-purple { background: rgba(var(--purple-rgb),0.15); color: var(--purple); }
.badge-green { background: rgba(var(--green-rgb),0.15); color: var(--green); }
.badge-red { background: rgba(var(--red-rgb),0.15); color: var(--red); }
.badge-orange { background: rgba(var(--orange-rgb),0.15); color: var(--orange); }
.batch-notice { border-color: var(--accent); }
.batch-notice h4 { margin-bottom: 6px; }
.progress { height: 8px; background: var(--bg); border: 1px solid var(--border); border-radius: 4px; overflow: hidden; margin: 8px 0 4px; }
.progress-bar { height: 100%; background: var(--accent); }
.btn:disabled { opacity: 0.45; cursor: not-allowed; }
table { width: 100%; border-collapse: collapse; font-size: 11px; }
th { background: rgba(var(--accent-rgb),0.06); color: var(--accent); padding: 6px 10px; text-align: left; font-weight: 600; position: sticky; top: 0; }
td { padding: 5px 10px; border-bottom: 1px solid var(--border); max-width: 200px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
tr:hover td { background: rgba(var(--accent-rgb),0.03); }
.empty-state { color: var(--muted); text-align: center; padding: 40px 20px; font-size: 12px; font-style: italic; }
.loading { color: var(--muted); font-size: 12px; padding: 20px; }
.error-text { color: var(--red); font-size: 12px; }
.mono { font-family: var(--mono); font-size: 11px; }
.pre-box { background: var(--bg); border: 1px solid var(--border); border-radius: 5px; padding: 12px; font-family: var(--mono); font-size: 11px; white-space: pre-wrap; word-break: break-all; max-height: 300px; overflow-y: auto; color: var(--text); }
.kv-row { display: flex; padding: 3px 0; border-bottom: 1px solid color-mix(in srgb, var(--border) 50%, transparent); font-size: 11px; }
.kv-key { color: var(--muted); width: 100px; flex-shrink: 0; }
.kv-val { color: var(--text); word-break: break-all; }

/* ── Graph panel ── */
#graph-area { height: calc(100vh - 130px); position: relative; border-radius: 8px; overflow: hidden; border: 1px solid var(--border); }
#graph-viz { width: 100%; height: 100%; }
.graph-toolbar { position: absolute; top: 8px; left: 8px; z-index: 5; display: flex; gap: 4px; flex-wrap: wrap; }
.graph-toolbar input, .graph-toolbar select { padding: 5px 8px; background: var(--glass); border: 1px solid var(--border); color: var(--text); border-radius: 4px; font-size: 11px; font-family: var(--font); }
.graph-toolbar input { width: 240px; }
.graph-toolbar input:focus, .graph-toolbar select:focus { outline: none; border-color: var(--accent); }
.legend-box { position: absolute; bottom: 8px; left: 8px; background: var(--glass); padding: 6px 10px; border-radius: 5px; font-size: 11px; max-height: 280px; overflow-y: auto; z-index: 5; }
.legend-item { display: flex; align-items: center; margin: 2px 0; gap: 5px; }
.legend-dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; }

/* ── Query panel ── */
.query-editor-label { font-size: 11px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.5px; margin-bottom: 6px; display: flex; justify-content: space-between; align-items: center; flex-wrap: wrap; gap: 4px 12px; }
#query-editor { border: 1px solid var(--border-strong); border-radius: 8px; overflow: hidden; }
#query-editor:focus-within { border-color: var(--accent); box-shadow: 0 0 0 3px rgba(var(--accent-rgb),0.12); }
#query-editor .cm-editor { background: var(--code-bg); border-radius: 8px; }
#query-editor .cm-editor .cm-scroller { font-family: var(--mono); font-size: 13px; line-height: 1.6; }
#query-editor .cm-editor .cm-content { padding: 8px 0; }
#query-editor .cm-editor .cm-gutters { background: var(--code-gutter); border-right: 1px solid var(--border); color: var(--muted); }
#query-editor .cm-editor .cm-activeLineGutter { background: rgba(var(--accent-rgb),0.06); }
.query-toolbar { display: flex; gap: 6px; margin-top: 10px; align-items: center; }
#query-results { margin-top: 12px; max-height: 400px; overflow-y: auto; }

/* ── Explorer panel ── */
.explorer-layout { display: flex; gap: 12px; height: calc(100vh - 130px); }
.explorer-tree { width: 280px; min-width: 280px; background: var(--panel); border: 1px solid var(--border); border-radius: 8px; overflow-y: auto; padding: 8px 0; }
.explorer-tree .tree-item { padding: 5px 12px 5px 20px; cursor: pointer; font-size: 11px; color: var(--text); display: flex; align-items: center; gap: 6px; border-left: 2px solid transparent; }
.explorer-tree .tree-item:hover { background: rgba(var(--accent-rgb),0.05); }
.explorer-tree .tree-item.selected { background: rgba(var(--accent-rgb),0.1); border-left-color: var(--accent); color: var(--accent); }
.explorer-tree .tree-item.type-node { font-weight: 600; padding-left: 12px; font-size: 12px; }
.explorer-tree .tree-item .count { margin-left: auto; color: var(--muted); font-size: 11px; }
.explorer-tree .tree-header { padding: 6px 12px; font-size: 11px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.5px; }
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
.inspector-search .suggest-item:hover { background: rgba(var(--accent-rgb),0.08); }
.inspector-detail { flex: 1; overflow-y: auto; }
.section-title { font-size: 11px; font-weight: 600; color: var(--accent); margin: 16px 0 6px; padding-bottom: 4px; border-bottom: 1px solid var(--border); text-transform: uppercase; letter-spacing: 0.5px; }
.section-title:first-child { margin-top: 0; }

/* ── Admin panel ── */
.admin-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(340px, 1fr)); gap: 12px; }
.metric-big { font-size: 28px; font-weight: 700; color: var(--accent); }
.metric-label { font-size: 11px; color: var(--muted); }

/* ── Tenant context ── */
.header-spacer { flex: 1; }
.tenant-ctx { font-size: 12px; color: var(--muted); }
#tenant-context { padding: 5px 8px; background: var(--bg); border: 1px solid var(--border); color: var(--text); border-radius: 5px; font-size: 12px; font-family: var(--font); max-width: 200px; }

/* ── Command palette ── */
#palette-backdrop { position: fixed; inset: 0; background: var(--scrim); z-index: 280; }
#palette-backdrop[hidden], #palette[hidden] { display: none; }
#palette { position: fixed; top: 14vh; left: 50%; transform: translateX(-50%); width: min(620px, calc(100vw - 32px)); background: var(--panel); border: 1px solid var(--border); border-radius: 10px; box-shadow: 0 24px 80px rgba(0,0,0,0.6); z-index: 290; overflow: hidden; }
#palette-input { width: 100%; padding: 14px 16px; background: var(--bg); border: none; border-bottom: 1px solid var(--border); color: var(--text); font-size: 15px; font-family: var(--font); }
#palette-input:focus-visible { outline: none; box-shadow: inset 0 -2px 0 var(--accent); }
#palette-list { list-style: none; max-height: min(52vh, 420px); overflow-y: auto; padding: 6px; }
#palette-list li { display: flex; justify-content: space-between; gap: 12px; padding: 9px 12px; border-radius: 6px; cursor: pointer; font-size: 13px; }
#palette-list li[aria-selected="true"] { background: rgba(var(--accent-rgb),0.16); color: var(--text-strong); }
#palette-list li:hover { background: rgba(var(--accent-rgb),0.09); }
.pal-hint { color: var(--muted); font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 55%; }
#palette-list li.pal-empty { cursor: default; color: var(--muted); justify-content: center; }

/* ── Results ── */
.res-bar { display: flex; gap: 6px; margin-bottom: 8px; flex-wrap: wrap; }
.res-count { font-size: 12px; color: var(--muted); margin-top: 6px; }
.res-note { font-size: 12px; color: var(--orange); margin-top: 4px; }
.th-btn { all: unset; cursor: pointer; display: inline-flex; gap: 4px; align-items: center; color: inherit; font: inherit; font-weight: 600; }
.th-btn:hover { text-decoration: underline; }
th[aria-sort="ascending"], th[aria-sort="descending"] { background: rgba(var(--accent-rgb),0.14); }
td a { text-decoration: none; }
td a:hover { text-decoration: underline; }

/* ── Query library ── */
#query-library summary { cursor: pointer; font-size: 13px; font-weight: 600; margin-bottom: 8px; }
.lib-row { display: flex; gap: 8px; align-items: center; margin: 8px 0; }
.lib-row input { flex: 1; padding: 7px 10px; background: var(--bg); border: 1px solid var(--border); color: var(--text); border-radius: 5px; font-size: 13px; font-family: var(--font); }
.lib-head { justify-content: space-between; margin-top: 14px; }
.lib-list { display: flex; flex-direction: column; gap: 2px; max-height: 220px; overflow-y: auto; }
.lib-item { display: flex; align-items: center; gap: 4px; }
.lib-load { flex: 1; text-align: left; padding: 6px 8px; background: none; border: 1px solid transparent; border-radius: 5px; color: var(--text); font-size: 13px; cursor: pointer; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--font); }
.lib-load:hover { background: rgba(var(--accent-rgb),0.08); border-color: var(--border); }
.lib-when { color: var(--muted); font-size: 11px; margin-left: 8px; }

/* ── Notifications ── */
#toasts { position: fixed; right: 16px; bottom: 36px; z-index: 250; display: flex; flex-direction: column; gap: 8px; max-width: min(420px, calc(100vw - 32px)); }
.toast { display: flex; align-items: flex-start; gap: 10px; padding: 10px 12px; background: var(--panel); border: 1px solid var(--border); border-left: 3px solid var(--accent); border-radius: 6px; font-size: 13px; box-shadow: 0 6px 24px rgba(0,0,0,0.4); }
.toast-success { border-left-color: var(--green); }
.toast-error { border-left-color: var(--red); }
.toast span { flex: 1; word-break: break-word; }
.toast-x { background: none; border: none; color: var(--muted); font-size: 16px; line-height: 1; cursor: pointer; padding: 0 2px; }
.toast-x:hover { color: var(--text); }

/* ── Accessibility & orientation ── */
:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 3px; }
.skip { position: fixed; top: -48px; left: 8px; z-index: 300; padding: 8px 14px; background: var(--accent); color: var(--on-accent); border: none; border-radius: 0 0 6px 6px; font-weight: 600; font-size: 12px; cursor: pointer; }
.skip:focus { top: 0; }
#panel-title:focus { outline: none; }
/* wide screens: the rail is always a labelled sidebar, never icons you must hover to decode */
@media (min-width: 1100px) {
  #sidebar { width: 200px; min-width: 200px; align-items: stretch; }
  #sidebar .logo { padding-left: 14px; }
  #sidebar .logo-text, #sidebar .nav-group { display: inline-block; margin-left: 10px; }
  #sidebar .nav-group { display: block; margin-left: 0; }
  #sidebar nav button .lbl { opacity: 1; }
  #sidebar .user-badge { font-size: 12px; }
}
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { animation: none !important; transition: none !important; scroll-behavior: auto !important; }
}
/* ── Phones: the navigation is a drawer ── */
#nav-toggle { display: none; }
@media (max-width: 799px) {
  #nav-toggle { display: inline-flex; align-items: center; justify-content: center; min-width: 40px; min-height: 40px; background: var(--btn-sec); color: var(--text); border: none; border-radius: 6px; font-size: 18px; cursor: pointer; }
  #sidebar, #sidebar:hover, #sidebar:has(:focus-visible) { position: fixed; left: 0; top: 0; bottom: 0; width: 280px; min-width: 280px; align-items: stretch; transform: translateX(-104%); visibility: hidden; transition: transform 0.18s, visibility 0s 0.18s; z-index: 230; box-shadow: 8px 0 32px rgba(0,0,0,0.45); overflow-y: auto; }
  #app.nav-open #sidebar { transform: none; visibility: visible; transition: transform 0.18s; }
  #sidebar .logo { padding-left: 14px; }
  #sidebar .logo-text { display: inline-block; margin-left: 10px; }
  #sidebar .nav-group { display: block; margin-left: 0; }
  #sidebar nav button .lbl { opacity: 1; }
  #sidebar .user-badge { font-size: 12px; }
  #nav-backdrop { position: fixed; inset: 0; background: var(--scrim); z-index: 225; }
  #nav-backdrop[hidden] { display: none; }
  #main { width: 100%; }
  #panel-header { flex-wrap: wrap; padding: 8px 12px; gap: 8px; }
  #panel-content { padding: 12px; }
  .card { overflow-x: auto; }
  .input-row, .query-toolbar, .res-bar, .lib-row { flex-wrap: wrap; }
  .btn { min-height: 40px; }
  /* !important: panel rules such as .lib-row input are more specific, and a phone zooms into anything under 16px */
  input, select, textarea { font-size: 16px !important; min-height: 40px; }
  /* a table wider than the screen scrolls inside the page; it never widens the page */
  #panel-content table { display: block; max-width: 100%; overflow-x: auto; }
  #tenant-context { max-width: 150px; }
  .login-card { width: min(380px, calc(100vw - 32px)); padding: 28px 22px; }
  #graph-area { height: calc(100dvh - 200px); min-height: 320px; }
  .graph-toolbar input { width: 150px; }
  #palette { top: 6vh; }
  .query-editor-label span + span { display: none; }   /* the keyboard-shortcut hint means nothing on a touch screen */
  .header-spacer { flex-basis: 100%; height: 0; }
  #toasts { left: 12px; right: 12px; max-width: none; }
}
</style>
</head>
<body>
<button class="skip" data-click="skipToContent">Skip to content</button>
<div id="toasts" role="region" aria-label="Notifications" aria-live="polite"></div>
<div id="nav-backdrop" data-click="closeNav" hidden></div>
<div id="palette-backdrop" data-click="closePalette" hidden></div>
<div id="palette" role="dialog" aria-modal="true" aria-label="Command palette" hidden>
  <input id="palette-input" role="combobox" aria-expanded="true" aria-controls="palette-list" aria-autocomplete="list" aria-label="Type a command" placeholder="Type a command or search…  (↑ ↓ to move · Enter to run · Esc to close)" autocomplete="off" spellcheck="false">
  <ul id="palette-list" role="listbox" aria-label="Commands"></ul>
</div>

<!-- ── Login ── -->
<div id="login-overlay">
  <div class="login-card">
    <h1>Aikoql Studio</h1>
    <div class="sub" style="margin-bottom:6px;">The Knowledge OS Desktop</div>
    <div class="sub" data-license-status style="margin-bottom:20px;"><!-- LICENSE_STATUS --></div>
    <input type="text" id="login-user" placeholder="Username" autocomplete="off" />
    <input type="password" id="login-pass" placeholder="Password" />
    <button data-click="doLogin">Sign In</button>
    <div class="err" id="login-error"></div>
    <div class="hint">Credentials: [auth].users in aikoql.toml, or the AIKOQL_ADMIN_PASSWORD bootstrap</div>
  </div>
</div>

<!-- ── App ── -->
<div id="app">
  <div id="sidebar">
    <div class="logo">🧠<span class="logo-text">aikoql</span></div>
    <nav aria-label="Main" role="navigation">
      <div class="nav-group" role="presentation">Work</div>
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
      <button data-panel="sources" title="Data Sources — discover external schemas">
        <span class="ico">🗄️</span><span class="lbl">Data Sources</span>
      </button>
      <button data-panel="documents" title="Document Explorer">
        <span class="ico">📄</span><span class="lbl">Documents</span>
      </button>
      <button data-panel="ask" title="Ask — grounded answers from your knowledge">
        <span class="ico">💬</span><span class="lbl">Ask</span>
      </button>
      <div class="nav-group" role="presentation">Govern</div>
      <button data-panel="timeline" title="Timeline — MVCC time travel">
        <span class="ico">⏳</span><span class="lbl">Timeline</span>
      </button>
      <button data-panel="provenance" title="Provenance — cryptographic audit chain">
        <span class="ico">🔗</span><span class="lbl">Provenance</span>
      </button>
      <button data-panel="admin" title="Administration">
        <span class="ico">⚙</span><span class="lbl">Admin</span>
      </button>
      <div class="nav-group" role="presentation">Tune</div>
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
    </nav>
    <div class="user-area">
      <div class="user-badge" data-click="logout" id="user-badge">―</div>
    </div>
  </div>
  <div id="main">
    <div id="panel-header">
      <button id="nav-toggle" data-click="toggleNav" aria-label="Open navigation" aria-expanded="false" aria-controls="sidebar">☰</button>
      <h2 id="panel-title">Query Editor</h2>
      <span class="breadcrumb" id="panel-breadcrumb"></span>
      <span class="header-spacer"></span>
      <label class="tenant-ctx" for="tenant-context">Tenant</label>
      <select id="tenant-context" data-change="setTenantContext" aria-label="Working tenant — applies to every panel"><option value="">Each panel’s default</option></select>
      <button id="theme-toggle" class="btn btn-sec btn-sm" data-click="cycleTheme" aria-label="Theme: automatic, follows your system. Activate to change.">◐ Auto</button>
    </div>
    <div id="panel-content">

      <!-- ── Query Editor ── -->
      <div class="panel active" id="panel-query">
        <div class="card">
          <div class="query-editor-label">
            <span>Aikoql Query</span>
            <span>Ctrl+Enter to run · AS_OF = wall-clock MVCC · BETWEEN = event-time · comma-separated properties · SQL-style highlighting</span>
          </div>
          <div id="query-editor"></div>
          <div class="query-toolbar">
            <button class="btn btn-pri" data-click="runQuery">▶ Run</button>
            <button class="btn btn-sec" data-click="explainQuery">🔍 Explain Plan</button>
            <button class="btn btn-sec btn-sm" data-click="streamQuery" title="Runs the query, then shows the rows in chunks of 100 — paged in the browser (client-side), not a server stream">📄 Chunked</button>
            <select id="query-tenant" style="padding:5px 8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:11px;font-family:var(--font);">
              <option value="">Default tenant</option>
            </select>
          </div>
        </div>
        <div id="query-results"></div>
        <details class="card" id="query-library" open>
          <summary>Saved &amp; recent queries</summary>
          <div class="lib-row">
            <input id="query-save-name" aria-label="Name for the saved query" placeholder="Name this query, then Save (Ctrl/Cmd+S)" maxlength="60" autocomplete="off">
            <button class="btn btn-sec btn-sm" data-click="saveQuery">Save</button>
          </div>
          <div id="query-saved" class="lib-list"></div>
          <div class="lib-row lib-head"><strong>Recent</strong><button class="btn btn-sec btn-sm" data-click="clearHistory">Clear</button></div>
          <div id="query-history" class="lib-list"></div>
        </details>
      </div>

      <!-- ── Knowledge Graph ── -->
      <div class="panel" id="panel-graph">
        <div id="graph-area">
          <div class="graph-toolbar">
            <input type="text" id="graph-search" placeholder="KOID hex or type name..." />
            <select id="graph-tenant" data-change="loadGraph"><option value="">All tenants</option></select>
            <button class="btn btn-pri btn-sm" data-click="loadGraph">Search</button>
            <button class="btn btn-sec btn-sm" data-click="loadGraphAll">View All</button>
          </div>
          <div id="graph-viz"></div>
          <div class="legend-box" id="legend"></div>
        </div>
      </div>

      <!-- ── Knowledge Explorer ── -->
      <div class="panel" id="panel-explorer">
        <div class="explorer-layout">
          <div class="explorer-tree">
            <div class="filter-bar"><input type="text" id="explorer-filter" placeholder="Filter types..." data-input="renderExplorerTree" /></div>
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
          <label style="font-size:11px;color:var(--muted);">Tenant <select id="ontology-tenant" data-change="loadOntologyPanel"><option value="">Global</option></select></label>
          <div id="ontology-status"><div class="loading">Loading ontology...</div></div>
          <div id="ontology-classes"></div>
          <div id="ontology-relationships"></div>
          <div id="ontology-mappings"></div>
        </div>
        <div class="card" id="ontology-reconcile">
          <h3>Reconcile sources</h3>
          <p style="font-size:11px;color:var(--muted);margin-bottom:8px;">When several sources describe the same thing, <b>Resolve</b> merges rows with an exactly equal key into one entity, records where the sources disagree, and settles each disagreement by the authority you declared (Data Sources → Preview → Authority). Nothing is merged on a near match.</p>
          <label style="font-size:11px;color:var(--muted);">Class <select id="reconcile-class" aria-label="Class to reconcile"></select></label>
          <button class="btn btn-pri btn-sm" id="reconcile-run" data-click="resolveClass">Resolve</button>
          <div id="reconcile-result" role="status" aria-live="polite"></div>
        </div>
        <details class="card">
          <summary><b>Advanced: Create / Replace Ontology</b> <span style="font-size:11px;color:var(--muted);">(raw definition)</span></summary>
          <p style="font-size:11px;color:var(--muted);margin-bottom:8px;">Paste an ontology definition as YAML-like JSON. Saves to the tenant chosen above (Global by default); an existing ontology there is replaced.</p>
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
          <button class="btn btn-pri" data-click="saveOntology" style="margin-top:8px;">Save Ontology</button>
          <button class="btn btn-sec" data-click="discoverOntology" style="margin-top:8px;margin-left:4px;">🔍 Auto-Discover</button>
          <span id="ontology-save-msg" style="margin-left:12px;font-size:11px;"></span>
        </details>
      </div>

      <!-- ── KO Inspector ── -->
      <div class="panel" id="panel-inspector">
        <div class="inspector-layout">
          <div class="inspector-search">
            <input type="text" id="inspector-search-input" placeholder="Search by KOID, type, or keyword..." data-input="inspectorSearch" />
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
            <button class="btn btn-pri" data-click="profileRun">▶ Profile</button>
            <button class="btn btn-sec btn-sm" data-click="profileExplain">🔍 Explain KO</button>
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
            <button class="btn btn-pri btn-sm" data-click="deployConnector">Deploy</button>
          </div>
          <div id="connector-deploy-result" style="margin-top:8px;"></div>
        </div>
      </div>

      <!-- ── Document Explorer ── -->
      <div class="panel" id="panel-documents">
        <div class="card" style="margin-bottom:12px;">
          <h3>📄 Document Explorer</h3>
          <p style="font-size:11px;color:var(--muted);">Ingest documents → Knowledge Objects. Upload a PDF, DOCX, XLSX, PPTX, CSV, TSV, HTML, Markdown or TXT file, or an image (PNG, JPEG, TIFF, BMP, GIF, WebP), to create queryable knowledge. Spreadsheet sheets, slide tables and tables found in PDFs and scans become structured tables. Text in images is read by OCR when Tesseract is installed on the server; PDF table detection uses poppler's pdftotext when installed.</p>
        </div>
        <div class="card" style="margin-bottom:12px;">
          <h4>Upload Document</h4>
          <div style="display:flex;gap:8px;margin-top:8px;flex-wrap:wrap;align-items:center;">
            <input type="file" id="doc-file-input" accept=".pdf,.docx,.xlsx,.pptx,.csv,.tsv,.html,.htm,.md,.txt,.png,.jpg,.jpeg,.tif,.tiff,.bmp,.gif,.webp" style="padding:6px 8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:11px;flex:1;min-width:200px;" />
            <button class="btn btn-pri btn-sm" data-click="ingestDocument">Ingest</button>
          </div>
          <div id="doc-ingest-result" style="margin-top:8px;"></div>
        </div>
        <div id="documents-list"><div class="loading">Loading documents...</div></div>
        <div id="doc-compile-result" style="margin-top:12px;"></div>
      </div>

      <!-- ── Data Sources ── -->
      <div class="panel" id="panel-sources">
        <div class="card" style="margin-bottom:12px;">
          <h3>🗄️ Data Sources</h3>
          <p style="font-size:11px;color:var(--muted);">External databases the operator configured in <code>aikoql.toml</code>. <b>Discover</b> reads the schema only (no rows) and records it as knowledge. <b>Preview</b> proposes one ontology class per table; merging and <b>Publish</b> are explicit, and what you wrote by hand is never changed. <b>Import</b> copies chosen tables as objects.</p>
          <label style="font-size:11px;color:var(--muted);">Tenant <select id="sources-tenant" data-change="loadSourcesPanel"><option value="">Global</option></select></label>
        </div>
        <div id="sources-list"><div class="loading">Loading sources...</div></div>
        <div class="card" style="margin:12px 0;">
          <button class="btn btn-sec btn-sm" data-click="previewSourceOntology">Preview ontology for the checked sources</button>
          <div id="sources-msg" style="margin-top:8px;font-size:12px;" role="status"></div>
        </div>
        <div id="sources-result"></div>
        <div id="sources-preview"></div>
      </div>

      <!-- ── Ask ── -->
      <div class="panel" id="panel-ask">
        <div class="card" style="margin-bottom:12px;">
          <h3>💬 Ask</h3>
          <p style="font-size:11px;color:var(--muted);">Ask a question about your knowledge. The answer is written by the configured model from facts the server retrieves for the selected tenant, and is refused when those facts do not support it.</p>
          <div id="ask-disabled" style="display:none;font-size:12px;color:var(--orange);">No model configured — see docs/CHAT.md</div>
          <div id="ask-model" style="font-size:11px;color:var(--muted);"></div>
        </div>
        <div class="card" id="ask-form" style="margin-bottom:12px;">
          <label style="font-size:11px;color:var(--muted);">Tenant <select id="ask-tenant"><option value="">All tenants</option></select></label>
          <textarea id="ask-question" rows="3" maxlength="2000" placeholder="Ask a question… (Ctrl/Cmd+Enter to send)" style="width:100%;margin-top:8px;padding:8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:12px;box-sizing:border-box;"></textarea>
          <div style="margin-top:8px;"><button class="btn btn-pri btn-sm" id="ask-submit" data-click="askQuestion">Ask</button></div>
        </div>
        <div id="ask-result"></div>
      </div>

      <!-- ── Administration ── -->
      <div class="panel" id="panel-admin">
        <div class="admin-grid" id="admin-grid"><div class="loading">Loading dashboard...</div></div>
        <!-- AUDIT_EXPORT -->
        <div class="card" id="users-panel" data-users-panel style="display:none;">
          <h3>👥 Users</h3>
          <p style="font-size:11px;color:var(--muted);margin-bottom:12px;">Pre-provision an IdP subject, or edit roles for an existing user. Roles: admin, viewer.</p>
          <div style="display:flex;gap:8px;margin-bottom:8px;">
            <input type="text" id="user-sub" placeholder="sub (IdP subject)" style="flex:1;padding:8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:12px;font-family:var(--font-mono);" />
            <input type="text" id="user-name" placeholder="Name" style="flex:1;padding:8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:12px;font-family:var(--font-mono);" />
            <input type="text" id="user-roles" placeholder="roles (comma separated)" style="flex:1;padding:8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:12px;font-family:var(--font-mono);" />
            <button class="btn btn-pri" data-click="createUser">Pre-provision</button>
          </div>
          <div id="users-msg"></div>
          <div id="users-list"></div>
        </div>
      </div>

      <!-- ── Timeline — MVCC Time Travel ── -->
      <div class="panel" id="panel-timeline">
        <div class="card">
          <h3>⏳ Timeline — MVCC Time Travel</h3>
          <p style="font-size:11px;color:var(--muted);margin-bottom:12px;">Every mutation is a versioned KnowledgeEvent. Enter a KOID to see its full version history.</p>
          <div style="display:flex;gap:8px;margin-bottom:12px;">
            <input type="text" id="timeline-koid-input" placeholder="KOID hex..." style="flex:1;padding:8px;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:5px;font-size:12px;font-family:var(--font-mono);" />
            <button class="btn btn-pri" data-click="loadTimeline">Load Timeline</button>
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
            <button class="btn btn-pri" data-click="loadProvenance">Trace Provenance</button>
            <button class="btn btn-sec" data-click="verifyProvenance">🔐 Prove Integrity</button>
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
            <button class="btn btn-pri" data-click="loadProgramDebugger">Inspect</button>
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
            <button class="btn btn-pri" data-click="loadBenchmarks">🔄 Refresh</button>
            <button class="btn btn-sec btn-sm" data-click="showBenchmarkDeployForm">+ esc(New Benchmark</button>)
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
            <button class="btn btn-pri btn-sm" data-click="deployBenchmark">Deploy Benchmark</button>
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

<script src="/studio/assets/app.js"></script>
</body>
</html>"##;

/// The vendored Studio scripts, served at `/studio/assets/*` (see the module
/// doc). Embedded so a deployed binary needs no network to render Studio.
pub(crate) const APP_JS: &str = include_str!("../assets/studio/app.js");
pub(crate) const EDITOR_JS: &str = include_str!("../assets/studio/editor.js");
pub(crate) const VIS_JS: &str = include_str!("../assets/studio/vis-network.js");

/// Headers for the Studio page itself (`GET /` and `/studio`). Everything the
/// page loads is same-origin (vendored scripts, `/api/*`), so scripts and
/// connections are confined to the origin: an injected `<script src=evil>` or
/// an `fetch('https://evil')` exfiltration is blocked. The page has no inline
/// script and no inline event handler (delegated `data-click` actions, see
/// `ACTIONS` in app.js), so `script-src 'self'` also blocks injected inline
/// script — esc() and its lint remain the first line, this is the backstop.
/// `style-src` keeps `'unsafe-inline'`: the page carries hundreds of inline
/// style attributes, and CSS cannot execute code. No forms, `<base>`, frames
/// or plugins are used, and Studio is never framed; its token rides a header,
/// and Referrer-Policy keeps URLs from leaking anyway.
pub(crate) const STUDIO_HEADERS: &[(&str, &str)] = &[
    (
        "Content-Security-Policy",
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'; \
         base-uri 'none'; object-src 'none'; form-action 'self'",
    ),
    ("X-Frame-Options", "DENY"),
    ("Referrer-Policy", "no-referrer"),
    ("X-Content-Type-Options", "nosniff"),
];

/// Escape text for an HTML text/attribute context.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The Studio page. The served HTML always carries the Task 9 markers: the
/// login card license line (`data-license-status`) and the admin users panel
/// anchor (`data-users-panel`). Content varies: the line renders "Community"
/// without a license, or `org · expires YYYY-MM-DD` with one; the SSO button
/// (`data-sso-button`) appears only when OIDC is licensed AND configured; the
/// audit export card (`data-audit-export`) only under the audit-export flag.
pub(crate) fn studio_html(sso: bool, license: Option<&crate::license::LicenseState>) -> String {
    let status = match license {
        Some(l) => {
            let mut line = l.org.clone();
            let flags = crate::license::feature_flags(&l.features);
            if !flags.is_empty() {
                line.push_str(" · ");
                line.push_str(&flags.join(", "));
            }
            if let Some(e) = l.expires {
                line.push_str(&format!(" · expires {}", crate::license::expiry_date(e)));
            }
            line
        }
        None => "Community".to_string(),
    };
    let mut html = STUDIO_HTML.replace("<!-- LICENSE_STATUS -->", &html_escape(&status));
    if sso {
        let anchor = format!("<button data-click=\"doLogin\">Sign In</button>{SSO_LOGIN_HTML}");
        html = html.replace("<button data-click=\"doLogin\">Sign In</button>", &anchor);
        debug_assert!(
            html.contains("data-sso-button"),
            "the studio SSO anchor moved"
        );
    }
    if license.is_some_and(|l| l.features.audit_export) {
        html = html.replace("<!-- AUDIT_EXPORT -->", AUDIT_EXPORT_HTML);
    }
    html
}

/// Injected only under the audit-export flag: the admin-panel download card
/// (the download is a header-authenticated fetch + Blob, see app.js).
const AUDIT_EXPORT_HTML: &str = r#"<div class="card" data-audit-export>
  <h3>🧾 Audit Export</h3>
  <p style="font-size:11px;color:var(--muted);margin-bottom:12px;">Download the retention-trimmed audit log as JSONL.</p>
  <button class="btn btn-sec" data-click="downloadAuditExport">⬇ Download audit log</button>
</div>"#;

/// Injected only with SSO active: the IdP button. The callback hand-off (a
/// URL-fragment read) lives in app.js and runs only when this button exists.
const SSO_LOGIN_HTML: &str =
    r#"<button data-sso-button data-click="signInSso">Sign in with SSO</button>"#;

/// The whole Studio page as a reviewer sees it: the HTML shell, every script
/// the shell loads from this binary, and the snippets injected per licence.
/// Tests assert on this, never on `STUDIO_HTML` alone, so moving code between
/// files cannot turn an assertion vacuous (an absence check on an empty
/// string passes forever).
#[cfg(test)]
pub(crate) fn page_for_tests() -> &'static str {
    static PAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PAGE.get_or_init(|| [STUDIO_HTML, APP_JS, SSO_LOGIN_HTML, AUDIT_EXPORT_HTML].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::license::{FeatureSet, LicenseState};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn license(flags: &[&str], expires: Option<SystemTime>) -> LicenseState {
        LicenseState {
            org: "acme-corp".into(),
            expires,
            features: FeatureSet::from_flags(
                &flags.iter().map(|f| f.to_string()).collect::<Vec<_>>(),
            ),
        }
    }

    // M1 (Studio hardening): every value the Studio puts into markup comes from
    // stored data or an API reply — KO fields, type/tenant names, ontology
    // text, error messages (a compiler error echoes the query) — and must go
    // through esc(). The JS lives in a Rust string, so the string is what these
    // source-level lints check, in the style of the other marker tests here.
    //
    // The lint is exhaustive, not a deny-list: in any expression that
    // concatenates a markup literal (one containing '<' or '>'), every other
    // operand must be esc(...), a variable that already holds built markup
    // (MARKUP_VARS), or a `.map(cb).join(..)` whose callback itself builds
    // markup (checked recursively). Numbers are wrapped too — harmless.

    /// Variables that hold markup built (and escaped) by the Studio itself.
    const MARKUP_VARS: &[&str] = &["h", "html", "detail", "hp", "rows", "lh"];

    /// Split `s` at `+` operators that are outside quotes and brackets.
    fn plus_operands(s: &str) -> Vec<String> {
        let (mut ops, mut cur) = (Vec::new(), String::new());
        let (mut quote, mut escaped, mut depth) = (None::<char>, false, 0i32);
        let cs: Vec<char> = s.chars().collect();
        for (i, &c) in cs.iter().enumerate() {
            if let Some(q) = quote {
                cur.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
                continue;
            }
            match c {
                '\'' | '"' | '`' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '(' | '[' | '{' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' | ']' | '}' => {
                    depth -= 1;
                    cur.push(c);
                }
                '+' if depth <= 0 && !matches!(cs.get(i + 1), Some('+' | '=')) => {
                    ops.push(std::mem::take(&mut cur));
                }
                _ => cur.push(c),
            }
        }
        ops.push(cur);
        ops
    }

    /// Split at `;`, `{`, `}` and `,` outside quotes and `()`/`[]` — the
    /// statement and argument boundaries.
    fn statements(s: &str) -> Vec<String> {
        let (mut out, mut cur) = (Vec::new(), String::new());
        let (mut quote, mut escaped, mut depth) = (None::<char>, false, 0i32);
        for c in s.chars() {
            if let Some(q) = quote {
                cur.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
                continue;
            }
            match c {
                '\'' | '"' | '`' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '(' | '[' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' | ']' => {
                    depth -= 1;
                    cur.push(c);
                }
                ';' | '{' | '}' | ',' if depth <= 0 => out.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            }
        }
        out.push(cur);
        out
    }

    /// Drop a leading `lhs =`, `lhs +=`, `x =>`, `return` or `key:` so a
    /// leading string literal shows (scans outside quotes and brackets only).
    fn strip_prefix(op: &str) -> &str {
        let b = op.as_bytes();
        let (mut quote, mut escaped, mut depth, mut cut) = (None::<u8>, false, 0i32, 0usize);
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if let Some(q) = quote {
                if escaped {
                    escaped = false;
                } else if c == b'\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
                i += 1;
                continue;
            }
            match c {
                b'\'' | b'"' | b'`' if depth == 0 => break, // the first literal operand
                b'\'' | b'"' | b'`' => quote = Some(c),
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'=' if depth == 0 => {
                    let prev = if i > 0 { b[i - 1] } else { b' ' };
                    let next = b.get(i + 1).copied().unwrap_or(b' ');
                    if next == b'>' {
                        cut = i + 2;
                    } else if next != b'=' && !matches!(prev, b'=' | b'!' | b'<' | b'>') {
                        cut = i + 1;
                    }
                }
                b':' if depth == 0 => cut = i + 1,
                _ => {}
            }
            i += 1;
        }
        let t = op[cut..].trim_start();
        t.strip_prefix("return ").unwrap_or(t)
    }

    fn is_markup_literal(op: &str) -> bool {
        let t = strip_prefix(op).trim_start_matches(['(', ' ']);
        let Some(q) = t.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
            return false;
        };
        // the literal's own text: up to its closing, unescaped quote
        let (mut esc, mut body) = (false, String::new());
        for c in t[1..].chars() {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                break;
            }
            body.push(c);
        }
        body.contains('<') || body.contains('>')
    }

    fn is_literal(op: &str) -> bool {
        let t = strip_prefix(op).trim_start_matches(['(', ' ']);
        t.starts_with('\'') || t.starts_with('"')
    }

    /// Bracketed groups directly inside `op` (their inner text).
    fn groups(op: &str) -> Vec<String> {
        let (mut out, mut cur) = (Vec::new(), String::new());
        let (mut quote, mut escaped, mut depth) = (None::<char>, false, 0i32);
        for c in op.chars() {
            if let Some(q) = quote {
                if depth > 0 {
                    cur.push(c);
                }
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
                continue;
            }
            match c {
                '\'' | '"' | '`' => {
                    quote = Some(c);
                    if depth > 0 {
                        cur.push(c);
                    }
                }
                '(' | '[' | '{' => {
                    if depth > 0 {
                        cur.push(c);
                    }
                    depth += 1;
                }
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(std::mem::take(&mut cur));
                    } else {
                        cur.push(c);
                    }
                }
                _ if depth > 0 => cur.push(c),
                _ => {}
            }
        }
        out
    }

    fn lint_expr(expr: &str, bad: &mut Vec<String>) {
        for st in statements(expr) {
            lint_stmt(&st, bad);
        }
    }

    fn lint_stmt(expr: &str, bad: &mut Vec<String>) {
        let ops = plus_operands(expr);
        let markup = ops.len() > 1 && ops.iter().any(|o| is_markup_literal(o));
        for (k, op) in ops.iter().enumerate() {
            let t = op.trim();
            let core = t.trim_end_matches([';', ')', ',', '}', ' ']);
            if markup && !is_literal(t) && !core.is_empty() {
                let unwrapped = core.trim_start_matches(['(', ' ']);
                let first = k == 0;
                let name = core.trim_start_matches('(');
                // the first operand's statement prefix (`h += `, `x =>`) is not a value
                let value = if first { strip_prefix(t).trim() } else { t };
                let value = value.trim_end_matches([';', ',', '}', ' ']);
                let ok = value.is_empty()
                    || value.starts_with("esc(")
                    || MARKUP_VARS.contains(&value.trim_end_matches(')'))
                    || MARKUP_VARS.contains(&name)
                    || (value.contains(".join(") && value.contains("=>") && {
                        // an HTML-producing map/join: its callback must build markup
                        // (and is linted below); a text join must be esc()-wrapped
                        groups(value)
                            .iter()
                            .any(|g| g.contains("=>") && g.contains('<'))
                    });
                if !ok
                    && !unwrapped.is_empty()
                    && !value.starts_with('\'')
                    && !value.starts_with('"')
                {
                    bad.push(value.chars().take(70).collect());
                }
            }
            for g in groups(op) {
                lint_expr(&g, bad);
            }
        }
    }

    /// Join physical lines that continue a `+` chain into logical lines.
    fn logical_lines(js: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut carry = false;
        for l in js.lines() {
            let t = l.trim();
            if carry {
                if let Some(last) = out.last_mut() {
                    last.push(' ');
                    last.push_str(t);
                }
            } else {
                out.push(t.to_string());
            }
            carry = t.ends_with('+') && !t.ends_with("++");
        }
        out
    }

    #[test]
    fn studio_js_escapes_untrusted_values_before_markup() {
        // The scanned text IS the app script — and it must be a lot of it, so
        // a path change can never leave this lint checking nothing.
        assert!(APP_JS.contains("function esc(") && APP_JS.len() > 60_000);
        let mut bad = Vec::new();
        for line in logical_lines(APP_JS) {
            if line.starts_with("//") || !line.contains('<') {
                continue;
            }
            lint_expr(&line, &mut bad);
        }
        bad.sort();
        bad.dedup();
        assert!(
            bad.is_empty(),
            "{} operands concatenated into markup without esc(): {bad:#?}",
            bad.len()
        );
    }

    #[test]
    fn documents_panel_lists_the_extractable_formats() {
        let page = page_for_tests();
        for fmt in [
            "PDF", "DOCX", "XLSX", "PPTX", "CSV", "TSV", "HTML", "TXT", "PNG",
        ] {
            assert!(
                page.contains(&format!("{fmt},")) || page.contains(&format!("{fmt} ")),
                "Documents panel does not list {fmt}"
            );
        }
        assert!(
            page.contains("accept=\""),
            "the file picker offers the supported types"
        );
    }

    #[test]
    fn studio_handlers_take_data_from_dataset_not_js_strings() {
        // `onclick="f(\''+x+'\')"` splices data into a JS string inside an
        // attribute: a `'` or entity in x breaks out. Handlers read
        // this.dataset.* from an esc()'d data-* attribute instead (renderUsers).
        let src: String = page_for_tests()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let n = src.matches("\\''+").count();
        assert_eq!(n, 0, "{n} inline handlers splice data into JS strings");
    }

    #[test]
    fn auto_discover_previews_before_it_saves() {
        // Discovery output is a proposal: the button shows it and an explicit
        // second action saves (it must never silently become the ontology).
        assert!(
            page_for_tests().contains("{preview: !save}"),
            "discoverOntology must request a preview unless saving"
        );
        assert!(
            page_for_tests().contains("discoverOntology(true)"),
            "the preview must offer an explicit Save action"
        );
    }

    #[test]
    fn the_ontology_workbench_declares_through_the_publish_flow_and_keeps_raw_json_advanced() {
        let page = page_for_tests();
        // keys, links and authority ride along with BOTH the preview and the publish,
        // so the draft the reviewer sees is the draft that is saved
        let body_uses = page.matches("declaredBody(").count();
        assert!(
            body_uses >= 3,
            "the definition, the preview and the publish must all use declaredBody"
        );
        for field in ["identities", "relationships", "assertions"] {
            let d = page.find("function declaredBody(").expect("declaredBody");
            let window: String = page[d..].chars().take(700).collect();
            assert!(window.contains(field), "{field}");
        }
        // reconciling goes through the one route and lists contradictions by class
        let r = page
            .find("async function resolveClass()")
            .expect("resolveClass");
        let body: String = page[r..].chars().take(1400).collect();
        assert!(body.contains("/api/v1/sources/resolve"), "{body}");
        assert!(body.contains("aikoql:contradiction"), "{body}");
        // the raw definition editor is a collapsed, advanced section
        let d = page
            .find("<details class=\"card\">")
            .expect("advanced section");
        let y = page.find("id=\"ontology-yaml-input\"").expect("editor");
        assert!(
            d < y && !page[d..y].contains("</details>"),
            "the editor lives inside <details>"
        );
        assert!(!page[d..d + 40].contains("open"), "collapsed by default");
        for a in [
            "toggleIdentity",
            "acceptLink",
            "removeLink",
            "addAssertion",
            "removeAssertion",
            "resolveClass",
        ] {
            assert!(
                page.contains(&format!("data-click=\"{a}\"")),
                "{a} is used by the markup"
            );
            assert!(
                page.contains(&format!("  {a}: el =>")),
                "{a} is in the closed ACTIONS table"
            );
        }
    }

    #[test]
    fn compile_asks_for_a_view_a_browser_can_hold() {
        // a 1,446-page book compiles to 83 MB; the page must never request that
        let page = page_for_tests();
        let c = page
            .find("async function compileDocument(")
            .expect("compileDocument");
        let body: String = page[c..].chars().take(700).collect();
        assert!(body.contains("/api/v1/documents/compile"), "{body}");
        assert!(
            body.contains("limit:") && body.contains("max_text:"),
            "{body}"
        );
    }

    #[test]
    fn ontology_panel_reads_the_active_ontology_and_saves_with_cas() {
        // The panel shows what the compiler uses (GET /api/v1/ontology, not
        // results[0] of a MATCH) and saves in place under expected_version.
        assert!(
            !page_for_tests().contains("MATCH%20Ontology"),
            "the panel must not pick an arbitrary Ontology row"
        );
        let a = page_for_tests()
            .find("async function saveOntology()")
            .expect("saveOntology");
        let body = &page_for_tests()[a..a + 1800];
        assert!(body.contains("/api/v1/ontology"), "{body}");
        assert!(body.contains("expected_version"), "{body}");
        assert!(!body.contains("/api/v1/remember"), "{body}");
    }

    #[test]
    fn studio_sends_the_session_token_in_a_header_never_a_url() {
        // A token in a URL lands in browser history, proxy/server logs and
        // (before the Referrer-Policy) Referer headers. Every Studio request
        // authenticates with `Authorization: Bearer`; the audit download is a
        // fetch + Blob, not a navigation to `?token=`.
        assert!(
            !page_for_tests().contains("token="),
            "Studio still builds a ?token= URL"
        );
        assert!(
            page_for_tests().contains("'Authorization': 'Bearer '"),
            "no Authorization header helper"
        );
        assert!(
            !page_for_tests().contains("location.href = '/api/audit-export"),
            "the audit export is a navigation"
        );
    }

    #[test]
    fn studio_loads_no_third_party_scripts() {
        // Supply chain + air-gap: the editor and the graph library are vendored
        // into the binary and served same-origin, not fetched from a CDN.
        assert!(
            !page_for_tests().contains("https://") && !page_for_tests().contains("http://"),
            "the page references an external origin"
        );
        for asset in ["/studio/assets/editor.js", "/studio/assets/vis-network.js"] {
            assert!(page_for_tests().contains(asset), "{asset} not referenced");
        }
        assert!(
            !page_for_tests().contains("type=\"module\""),
            "inline module script remains"
        );
    }

    #[test]
    fn user_admin_is_probed_only_when_the_licence_includes_it() {
        // Community mode has no /api/users: asking anyway logs a 404 in the
        // browser console on every login. Ask /api/license first.
        let a = page_for_tests()
            .find("async function loadUsers()")
            .expect("loadUsers");
        let body = &page_for_tests()[a..a + 700];
        let lic = body.find("/api/license").expect("no licence check");
        let users = body.find("/api/users").expect("no users call");
        assert!(lic < users, "{body}");
        assert!(body.contains("multi-user"), "{body}");
    }

    #[test]
    fn the_escape_lint_is_not_bypassable_by_template_literals() {
        // The lint understands `+` concatenation. A markup template literal
        // (`<b>${x}</b>`) would slip past it, so the page may not use any until
        // the lint learns them.
        assert!(
            !page_for_tests().contains('`'),
            "a template literal appeared in the Studio JS — teach studio_js_escapes_* about ${{}}"
        );
    }

    #[test]
    fn the_chunked_view_does_not_claim_to_stream() {
        // streamQuery() runs the whole query and pages the rows in the browser.
        assert!(
            !page_for_tests().contains("📡 Stream")
                && !page_for_tests().contains("Stream results in chunks"),
            "the button still advertises streaming"
        );
        assert!(page_for_tests().contains("client-side"), "no honest label");
    }

    #[test]
    fn the_page_helper_contains_the_whole_app() {
        // If code moves out of STUDIO_HTML, page_for_tests() must follow it;
        // this fails loudly instead of letting absence checks go vacuous.
        let page = page_for_tests();
        for marker in [
            "function esc(",
            "async function saveOntology(",
            "function renderUsers(",
            "<title>Aikoql Studio</title>",
            "data-sso-button",
            "data-audit-export",
        ] {
            assert!(page.contains(marker), "page_for_tests() lost {marker}");
        }
        assert!(
            page.len() > 60_000,
            "page is suspiciously small: {}",
            page.len()
        );
    }

    #[test]
    fn studio_has_no_inline_event_handlers_or_inline_scripts() {
        // CSP `script-src 'self'` blocks inline handlers and inline <script>:
        // a control still carrying one would silently do nothing. Handlers are
        // `data-click` / `data-change` / `data-input` names resolved by one
        // delegated listener against a fixed allowlist (ACTIONS).
        let page = page_for_tests();
        let events = [
            "click",
            "dblclick",
            "change",
            "input",
            "keydown",
            "keyup",
            "keypress",
            "submit",
            "load",
            "error",
            "focus",
            "blur",
            "mouseover",
            "mouseout",
            "mouseenter",
            "mouseleave",
            "mousedown",
            "mouseup",
            "contextmenu",
            "paste",
            "select",
            "toggle",
            "scroll",
            "wheel",
            "resize",
        ];
        let mut bad = Vec::new();
        for ev in events {
            for pat in [format!(" on{ev}="), format!(" on{ev} =")] {
                for (i, _) in page.match_indices(&pat) {
                    bad.push(page[i..(i + 60).min(page.len())].replace('\n', " "));
                }
            }
        }
        assert!(bad.is_empty(), "{} inline handlers: {bad:#?}", bad.len());
        assert!(!page.contains("javascript:"), "a javascript: URL");
        for (i, _) in page.match_indices("<script") {
            let tag = &page[i..page[i..].find('>').map_or(page.len(), |e| i + e)];
            assert!(tag.contains(" src="), "inline <script>: {tag}");
        }
    }

    #[test]
    fn every_data_action_names_an_entry_in_the_actions_allowlist() {
        // A typo'd action name does nothing (silently). Every name the markup
        // uses must be a key of ACTIONS, one key per line (`  name: ...`).
        let page = page_for_tests();
        let block = page
            .find("const ACTIONS = ")
            .map(|i| &page[i..])
            .expect("no ACTIONS allowlist");
        let mut used = std::collections::BTreeSet::new();
        for attr in ["data-click", "data-change", "data-input"] {
            let pat = format!("{attr}=\"");
            for (i, _) in page.match_indices(&pat) {
                let rest = &page[i + pat.len()..];
                used.insert(rest[..rest.find('"').unwrap_or(0)].to_string());
            }
        }
        assert!(
            used.len() > 25,
            "found only {} actions: {used:?}",
            used.len()
        );
        for name in &used {
            assert!(
                block.contains(&format!("\n  {name}:")),
                "data-* action {name:?} is not in ACTIONS"
            );
        }
    }

    #[test]
    fn merge_suggestions_are_listed_escaped_and_applied_through_the_allowlist() {
        let page = page_for_tests();
        let block = page
            .find("function renderSourcesPreview")
            .map(|i| &page[i..])
            .expect("renderSourcesPreview");
        let render = &block[..block
            .find("\nasync function previewSources")
            .expect("end of render")];
        assert!(
            render.contains("p.suggestions"),
            "the preview must render suggestions"
        );
        assert!(
            render.contains("applySuggestion"),
            "each suggestion offers an Apply"
        );
        // every value of a suggestion goes through esc()
        for field in ["s.a", "s.b", "pr.a_field", "pr.b_field", "s.confidence"] {
            if let Some(i) = render.find(field) {
                let before = &render[i.saturating_sub(12)..i];
                assert!(
                    before.contains("esc("),
                    "{field} is concatenated unescaped: ...{before}"
                );
            }
        }
        assert!(
            render.contains("'+esc(e)+'"),
            "evidence lines must be escaped"
        );
        assert!(
            page.contains("\n  applySuggestion:"),
            "applySuggestion is not in ACTIONS"
        );
        assert!(
            page.contains("properties: (s.properties || [])"),
            "applying sends the accepted property alignments"
        );
    }

    // ---- E1: accessible, labelled, routable navigation (docs/STUDIO-ENHANCEMENTS.md) ----

    fn css_color(name: &str) -> (f64, f64, f64) {
        let page = page_for_tests();
        let i = page
            .find(&format!("{name}: #"))
            .unwrap_or_else(|| panic!("no {name}"));
        let hex = &page[i + name.len() + 3..i + name.len() + 9];
        let c = |k: usize| f64::from(u8::from_str_radix(&hex[k..k + 2], 16).unwrap()) / 255.0;
        (c(0), c(2), c(4))
    }

    fn luminance((r, g, b): (f64, f64, f64)) -> f64 {
        let f = |c: f64| {
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
    }

    fn contrast(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn secondary_text_meets_wcag_aa_on_the_page_and_on_cards() {
        for bg in ["--bg", "--panel"] {
            for fg in ["--muted", "--text", "--accent"] {
                let c = contrast(css_color(fg), css_color(bg));
                assert!(c >= 4.5, "{fg} on {bg} is {c:.2}:1, below WCAG AA (4.5)");
            }
        }
    }

    #[test]
    fn navigation_is_a_labelled_landmark_whose_buttons_are_all_closed_and_named() {
        let page = page_for_tests();
        assert!(
            page.contains("<nav aria-label=\"Main\""),
            "the rail must be a named navigation landmark"
        );
        let start = page.find("<nav").unwrap();
        let nav = &page[start..start + page[start..].find("</nav>").unwrap()];
        let (open, close) = (
            nav.matches("<button").count(),
            nav.matches("</button>").count(),
        );
        assert_eq!(
            open, close,
            "an unclosed <button> in the navigation (the Data Sources entry was one)"
        );
        assert!(open >= 16, "{open} nav buttons");
        for b in nav.split("<button").skip(1) {
            assert!(b.contains("data-panel=\""), "{b}");
            assert!(
                b.contains("class=\"lbl\""),
                "every entry has a visible text label, not only an icon: {b}"
            );
        }
    }

    #[test]
    fn the_page_is_keyboard_and_motion_friendly() {
        let page = page_for_tests();
        assert!(page.contains(":focus-visible"), "a visible focus ring");
        assert!(
            page.contains("prefers-reduced-motion"),
            "reduced motion is honoured"
        );
        assert!(
            page.contains("data-click=\"skipToContent\""),
            "a skip-to-content control"
        );
        assert!(
            page.contains("#sidebar:has(:focus-visible)"),
            "labels appear on keyboard focus, not only on hover"
        );
    }

    #[test]
    fn panels_are_routable() {
        let page = page_for_tests();
        for needle in ["hashchange", "'#/' +", "aria-current", "document.title"] {
            assert!(page.contains(needle), "router piece missing: {needle}");
        }
        // an SSO hand-off puts #token=... in the hash: the router must leave it alone
        assert!(
            page.contains("^#\\/") || page.contains("startsWith('#/')"),
            "the router must only claim #/panel hashes"
        );
    }

    // ---- E3: one request helper; toasts (docs/STUDIO-ENHANCEMENTS.md) ----

    fn js_fn(name: &str) -> String {
        let page = page_for_tests();
        let i = page.find(name).unwrap_or_else(|| panic!("no {name}"));
        let rest = &page[i + name.len()..];
        let end = rest
            .find("\nasync function ")
            .min(rest.find("\nfunction "))
            .unwrap_or(rest.len());
        rest[..end].to_string()
    }

    #[test]
    fn every_http_verb_shares_one_request_helper_that_handles_session_expiry() {
        let page = page_for_tests();
        assert!(page.contains("async function request(method, url, body)"));
        let request = js_fn("async function request(method, url, body)");
        assert!(
            request.contains("res.status === 401") && request.contains("showLogin()"),
            "401 must return to sign-in for EVERY verb"
        );
        assert!(
            request.contains("X-Aikoql-Csrf") || request.contains("authHeaders("),
            "CSRF header on every request"
        );
        for (name, verb) in [
            ("async function api(", "GET"),
            ("async function apiPost(", "POST"),
            ("async function apiPut(", "PUT"),
        ] {
            let body = js_fn(name);
            assert!(
                body.contains(&format!("request('{verb}'")),
                "{name} must delegate to request: {body}"
            );
            assert!(
                !body.contains("fetch("),
                "{name} must not duplicate the transport"
            );
        }
    }

    #[test]
    fn notifications_are_a_polite_live_region_with_alerts_for_errors() {
        let page = page_for_tests();
        assert!(
            page.contains("id=\"toasts\"") && page.contains("aria-live=\"polite\""),
            "a live region"
        );
        let toast = js_fn("function toast(kind, text");
        assert!(
            toast.contains("'alert'") && toast.contains("'status'"),
            "errors are alerts, the rest status"
        );
        assert!(
            toast.contains("textContent"),
            "toast text is set as text, never markup"
        );
        assert!(
            page.contains("\n  dismissToast:"),
            "dismiss goes through the allowlist"
        );
        assert!(page.contains("aria-label"), "the dismiss control is named");
    }

    // ---- E2/E6: truthful persistence, saved queries and history ----

    #[test]
    fn nothing_claims_to_save_what_it_does_not_store() {
        let page = page_for_tests();
        assert!(
            !page.contains("auto-saved"),
            "the old Ctrl+S message claimed a save that stored nothing"
        );
        let store = js_fn("const store = ");
        assert!(
            store.contains("localStorage") && store.contains("catch"),
            "storage is guarded: it can throw or be absent"
        );
        assert!(
            page.contains("'aikoql.studio.' + (currentUser"),
            "stored per signed-in user, not shared across people on one browser"
        );
    }

    #[test]
    fn the_query_panel_has_a_saved_and_recent_library_reachable_through_the_allowlist() {
        let page = page_for_tests();
        for id in [
            "id=\"query-library\"",
            "id=\"query-save-name\"",
            "id=\"query-saved\"",
            "id=\"query-history\"",
        ] {
            assert!(page.contains(id), "missing {id}");
        }
        for action in ["saveQuery", "loadQuery", "deleteSaved", "clearHistory"] {
            assert!(
                page.contains(&format!("\n  {action}:")),
                "{action} not in ACTIONS"
            );
        }
        assert!(
            page.contains("aria-label=\"Name for the saved query\""),
            "the name input has a label, not only a placeholder"
        );
        // saving is a keyboard gesture on both platforms
        assert!(page.contains("(e.ctrlKey || e.metaKey) && e.key === 's'"));
    }

    // ---- E4: one results table that can be sorted, copied and exported ----

    #[test]
    fn query_results_use_one_shared_table_component() {
        let page = page_for_tests();
        for needle in [
            "function renderResults(",
            "function resultsToCsv(",
            "function cellText(",
        ] {
            assert!(page.contains(needle), "missing {needle}");
        }
        for action in ["sortResults", "copyResults", "downloadResults"] {
            assert!(
                page.contains(&format!("\n  {action}:")),
                "{action} not in ACTIONS"
            );
        }
        let run = js_fn("async function runQuery()");
        assert!(
            run.contains("renderResults("),
            "runQuery must use the shared component"
        );
        assert!(
            !run.contains("<table>"),
            "runQuery must not build its own table any more"
        );
        let table = js_fn("function tableHtml(");
        assert!(
            table.contains("aria-sort"),
            "sortable headers announce their state"
        );
        assert!(
            table.contains("data-click=\"openInInspector\""),
            "a row opens its object in the Inspector"
        );
    }

    // ---- E5: command palette ----

    #[test]
    fn the_command_palette_is_a_modal_combobox_with_a_keyboard_contract() {
        let page = page_for_tests();
        for needle in [
            "id=\"palette\"",
            "role=\"dialog\"",
            "aria-modal=\"true\"",
            "role=\"combobox\"",
            "role=\"listbox\"",
            "aria-activedescendant",
            "id=\"palette-backdrop\"",
        ] {
            assert!(page.contains(needle), "palette markup missing {needle}");
        }
        for action in ["closePalette", "paletteRun"] {
            assert!(
                page.contains(&format!("\n  {action}:")),
                "{action} not in ACTIONS"
            );
        }
        assert!(
            page.contains("(e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k'"),
            "Ctrl/Cmd+K opens it"
        );
        let keys = js_fn("function paletteKey(");
        for k in ["'Escape'", "'ArrowDown'", "'ArrowUp'", "'Enter'"] {
            assert!(keys.contains(k), "palette key {k} unhandled");
        }
        let close = js_fn("function closePalette(");
        assert!(
            close.contains("lastFocus"),
            "closing returns focus to where it was"
        );
        assert!(
            keys.contains("closePalette()"),
            "Escape closes through the one close path"
        );
        // the backdrop is a SIBLING that precedes the dialog: a click inside the
        // dialog must not close it
        let (backdrop, dialog) = (
            page.find("id=\"palette-backdrop\"").unwrap(),
            page.find("id=\"palette\"").unwrap(),
        );
        assert!(backdrop < dialog, "backdrop precedes the dialog");
        let between = &page[backdrop..dialog];
        assert!(
            between.contains("</div>"),
            "the backdrop element is closed before the dialog opens (not nested inside it)"
        );
    }

    // ---- P1: one working tenant ----

    #[test]
    fn one_header_selector_sets_the_tenant_of_every_panel() {
        let page = page_for_tests();
        assert!(
            page.contains("id=\"tenant-context\""),
            "the header tenant selector"
        );
        assert!(
            page.contains("aria-label=\"Working tenant"),
            "it has an accessible name"
        );
        assert!(
            page.contains("\n  setTenantContext:"),
            "reachable through the allowlist"
        );
        let apply = js_fn("function applyTenantContext(");
        for id in [
            "query-tenant",
            "ontology-tenant",
            "sources-tenant",
            "ask-tenant",
            "graph-tenant",
        ] {
            assert!(page.contains(&format!("id=\"{id}\"")), "{id} still exists");
            assert!(
                page.contains(&format!("'{id}'")),
                "{id} is driven by the context"
            );
        }
        assert!(
            apply.contains("tenant-context"),
            "the header selector mirrors the context"
        );
        assert!(
            page.contains("store.set('tenant'"),
            "the choice is remembered"
        );
        // one-way: a panel's own selector must never rewrite the working tenant (a view
        // filter silently re-pointing another panel's writes)
        assert!(
            !page.contains("TENANT_SELECTS.includes(e.target.id)"),
            "panel selectors must not feed back into the working tenant"
        );
    }

    // ---- P1: light theme ----

    /// The CSS between `<style>` and `</style>`.
    fn studio_css() -> &'static str {
        let page = page_for_tests();
        &page[page.find("<style>").unwrap()..page.find("</style>").unwrap()]
    }

    /// `--name: #rrggbb` tokens declared inside the first `{ ... }` after `selector`.
    fn tokens_of(selector: &str) -> std::collections::BTreeMap<String, (f64, f64, f64)> {
        let css = studio_css();
        let at = css
            .find(selector)
            .unwrap_or_else(|| panic!("no {selector}"));
        let open = at + css[at..].find('{').unwrap() + 1;
        let block = &css[open..open + css[open..].find('}').unwrap()];
        let mut out = std::collections::BTreeMap::new();
        for decl in block.split(';') {
            if let Some((k, v)) = decl.split_once(':') {
                let (k, v) = (k.trim(), v.trim());
                if k.starts_with("--") && v.len() == 7 && v.starts_with('#') {
                    let c =
                        |i: usize| f64::from(u8::from_str_radix(&v[i..i + 2], 16).unwrap()) / 255.0;
                    out.insert(k.to_string(), (c(1), c(3), c(5)));
                }
            }
        }
        out
    }

    #[test]
    fn the_light_theme_exists_and_applies_by_system_preference_and_by_choice() {
        let css = studio_css();
        assert!(
            css.contains(":root[data-theme=\"light\"]"),
            "an explicit light theme"
        );
        assert!(
            css.contains("@media (prefers-color-scheme: light)"),
            "light follows the system"
        );
        assert!(
            css.contains(":root:not([data-theme=\"dark\"])"),
            "an explicit dark choice beats a light system"
        );
        assert!(
            css.contains("color-scheme"),
            "native controls and scrollbars follow the theme"
        );
        let page = page_for_tests();
        assert!(
            page.contains("id=\"theme-toggle\""),
            "a visible theme control"
        );
        assert!(
            page.contains("\n  cycleTheme:"),
            "reachable through the allowlist"
        );
        for f in [
            "function applyTheme(",
            "function cycleTheme(",
            "function effectiveDark(",
        ] {
            assert!(page.contains(f), "{f}");
        }
        // the saved choice is applied before first paint, by the blocking head script
        assert!(
            EDITOR_JS.contains("aikoql.studio.theme"),
            "no dark flash for a light-theme user"
        );
        assert!(
            !EDITOR_JS.contains("aikoql-query-history"),
            "the editor no longer keeps an unscoped, cross-user query history"
        );
    }

    #[test]
    fn every_color_token_has_a_light_value_and_both_palettes_meet_wcag_aa() {
        let dark = tokens_of(":root {");
        let light = tokens_of(":root[data-theme=\"light\"]");
        assert!(dark.len() >= 10, "dark tokens parsed: {}", dark.len());
        for k in dark.keys() {
            assert!(light.contains_key(k), "the light theme does not define {k}");
        }
        for (name, palette) in [("dark", &dark), ("light", &light)] {
            for bg in ["--bg", "--panel"] {
                for fg in [
                    "--text", "--muted", "--accent", "--green", "--red", "--purple", "--orange",
                ] {
                    let c = contrast(palette[fg], palette[bg]);
                    assert!(
                        c >= 4.5,
                        "{name}: {fg} on {bg} is {c:.2}:1 (WCAG AA needs 4.5)"
                    );
                }
            }
            // text on the filled accent button
            let c = contrast(palette["--on-accent"], palette["--accent"]);
            assert!(c >= 4.5, "{name}: button label on accent is {c:.2}:1");
        }
    }

    #[test]
    fn no_color_is_hard_coded_outside_the_theme_blocks() {
        // a hard-coded colour is a colour the light theme cannot change
        let css = studio_css();
        let (start, end) = (
            css.find("/* theme:start */").unwrap(),
            css.find("/* theme:end */").unwrap(),
        );
        let outside = format!("{}{}", &css[..start], &css[end..]);
        let mut offenders = Vec::new();
        for line in outside.lines() {
            let t = line.trim();
            let hex = t.match_indices('#').any(|(i, _)| {
                let h: String = t[i + 1..]
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .collect();
                matches!(h.len(), 3 | 6 | 8) && !t[..i].ends_with('&')
            });
            let rgba = t.contains("rgba(")
                && !t.contains("rgba(var(--")
                && !t.contains("rgba(0, 0, 0")
                && !t.contains("rgba(0,0,0");
            if hex || rgba {
                offenders.push(t.chars().take(90).collect::<String>());
            }
        }
        assert!(
            offenders.is_empty(),
            "{} hard-coded colours:\n{}",
            offenders.len(),
            offenders.join("\n")
        );
    }

    #[test]
    fn the_system_preference_block_cannot_drift_from_the_explicit_light_theme() {
        let css = studio_css();
        let media = css.find("@media (prefers-color-scheme: light)").unwrap();
        let explicit = tokens_of(":root[data-theme=\"light\"]");
        let auto = tokens_of(&css[media..][css[media..].find(":root:not").unwrap()..][..60]);
        assert_eq!(
            explicit, auto,
            "the two light blocks must hold the same colours"
        );
    }

    // ---- P1: responsive layout ----

    #[test]
    fn on_a_phone_the_navigation_is_a_drawer_with_a_named_toggle() {
        let page = page_for_tests();
        for needle in [
            "id=\"nav-toggle\"",
            "aria-controls=\"sidebar\"",
            "aria-expanded=\"false\"",
            "id=\"nav-backdrop\"",
            "@media (max-width: 799px)",
            "#app.nav-open #sidebar",
        ] {
            assert!(page.contains(needle), "missing {needle}");
        }
        for action in ["toggleNav", "closeNav"] {
            assert!(
                page.contains(&format!("\n  {action}:")),
                "{action} not in ACTIONS"
            );
        }
        let css = studio_css();
        let small = &css[css.find("@media (max-width: 799px)").unwrap()..];
        assert!(
            small.contains("visibility: hidden"),
            "a closed drawer must leave the tab order"
        );
        assert!(
            small.contains("min-height: 40px"),
            "touch targets are at least 40px"
        );
        assert!(
            small.contains("font-size: 16px"),
            "inputs are 16px so a phone does not zoom on focus"
        );
        // a flex child defaults to min-width:auto and would let wide content push the page sideways
        assert!(
            css.contains("#main { flex: 1; min-width: 0;") || css.contains("min-width: 0"),
            "main may shrink"
        );
        let close = js_fn("function closeNav(");
        assert!(
            close.contains("nav-toggle") && close.contains("focus"),
            "closing returns focus to the toggle"
        );
        assert!(
            js_fn("function switchPanel(").contains("closeNav"),
            "choosing a panel closes the drawer"
        );
    }

    // ---- polish found by looking at screenshots ----

    #[test]
    fn the_navigation_scrolls_so_every_entry_is_reachable_on_a_short_window() {
        let css = studio_css();
        let at = css.find("#sidebar nav {").expect("the nav rule");
        let rule = &css[at..at + css[at..].find('}').unwrap()];
        assert!(
            rule.contains("overflow-y: auto") && rule.contains("min-height: 0"),
            "{rule}"
        );
    }

    #[test]
    fn a_destructive_palette_command_needs_more_than_a_letter() {
        let page = page_for_tests();
        assert!(page.contains("risky: true"), "Sign out is marked risky");
        let render = js_fn("function renderPalette(");
        assert!(
            render.contains("risky"),
            "the palette filters risky commands by query length"
        );
    }

    #[test]
    fn plain_form_controls_follow_the_theme_and_long_hints_do_not_collide() {
        let css = studio_css();
        assert!(
            css.contains("\nselect {") || css.contains("\nselect, "),
            "bare <select>s are styled by the theme"
        );
        assert!(
            css.contains(".query-editor-label { ") && css.contains("flex-wrap: wrap"),
            "the label row wraps"
        );
        let small = &css[css.find("@media (max-width: 799px)").unwrap()..];
        assert!(
            small.contains(".query-editor-label span + span { display: none; }"),
            "the keyboard hint is hidden on touch screens"
        );
        assert!(
            small.contains(".header-spacer { flex-basis: 100%"),
            "the header breaks into a tidy second row"
        );
    }

    #[test]
    fn save_ontology_does_not_assert_an_identity() {
        // The server derives the subject from the bearer token (with_principal);
        // the client must not claim one.
        assert!(
            !page_for_tests().contains("subject: \"admin\""),
            "saveOntology sends a body-declared subject"
        );
    }

    #[test]
    fn licence_status_line_is_html_escaped() {
        let mut lic = license(&["multi-user"], None);
        lic.org = "<img src=x onerror=alert(1)>".into();
        let html = studio_html(false, Some(&lic));
        assert!(
            !html.contains("<img src=x onerror=alert(1)>"),
            "the licence org reaches the page as markup"
        );
        assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
    }

    #[test]
    fn login_card_shows_license_status_marker() {
        // Task 9: the served login card carries the status line — licensed
        // servers render org + features + human-readable expiry (the same
        // data GET /api/license serves), community renders "Community".
        let lic = license(
            &["multi-user"],
            Some(UNIX_EPOCH + Duration::from_secs(4_102_444_800)), // 2100-01-01T00:00Z
        );
        let html = studio_html(false, Some(&lic));
        assert!(html.contains("data-license-status"), "marker missing");
        assert!(
            html.contains("acme-corp · multi-user · expires 2099-12-31"),
            "licensed line must carry org, features and the expiry day"
        );
        let community = studio_html(false, None);
        assert!(
            community.contains("data-license-status"),
            "marker must stay in community mode"
        );
        assert!(
            community.contains(">Community<"),
            "community must say Community"
        );
    }

    #[test]
    fn login_card_has_sso_button_marker() {
        // Task 9: the SSO button (T5, injected only when OIDC is licensed
        // AND configured) carries data-sso-button; without it the button is
        // absent from the served page.
        let lic = license(&["oidc-sso"], None);
        let configured = studio_html(true, Some(&lic));
        assert!(configured.contains("data-sso-button"), "marker missing");
        let licensed_but_unconfigured = studio_html(false, Some(&lic));
        assert!(!licensed_but_unconfigured.contains("data-sso-button"));
        let unlicensed = studio_html(false, None);
        assert!(!unlicensed.contains("data-sso-button"));
    }

    #[test]
    fn users_panel_marker_present() {
        // Task 9: the served admin panel carries the users panel anchor —
        // the JS reveals it only for sessions GET /api/users admits.
        assert!(studio_html(false, None).contains("data-users-panel"));
        let lic = license(&["multi-user"], None);
        assert!(studio_html(false, Some(&lic)).contains("data-users-panel"));
    }

    #[test]
    fn users_panel_escapes_server_data() {
        // Task 9 fix: rows render /api/users data (IdP-influenceable in
        // SSO mode) through an all-five-entities helper, and the sub
        // reaches the handler via a data attribute — never raw inside an
        // onclick string, where a name/sub claim would break out and steal
        // the admin's authToken.
        let html = page_for_tests();
        assert!(html.contains("function esc("), "esc helper missing");
        assert!(
            html.contains(
                "const sub = esc(u.sub), name = esc(u.name), roles = esc((u.roles||[]).join(', '));"
            ),
            "the row builder must escape every user field"
        );
        assert!(html.contains("data-sub="), "sub must ride a data attribute");
        assert!(
            !html.contains("onclick=\"setUserRoles('"),
            "raw sub inside an onclick string is the XSS sink"
        );
    }

    #[test]
    fn audit_export_button_present_when_licensed() {
        // Task 9: the export button exists only under the audit-export
        // license flag — unlicensed pages must not carry the marker.
        let lic = license(&["audit-export"], None);
        assert!(
            studio_html(false, Some(&lic)).contains("data-audit-export"),
            "export marker missing under the flag"
        );
        assert!(
            !studio_html(false, None).contains("data-audit-export"),
            "community must not carry the export button"
        );
        let other = license(&["multi-user"], None);
        assert!(
            !studio_html(false, Some(&other)).contains("data-audit-export"),
            "a license without the flag must not carry the export button"
        );
    }

    /// A merge once left a panel unclosed, nesting the Admin panel inside a hidden
    /// one: every control in it silently stopped working. Only a browser run saw it;
    /// this catches the class cheaply.
    #[test]
    fn studio_page_div_tags_balance_and_every_panel_is_a_top_level_sibling() {
        let page = page_for_tests();
        let (open, close) = (page.matches("<div").count(), page.matches("</div>").count());
        assert_eq!(
            open, close,
            "unbalanced <div> tags in the Studio page: {open} open, {close} closed"
        );
        // each `class="panel"` div must open at the same nesting depth
        let mut depth = 0i32;
        let mut depths = std::collections::BTreeSet::new();
        let mut rest: &str = page;
        while let Some(i) = rest.find('<') {
            rest = &rest[i..];
            if rest.starts_with("</div>") {
                depth -= 1;
            } else if rest.starts_with("<div") {
                if rest.starts_with("<div class=\"panel\"") {
                    depths.insert(depth);
                }
                depth += 1;
            }
            rest = &rest[1..];
        }
        assert_eq!(
            depths.len(),
            1,
            "panels open at different nesting depths: {depths:?}"
        );
    }
}
