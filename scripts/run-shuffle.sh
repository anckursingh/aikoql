#!/usr/bin/env bash
# PR6-F5 (P1-5): nightly randomized-order run. nextest shuffles every test
# (process-per-test default — the interference classes left after P1-4's
# thread-scoping are process-level: leaked listeners, temp dirs, tree
# mutation). The same skip list as ci.yml via the gated registry (the
# single source of truth); nextest prints the shuffle seed, and the run log
# lives with the job — a night's failure reproduces locally with that seed.
set -euo pipefail
cd "$(dirname "$0")/.."

filters="$(bash scripts/skip-list.sh --nextest)"
args=()
[ -z "$filters" ] || args=(-E "$filters")
cargo nextest run --workspace --all-features --no-fail-fast --shuffle "${args[@]}"
