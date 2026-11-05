# Vendored Studio scripts

`editor.js` and `vis-network.js` are committed build outputs, embedded in the
binary and served same-origin at `/studio/assets/*` (no CDN at runtime).
Rebuild with `scripts/build-studio-assets.sh` (exact versions in
`package.json` / `package-lock.json`); `CHECKSUMS.sha256` records the outputs.

| bundled in | package | version | licence |
|---|---|---|---|
| editor.js | codemirror | 6.0.1 | MIT |
| editor.js | @codemirror/language | 6.10.8 | MIT |
| editor.js | @codemirror/theme-one-dark | 6.1.2 | MIT |
| editor.js | @codemirror/autocomplete | 6.18.4 | MIT |
| editor.js | @codemirror/state, @codemirror/view | 6.7.6, 6.43.15 | MIT |
| editor.js | @lezer/common, @lezer/highlight, @lezer/lr | 1.5.5, 1.2.5, 1.4.12 | MIT |
| editor.js | style-mod, w3c-keyname, crelt, @marijn/find-cluster-break | 4.1.4, 2.2.8, 1.0.7, 1.0.4 | MIT |
| vis-network.js | vis-network (standalone UMD build, unmodified) | 9.1.2 | Apache-2.0 OR MIT |

`editor.src.js` is this repository's own code (the AIKOQL language definition
and completion); it carries the repository licence.
