#!/usr/bin/env bash
# PR6-F5 (P1-5): nightly randomized-order run. The shuffle is libtest's
# --shuffle on the nightly toolchain: nextest has no shuffle flag and
# stable libtest gates --shuffle behind -Z unstable-options (both
# verified 2026-09-29 — nextest 0.9.146, rustc 1.97.1), so nightly
# libtest is the only harness that shuffles; the job installs a PINNED
# nightly (benchmark.yml) so rust-cache's toolchain key stays stable.
# libtest prints the shuffle seed, and the run log lives with the job —
# a night's failure reproduces locally with
#   cargo +nightly test ... -- -Z unstable-options --shuffle-seed <seed>
# The same skip list as ci.yml via the gated registry (the single source
# of truth); the residue sweepers bracket the run and fail on any
# listener / temp dir / tree mutation it leaves behind — the cross-test
# interference classes caught out-of-band of the PR loop.
set -euo pipefail
cd "$(dirname "$0")/.."

filters="$(bash scripts/skip-list.sh)"
args=()
[ -z "$filters" ] || args=($filters)
cargo +nightly test --workspace --all-features --no-fail-fast -- -Z unstable-options --shuffle "${args[@]}"
