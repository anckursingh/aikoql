# Studio browser e2e

Drives the real Studio page in Chrome against a server it starts itself (temp
database, free port, bootstrap admin). Run by `scripts/prepush.sh` when a
browser is available; **not** part of CI.

    cd tests/e2e/studio && npm ci      # playwright-core only — uses your system Chrome
    cargo build -p aikoql-mcp
    node tests/e2e/studio/run.js       # 0 pass · 1 fail · 77 skipped (reason printed)

Env: `AIKOQL_BIN` (default `target/debug/aikoql-mcp`), `CHROME_BIN`.

Why it exists: the page has no inline handlers, and the CSP is
`script-src 'self'`. A handler that was missed does not raise an error — the
control just does nothing — so unit tests cannot see it. This test clicks every
delegated control (it must react), runs the named flows (login, query, explorer,
inspector, graph, ontology preview/save/CAS, backups), and fails on any CSP
violation, `?token=` request or third-party request.
