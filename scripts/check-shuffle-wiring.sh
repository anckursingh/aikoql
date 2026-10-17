#!/usr/bin/env bash
# PR6-F5 (P1-5): shuffle-run wiring gate. The nightly randomized-order
# run catches cross-test interference out-of-band of the PR loop — but
# a workflow step or script flag can rot silently (a bad merge dropping
# a piece nobody greps). This gate fails the moment any piece is
# missing: the run script and its nightly libtest shuffle flag, the
# proof step and its script, the pinned-nightly install, or the residue
# sweepers (CI-02: the shuffle job lives in benchmark.yml, the one
# benchmark owner). The RED for this milestone was exactly this gate
# failing against the unwired tree
# (docs/red-archive/shuffle-wiring-vs-unwired).

set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
wf="$root/.github/workflows/benchmark.yml"
fail=0

# -f, not -x: the repo convention tracks every script 644 (Windows
# authors — the exec bit is invisible in Git Bash locally and would
# fail on every Linux run); the workflow invokes them via `bash`.
[ -f "$root/scripts/run-shuffle.sh" ] || { echo "SHUFFLE: missing scripts/run-shuffle.sh" >&2; fail=1; }
[ -f "$root/scripts/shuffle-proof.sh" ] || { echo "SHUFFLE: missing scripts/shuffle-proof.sh" >&2; fail=1; }
[ -f "$root/scripts/check-residue.sh" ] || { echo "SHUFFLE: missing scripts/check-residue.sh" >&2; fail=1; }
# The nightly libtest shuffle on the LIVE command line (^ anchors past
# the header prose): nextest has no --shuffle and stable libtest gates
# it behind -Z unstable-options, so this exact shape is the only one
# that shuffles — the mutation harness m4 damages exactly this line.
grep -qE '^cargo \+nightly test .* -Z unstable-options --shuffle' "$root/scripts/run-shuffle.sh" \
  || { echo "SHUFFLE: run-shuffle.sh lost the nightly libtest shuffle" >&2; fail=1; }
# The pinned-nightly install (YAML mapping lines only — a comment
# quoting the toolchain would keep a raw grep green): without it the
# run errors on the stable toolchain, where the flags are unstable.
grep -qE '^[[:space:]]+toolchain: nightly-' "$wf" \
  || { echo "SHUFFLE: benchmark.yml lacks the pinned-nightly install step" >&2; fail=1; }
grep -q 'run-shuffle.sh' "$wf" || { echo "SHUFFLE: benchmark.yml lacks the shuffle run step" >&2; fail=1; }
grep -q 'shuffle-proof.sh' "$wf" || { echo "SHUFFLE: benchmark.yml lacks the shuffle proof step" >&2; fail=1; }
[ "$(grep -c 'check-residue.sh' "$wf")" -ge 2 ] || { echo "SHUFFLE: the residue sweepers must arm before AND after the run" >&2; fail=1; }

if [ $fail -ne 0 ]; then exit 1; fi
echo "shuffle wiring — OK"
