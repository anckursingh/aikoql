#!/usr/bin/env bash
# D-18 §27 — the every-release SDK certification battery: version parity,
# conformance, fuzz smoke, real-server integration, install smokes, the
# example, and the SBOM. ONE script so the sdk-release-cert job in
# release.yml and a local run certify identically.
#   Requires: the release server built at target/release/aikoql-mcp, the
#   toolchains on PATH (python3, go, JDK 17 + mvn, node >= 24, rust), and
#   syft on PATH (the job installs it). The fuzz smokes set the runtime
#   floor — this is the long pole of the release, not a unit gate.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

BIN="$(realpath "${AIKOQL_MCP_BIN:-$root/target/release/aikoql-mcp}")"
[ -x "$BIN" ] || { echo "sdk-release-cert: no release server at $BIN — build it first" >&2; exit 1; }
export AIKOQL_MCP_BIN="$BIN"

say()  { echo "::group::$1"; }
done_() { echo "::endgroup::"; }

# 1. Version parity — every SDK's MIN_SERVER_VERSION equals the workspace
#    version (compatibility.json is the frozen contract).
say "1/8 version parity"
bash scripts/check-sdk-compat.sh
done_

# 2. Conformance — the frozen §7 vectors through all five SDKs against real
#    servers (each arm spawns its own), plus the native transport on rust
#    (D-15: the framed binary protocol).
say "2/8 conformance (5 languages + native)"
for lang in python go rust typescript java; do
  bash scripts/sdk-conformance.sh --language "$lang" --bin "$BIN"
done
bash scripts/sdk-conformance.sh --language rust --transport native --bin "$BIN"
done_

# 3. Python — wheel build + venv install + the full suite (the fuzz estate
#    rides the same pytest run); installing the wheel IS the install smoke.
say "3/8 python wheel + suite"
( cd crates/sdk/python
  rm -rf dist                      # stale wheels ride the install glob
  python3 -m venv /tmp/aikoql-cert-venv
  /tmp/aikoql-cert-venv/bin/pip install -q maturin pytest hypothesis
  /tmp/aikoql-cert-venv/bin/maturin build --release --out dist
  /tmp/aikoql-cert-venv/bin/pip install -q "dist/aikoql-"*.whl
  /tmp/aikoql-cert-venv/bin/python -c "import aikoql; print('aikoql', aikoql.__version__)"
  /tmp/aikoql-cert-venv/bin/python -m pytest -q tests/ )
done_

# 4. Go — vet + the real-server suite + the 8 fuzz smokes + the example.
say "4/8 go suite + fuzz + example"
( cd crates/sdk/go
  go vet ./...
  go test -v ./...
  # go's -fuzz refuses a multi-target regex — one smoke per target.
  for name in $(grep -o '^func Fuzz[A-Za-z0-9]*' fuzz_test.go | awk '{print $2}'); do
    go test -fuzz "^${name}\$" -fuzztime 5s .
  done
  go build ./examples/... )
done_

# 5. Java — install (unit tests + the local-repo jar = the install smoke)
#    and the jazzer smokes; real-server integration is the conformance arm.
say "5/8 java install + jazzer smokes"
( cd crates/sdk/java
  mvn -B -q -Dgpg.skip=true install
  bash jazzer-smoke.sh )
done_

# 6. TypeScript — the real-server suite (fuzz estate + pool + wire ride the
#    same node --test run), then the npm tarball install smoke: pack, install
#    the exact tarball, run the release binary through run.js (the R7
#    AIKOQL_BINARY hatch keeps it off the download path).
say "6/8 typescript suite + npm install smoke"
( cd crates/sdk/typescript
  npm ci      # fast-check is a devDependency — the fresh release runner has none
  # the glob, not the dir — node resolves `tests/` as a module on Windows
  node --test tests/*.test.ts )
( cd npm-publish
  mkdir -p /tmp/aikoql-pkg          # npm pack does not create --pack-destination
  npm pack --pack-destination /tmp/aikoql-pkg
  tarball=$(ls /tmp/aikoql-pkg/aikoql-mcp-*.tgz | head -1)
  clean=$(mktemp -d)
  npm install --prefix "$clean" "$tarball" >/dev/null
  cd "$clean"
  AIKOQL_BINARY="$BIN" npx aikoql-mcp --version
  AIKOQL_BINARY="$BIN" node "$OLDPWD/smoke-mcp.js" npx aikoql-mcp serve "$clean/db" )
done_

# 7. Rust — the real-server suite + the 8 cargo-fuzz smokes (cargo-fuzz was
#    deferred at D-16; the release cert is where it finally runs).
say "7/8 rust suite + fuzz smokes"
cargo test -p aikoql-sdk
cargo install cargo-fuzz --locked -q
( cd crates/sdk/rust/fuzz
  for t in fuzz_targets/*.rs; do
    cargo fuzz run "$(basename "$t" .rs)" -- -max_total_time=5
  done )
done_

# 8. Docs — every package ships a README (pkg.go.dev, PyPI, npm, crates.io
#    and Maven all render it): a release must not publish a bare package.
say "8/9 docs"
for d in crates/sdk/go crates/sdk/python crates/sdk/rust crates/sdk/typescript crates/sdk/java npm-publish; do
  [ -s "$d/README.md" ] || { echo "sdk-release-cert: $d has no README.md" >&2; exit 1; }
done
done_

# 9. SBOM — syft catalogs every manifest in the tree (Cargo, npm, maven,
#    python) into one SPDX document stamped with the release.
say "9/9 SBOM"
syft dir:"$root" --exclude "./target" -o "spdx-json=aikoql-release.sbom.spdx.json"
done_

echo "sdk-release-cert: every leg green for $(basename "$BIN")"
