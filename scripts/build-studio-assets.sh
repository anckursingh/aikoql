#!/usr/bin/env bash
# Rebuild the vendored Studio scripts from assets/studio/package.json (exact,
# locked versions). The OUTPUT (editor.js, vis-network.js) is committed and
# embedded in the binary — nothing is fetched from a CDN at runtime, and
# nothing here runs in CI or at cargo build. Run this only to bump a library:
#   scripts/build-studio-assets.sh && git diff --stat crates/services/api/mcp/assets/studio
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
dir="$root/crates/services/api/mcp/assets/studio"
cd "$dir"
npm ci --no-audit --no-fund --ignore-scripts
npx esbuild editor.src.js --bundle --minify --format=iife --target=es2020 --legal-comments=none --outfile=editor.js
cp node_modules/vis-network/standalone/umd/vis-network.min.js vis-network.js
shasum -a 256 editor.js vis-network.js | tee CHECKSUMS.sha256
