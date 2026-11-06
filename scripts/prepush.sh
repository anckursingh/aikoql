#!/usr/bin/env bash
# Pre-push gate: run every CI check that can run on a dev machine, so a push
# costs one CI round, not five. Mirrors ci.yml's lint, helm and
# dependency-dag jobs plus the Linux test step, and — when a Chrome is
# available — the Studio browser e2e. Runs ALL gates and
# summarises (the disposition-head gate is expected red until the dogfood
# re-stamp is the tip). Windows-only, Docker/MinIO and coverage jobs are
# left to CI.
#   scripts/prepush.sh          everything
#   scripts/prepush.sh --fast   skip cargo test
set -u
cd "$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"

fast=0
[ "${1:-}" = "--fast" ] && fast=1
failed=()
skipped=()

gate() { # gate <name> <command...>
  local name=$1
  shift
  echo "=== $name"
  if "$@"; then echo "--- ok: $name"; else failed+=("$name"); echo "--- FAILED: $name"; fi
}
skip() { skipped+=("$1 ($2)"); echo "=== skip: $1 ($2)"; }

gate "cargo fmt" cargo fmt --check
gate "cargo clippy" cargo clippy --workspace --all-targets -- -D warnings

if command -v helm >/dev/null; then
  gate "helm lint" helm lint deploy/helm/aikoql
  # same placeholder credential value ci.yml uses
  gate "helm template" bash -c 'helm template aikoql deploy/helm/aikoql --set auth.adminPasswordKey=ci-render-test >/dev/null'
else
  skip "helm" "not installed"
fi

if command -v cargo-audit >/dev/null; then
  gate "advisories" bash scripts/check-advisories.sh
else
  skip "advisories" "cargo-audit not installed"
fi

for s in check-disposition-head check-red-archives check-red-archive-tooling \
  check-skip-drift check-strong-claims check-test-env-hygiene \
  check-shuffle-wiring check-sdk-compat; do
  gate "$s" bash "scripts/$s.sh"
done
gate "version stamps" python3 scripts/bump-version.py --check

if [ "$fast" = 1 ]; then
  skip "cargo test" "--fast"
else
  # shellcheck disable=SC2046 # the skip list is meant to word-split
  gate "cargo test" cargo test --workspace -- $(bash scripts/skip-list.sh)
fi

# Optional: the Studio browser e2e (a real Chrome clicks every control; with CSP
# script-src 'self' a missed handler silently does nothing, which no unit test
# sees). Skipped, with the reason, when there is no Chrome / playwright-core.
# Never part of CI. Exit 77 from the harness = skipped.
if [ -f tests/e2e/studio/run.js ]; then
  echo "=== studio e2e"
  cargo build -q -p aikoql-mcp
  node tests/e2e/studio/run.js
  rc=$?
  if [ "$rc" = 0 ]; then echo "--- ok: studio e2e"
  elif [ "$rc" = 77 ]; then skipped+=("studio e2e (no browser / binary / playwright-core — see its message)")
  else failed+=("studio e2e"); echo "--- FAILED: studio e2e"; fi
fi

skip "Windows, Docker, connector, k8s, coverage, perf jobs" "CI only"
echo
echo "skipped:"; printf '  %s\n' "${skipped[@]}"
if [ ${#failed[@]} -gt 0 ]; then
  echo "FAILED:"; printf '  %s\n' "${failed[@]}"
  exit 1
fi
echo "all local gates green — now run the dogfood re-stamp as the tip, then push"
