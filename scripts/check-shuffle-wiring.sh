#!/usr/bin/env bash
# PR6-F5 (P1-5): shuffle-run wiring gate. The nightly randomized-order run
# catches cross-test interference out-of-band of the PR loop — but a
# workflow step can rot silently (a bad merge re-adding a step nobody
# greps). This gate fails the moment any piece is missing: the run script,
# the residue sweepers, or the nextest install + run steps in the nightly
# workflow. The RED for this milestone was exactly this gate failing
# against the unwired tree (docs/red-archive/shuffle-wiring-vs-unwired).

set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
wf="$root/.github/workflows/benchmark-nightly.yml"
fail=0

[ -x "$root/scripts/run-shuffle.sh" ] || { echo "SHUFFLE: missing scripts/run-shuffle.sh" >&2; fail=1; }
[ -x "$root/scripts/check-residue.sh" ] || { echo "SHUFFLE: missing scripts/check-residue.sh" >&2; fail=1; }
grep -q 'tool: nextest' "$wf" || { echo "SHUFFLE: benchmark-nightly.yml lacks the nextest install step" >&2; fail=1; }
grep -q 'run-shuffle.sh' "$wf" || { echo "SHUFFLE: benchmark-nightly.yml lacks the shuffle run step" >&2; fail=1; }
[ "$(grep -c 'check-residue.sh' "$wf")" -ge 2 ] || { echo "SHUFFLE: the residue sweepers must arm before AND after the run" >&2; fail=1; }

if [ $fail -ne 0 ]; then exit 1; fi
echo "shuffle wiring — OK"
