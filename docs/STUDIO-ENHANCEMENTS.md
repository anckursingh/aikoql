# Studio — architectural review and UX enhancement plan

Status: written 2026-11-09 from the code (`crates/services/api/mcp/src/studio.rs`,
`assets/studio/app.js`, `tests/e2e/studio/run.js`), not from earlier documents.
Every claim below carries the measurement or code location that supports it.

## 1. What Studio is today

A single-page console: one HTML shell (`studio.rs`, 1.4k lines incl. CSS), one script
(`app.js`, 1.8k lines, 84 functions), two vendored libraries (CodeMirror, vis-network),
16 panels, all REST. It has no framework and no build step by design, and its security
posture is strong and test-enforced: HttpOnly cookie session, CSRF header, CSP
`script-src 'self'`, no inline handlers (a closed `ACTIONS` allowlist, 49 entries), and an
exhaustive lint that every value concatenated into markup goes through `esc()`.
A real-Chrome e2e (67 checks) clicks every delegated control.

That foundation is worth keeping. The weaknesses are in *experience* and *structure*.

## 2. Findings (evidence in brackets)

### A. Navigation and orientation
- **Nav is hover-only.** Labels are `opacity:0` until `#sidebar:hover`; a keyboard or touch
  user sees 16 unlabeled emoji buttons [`studio.rs` `#sidebar nav button .lbl`; only
  `title=` as fallback].
- **No routing.** `switchPanel()` mutates a variable; refresh returns to *Query Editor*,
  the browser Back button leaves Studio, and a panel cannot be linked or bookmarked
  [`app.js` `switchPanel`; no `hashchange`/`pushState`].
- **16 flat destinations** with no grouping (data work vs. governance vs. operations) and no
  search or command palette to jump.
- **Tenant is chosen five times**, once per panel, each populated by hand in `loadTenants()`
  [`app.js` 163-176]. There is no single "working in tenant X" context.

### B. Trust and honesty of feedback
- **Ctrl+S says "Query auto-saved" but nothing is stored** (`localStorage` use: 0). A reload
  loses the query [`app.js` 285-293]. This is a false assurance.
- **The chunked view is client-side paging.** The button is honestly labelled "Chunked" with an
  explanatory tooltip, but the function is still called `streamQuery` and the whole result is fetched
  first; real incremental delivery needs a server change (P1).
- **No notification system.** Outcomes appear in ad-hoc places (`status-center`, per-panel
  `*-msg` divs, inline `error-text`); nothing announces success/failure consistently.
- **Session expiry is handled only for GET.** `api()` redirects to sign-in on 401;
  `apiPost`/`apiPut` do not, so a write after expiry shows a raw error in a panel.
  The three verb helpers are copy-pasted (`app.js` 70-96).

### C. Working with results (the core job of a query console)
- Results are a bare table: values cut at 80 chars, **no sorting, no copy, no export, no way
  to open a row in the Inspector**, no elapsed time, no query history, no saved queries
  [`runQuery`]. `streamQuery` duplicates the table renderer with a different cut (60).
- Errors are shown but not actionable beyond one special case (AIKOQL1030).

### D. Accessibility (WCAG 2.2 AA)
- `aria-`/`role=` in the whole UI: **1** occurrence; `tabindex`: 0; `:focus-visible`: 0.
- Muted text `#5c5c78` has contrast **3.06:1** on the page and **2.86:1** on cards (AA needs
  4.5:1 for small text); **53** font-size declarations are ≤ 11px.
- No `prefers-reduced-motion`, no skip link, no live regions for async results, tables
  without captions/scope, form inputs rely on placeholders.

### E. Responsiveness and theming
- **0 media queries.** A fixed 52px rail + fixed-height graph (`calc(100vh - 130px)`) make
  the app unusable under ~700px; there is no light theme and no `prefers-color-scheme`.

### F. Code structure
- 165 `innerHTML` sites each hand-escaping with `esc()`. It is lint-protected, so it is safe,
  but it is the main reason adding UI is slow and error-prone (every table is hand-built;
  three table renderers exist in the query panel alone).
- Global mutable state at the top of one 1.8k-line file; panel loaders hard-wired in
  `switchPanel`; tenant lists re-implemented per panel.

## 3. Principles for the enhancements

1. **Truthful by default** — never claim a save/stream/success that did not happen.
2. **Keyboard-first, mouse-friendly** — every action reachable and visible without hover.
3. **Never lose work** — drafts survive reload; destructive actions are reversible or confirmed.
4. **One way to do each thing** — one request helper, one table component, one toast, one
   tenant context.
5. **Keep the security model** — no inline handlers, CSP unchanged, all dynamic text through
   `esc()`; every new control is registered in `ACTIONS` and covered by the Chrome e2e.
6. **Small, test-first steps** — each enhancement ships with a Rust page test and an e2e check.

## 4. Enhancements (prioritised by user impact ÷ effort)

### P0 — this iteration
| id | enhancement | acceptance |
|---|---|---|
| E1 | **Accessible, labelled, routable navigation.** Labels always visible at ≥ 1100px (rail collapses only on narrow screens), `aria-label`/`aria-current`, roving focus, `role="navigation"`; `#/panel` routing (refresh, Back/Forward and deep links work); skip link; `:focus-visible` ring; contrast of muted text ≥ 4.5:1; `prefers-reduced-motion` | e2e: reload on `#/ontology` lands on Ontology; Back returns to the previous panel; Tab reaches every nav item and shows its label; unit: contrast token ≥ 4.5 |
| E2 | **Truthful persistence.** Query draft, last panel, tenant and result view persist in `localStorage` (guarded; works without it); Ctrl/Cmd+S saves a *named* query to a saved list; the false "auto-saved" message is removed | e2e: type, reload → same text; save, reload → listed; storage disabled → still works |
| E3 | **One request helper + toasts.** A single `request(method, url, body)` handles CSRF, JSON, 401 → sign-in for *every* verb, and error normalisation; `toast(kind, text)` live-region notifications replace ad-hoc `*-msg` divs for outcomes | e2e: expired session during a POST returns to sign-in; success/failure toasts announced (`role=status`/`alert`) |
| E4 | **A results table worth using.** One shared renderer: sortable columns, sticky header, full value on expand, copy cell/row, **Copy as JSON/CSV**, **Download CSV/JSON**, row count + elapsed ms, **row → open in Inspector**; replaces the two duplicate renderers | e2e: sort order flips; CSV matches rows; clicking a row opens its KOID in the Inspector |
| E5 | **Command palette (Ctrl/Cmd+K).** Fuzzy jump to any panel, run/explain the query, switch tenant, open saved/history queries, sign out | e2e: Ctrl+K → type "onto" → Enter lands on Ontology |
| E6 | **Query history.** Last 50 executed queries with outcome and time, one click to re-load | e2e: run two queries, both listed, click restores |

### P1 — next
- **Global tenant context** in the header, replacing five per-panel selects.
- **Responsive layout** (rail → drawer < 800px, stacked panels, graph height from container).
- **Light theme + `prefers-color-scheme`**; theme token audit (all colours as tokens).
- **First-run guidance:** actionable empty states ("No sources configured — add one" with
  the exact `aikoql.toml` snippet; "Ingest a document" CTAs), a short product tour.
- **Honest streaming:** either real chunked transfer (server) or rename to "Load in pages".
- **Inspector ⇄ Graph ⇄ Explorer linking:** every KOID is a link everywhere.

### P2 — structural
- Split `app.js` into ES-module assets (state store, `request`, components, one file per
  panel) served as separate embedded assets; keep the CSP.
- A tiny escaped-template helper (`html\`…\``) so components cannot forget `esc()`,
  retiring most of the 165 hand-built `innerHTML` sites over time.
- Virtualised tables for > 5k rows; web-worker CSV export.
- Visual-regression screenshots in the Chrome e2e.

### Status (2026-11-09)
P0 is **implemented and verified in real Chrome** (67 → 121 checks, all passing):
E1 routable accessible navigation · E2/E6 persisted draft, saved queries, history (Ctrl/Cmd+S saves for
real) · E3 one request helper + toasts · E4 sortable/copyable/exportable results with row→Inspector ·
E5 command palette. Measured: muted-text contrast 2.9:1 → 5.5:1; nothing below 11px; every
control reachable by keyboard. P1 and P2 are next, in the order above.

**P1 so far (same day):** global working tenant (header; one-way, a panel override stays local) ·
light theme (Auto/Light/Dark, system-following, no flash, editor and graph follow, both palettes tested
for AA) · responsive layout (drawer under 800px, 40px touch targets, 16px inputs, no sideways scroll on
any of the 16 panels at 390px). Real-Chrome checks: 151.

**Ontology workbench (docs/ONTOLOGY-IN-PRACTICE.md, Phase D, first slice):** keys, links and authority are
authored in Data Sources → Preview and reviewed in the Ontology panel (class cards, mapping table, Reconcile
sources, raw JSON demoted to *Advanced*). Real-Chrome checks: 169.

**Parked (decided with the product owner):**
- *Empty-source guidance.* A source that discovers 0 containers prints only the counts; it should say
  "this database has no tables — check the database name or credentials" with the likely causes.
  (Hit for real: a demo database seeded wrongly looked like a product bug.)

**Still open from P1:** first-run guidance and actionable empty states · honest paging for the chunked
query view (rename + server-side paging) · KOID links between Inspector, Graph and Explorer.

## 5. Non-goals
No framework or bundler; no third-party CDN; no change to the auth/CSRF/CSP model; no new
backend endpoints for P0.

## 6. Verification
Every item adds (a) a Rust test over `page_for_tests()` for markup/allowlist/escaping
invariants and (b) checks in `tests/e2e/studio/run.js` (real Chrome, run by
`scripts/prepush.sh`). The existing 67 checks must stay green.
