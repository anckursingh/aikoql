// Studio query editor: CodeMirror 6 with an AIKOQL language definition and
// schema/ontology-aware completion. Bundled to editor.js (see
// scripts/build-studio-assets.sh); served same-origin, never from a CDN.
// Applied before first paint (this script blocks in <head>): a saved explicit theme
// choice must not flash the other palette. Storage may be blocked, so it is guarded.
try {
  const saved = localStorage.getItem("aikoql.studio.theme");
  if (saved === "light" || saved === "dark") document.documentElement.dataset.theme = saved;
} catch (_) { /* no storage: the system preference decides */ }

import { EditorView, basicSetup } from "codemirror";
import { Compartment } from "@codemirror/state";
import { StreamLanguage } from "@codemirror/language";
import { oneDark } from "@codemirror/theme-one-dark";
import { autocompletion } from "@codemirror/autocomplete";

// Light mode keeps CodeMirror's default highlighting and takes its surface colours
// from the same CSS tokens as the rest of Studio; dark mode is One Dark.
const lightTheme = EditorView.theme({
  "&": { color: "var(--text)", backgroundColor: "var(--code-bg)" },
  ".cm-content": { caretColor: "var(--text)" },
  ".cm-gutters": { backgroundColor: "var(--code-gutter)", color: "var(--muted)", borderRight: "1px solid var(--border)" },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "rgba(var(--accent-rgb), 0.08)" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": { backgroundColor: "rgba(var(--accent-rgb), 0.25)" },
}, { dark: false });
const themeSlot = new Compartment();
function prefersDark() {
  const t = document.documentElement.dataset.theme;
  if (t) return t === "dark";
  return !window.matchMedia("(prefers-color-scheme: light)").matches;
}

const KEYWORDS = [
  ["CREATE", "Create a Knowledge Object", 3], ["MATCH", "Query Knowledge Objects", 3],
  ["RETURN", "Return clause (required)", 4], ["TRAVERSE", "Walk relationships", 2],
  ["UPDATE", "Update a KO", 2], ["DELETE", "Delete/deactivate a KO", 1],
  ["INGEST", "Ingest external data", 1], ["EXTRACT", "Extract from a source", 1],
  ["EXPLAIN", "Show the plan", 1], ["COMMIT", "Commit staged changes", 1],
  ["WHERE", "Filter condition", 2], ["AND", "Logical AND", 1], ["OR", "Logical OR", 1],
  ["NOT", "Logical NOT", 1], ["ORDER", "Sort results", 1], ["BY", "", 1], ["GROUP", "Group rows", 1],
  ["ASC", "Ascending", 1], ["DESC", "Descending", 1], ["LIMIT", "Max results", 1],
  ["OFFSET", "Skip results", 1], ["DEPTH", "Traversal depth", 1], ["FUSION", "Search fusion mode", 1],
  ["SIMILAR", "Vector similarity", 1], ["TO", "", 1], ["SCORE", "Score output", 1], ["BM25", "Text scoring", 1],
  ["USING", "", 1], ["EMBEDDING", "", 1], ["JOIN", "", 1], ["LEFT", "", 1], ["ON", "", 1],
  ["COUNT", "Aggregate", 1], ["SUM", "Aggregate", 1], ["AVG", "Aggregate", 1], ["MIN", "Aggregate", 1], ["MAX", "Aggregate", 1],
  ["ASOF", "Time travel", 1], ["JOURNAL", "", 1], ["BETWEEN", "", 1], ["HISTORICAL", "All versions", 1],
  ["CHANGESET", "", 1], ["EPISTEMIC", "Epistemic status", 1], ["SOURCE", "", 1],
  ["INBOUND", "Inbound direction", 1], ["OUTBOUND", "Outbound direction", 1], ["BOTH", "Both directions", 1],
  ["DEPENDS_ON", "Dependency relationship", 1], ["CONTAINS", "Container relationship", 1],
  ["USES", "Usage relationship", 1], ["GOVERNED_BY", "Governance relationship", 1],
  ["ACTIVE", "Active lifecycle", 0], ["DRAFT", "Draft lifecycle", 0], ["ARCHIVED", "Archived lifecycle", 0],
  ["TRUE", "Boolean", 0], ["FALSE", "Boolean", 0], ["NULL", "Null", 0],
];
const KW = new Set(KEYWORDS.map(k => k[0]));
const BUILTIN_TYPES = ["program", "workflow", "policy", "agent", "trigger", "connector", "benchmark", "view", "report", "ontology", "memory", "query"]
  .map(t => "aikoql:" + t);

// AIKOQL is not SQL: its own tokenizer (keywords, strings, numbers, `//`
// comments, operators, `aikoql:type` names) instead of the PostgreSQL dialect.
const aikoql = StreamLanguage.define({
  token(stream) {
    if (stream.eatSpace()) return null;
    if (stream.match("//")) { stream.skipToEnd(); return "comment"; }
    const ch = stream.peek();
    if (ch === '"' || ch === "'") {
      stream.next();
      let esc = false, c;
      while ((c = stream.next()) != null) {
        if (!esc && c === ch) break;
        esc = !esc && c === "\\";
      }
      return "string";
    }
    if (stream.match(/^-?\d+(\.\d+)?/)) return "number";
    if (stream.match(/^(==|!=|<=|>=|[<>=*+\-\/%])/)) return "operator";
    if (stream.match(/^[A-Za-z_][\w:]*/)) {
      const w = stream.current();
      if (KW.has(w.toUpperCase())) return "keyword";
      if (w.includes(":")) return "typeName";
      return "variableName";
    }
    stream.next();
    return null;
  },
});

// Completion: keywords, then the live schema — type names, the property names
// seen on those types, and the classes of the active ontology (all filled by
// Studio after login).
function completions(context) {
  const word = context.matchBefore(/[\w:]*/);
  if (!word || (word.from === word.to && !context.explicit)) return null;
  const text = word.text.toLowerCase();
  const seen = new Set();
  const options = [];
  const add = (label, type, detail, boost) => {
    if (label.toLowerCase().startsWith(text) && !seen.has(label)) {
      seen.add(label);
      options.push({ label, type, detail, boost });
    }
  };
  for (const [k, d, b] of KEYWORDS) add(k, "keyword", d, b);
  for (const t of BUILTIN_TYPES) add(t, "type", "Built-in KO type", 0);
  for (const t of window.__schemaTypes || []) add(t, "type", "Knowledge Object type", 1);
  for (const c of window.__ontologyClasses || []) add(c, "class", "Ontology class", 1);
  for (const p of window.__schemaProps || []) add(p, "property", "Property", 0);
  return options.length ? { from: word.from, options, filter: false } : null;
}

window.__initCodeMirror = function () {
  const el = document.getElementById("query-editor");
  if (!el || window.__cmView) return;
  window.__cmView = new EditorView({
    doc: "",
    extensions: [
      basicSetup,
      aikoql,
      themeSlot.of(prefersDark() ? oneDark : lightTheme),
      autocompletion({ override: [completions] }),
    ],
    parent: el,
  });
  // Ctrl+Enter → run query (capture phase to beat CodeMirror's own handlers).
  window.__cmView.dom.addEventListener("keydown", e => {
    if (e.ctrlKey && e.key === "Enter") { e.preventDefault(); e.stopPropagation(); window.runQuery(); }
  }, true);
};
window.__cmSetTheme = function (dark) {
  if (!window.__cmView) return;
  window.__cmView.dispatch({ effects: themeSlot.reconfigure(dark ? oneDark : lightTheme) });
};
window.__cmSetValue = function (v) {
  if (!window.__cmView) return;
  window.__cmView.dispatch({ changes: { from: 0, to: window.__cmView.state.doc.length, insert: v } });
};
window.__cmGetValue = function () {
  return window.__cmView ? window.__cmView.state.doc.toString() : "";
};
