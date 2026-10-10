#!/usr/bin/env bash
# L-22 (TDD-033): shuffle behavioral proof. The P1-5 wiring gate
# (check-shuffle-wiring.sh) proves the nightly randomized-order run is
# WIRED; this proves the nightly MECHANISM actually catches an order
# dependency — the gap the L-22 row calls "wiring grep only".
#
# The nightly shuffle (scripts/run-shuffle.sh) runs on nightly libtest:
# nextest has no --shuffle flag and stable libtest gates the flag behind
# -Z unstable-options (both verified 2026-09-29 — nextest 0.9.146, rustc
# 1.97.1), so the pinned-nightly harness is the only one that shuffles.
#
# A deliberate dependency between two env-armed probes
# (crates/storage/aikoql-v2/tests/shuffle_probe.rs — no-ops unless
# AIKOQL_SHUFFLE_PROBE_DIR is set): the writer leaks a marker file, the
# reader fails when it runs first. The proof runs the pair through the
# real nightly shuffle, serialized (--test-threads=1, so the shuffled
# order IS the execution order), at two pinned seeds:
#   SEED_CONTROL=1 — writer first -> the pair PASSES (not merely broken)
#   SEED_CAUGHT=2  — reader first -> the run FAILS with the probe panic
# Each probe records its name in the shared order log, so the proof
# asserts the order the shuffle ran, not an assumption. Re-pin the seeds
# if the probe set changes shape or libtest's shuffle algorithm moves
# (scan seeds 1..40 for both orders, serialized).
set -euo pipefail
cd "$(dirname "$0")/.."

SEED_CONTROL=1
SEED_CAUGHT=2

# 0. the nightly mechanism itself: run-shuffle.sh must run the nightly
# libtest shuffle. A wiring drift back to a non-shuffling harness makes
# every seed below vacuous. Live command lines only (^ anchors past the
# header prose) — the mutation harness m4 damages exactly this line.
if ! grep -qE '^cargo \+nightly test .* -Z unstable-options --shuffle' scripts/run-shuffle.sh; then
  echo "SHUFFLE PROOF: scripts/run-shuffle.sh does not run the nightly libtest shuffle — the nightly randomized-order run cannot shuffle (nextest has no --shuffle; stable libtest gates it behind -Z unstable-options)" >&2
  exit 1
fi

probe_dir="$(mktemp -d)"
trap 'rm -rf "$probe_dir"' EXIT
export AIKOQL_SHUFFLE_PROBE_DIR="$probe_dir"

run_pair() { # $1 seed — runs the pair, leaves the order log behind
  rm -f "$probe_dir/order" "$probe_dir/marker"
  cargo +nightly test -p aikoql-storage-v2 --test shuffle_probe \
    -- -Z unstable-options --test-threads=1 --shuffle-seed "$1" 2>&1
}

# control: the dependency satisfied — both pass, writer recorded first
if ! control_out="$(run_pair "$SEED_CONTROL")"; then
  echo "SHUFFLE PROOF: control seed $SEED_CONTROL FAILED — the pair is broken, not order-dependent:" >&2
  echo "$control_out" | tail -20 >&2
  exit 1
fi
[ "$(head -n1 "$probe_dir/order")" = "writer" ] || {
  echo "SHUFFLE PROOF: control seed $SEED_CONTROL did not run the writer first (order: $(tr '\n' ',' < "$probe_dir/order")) — re-pin the seeds" >&2
  exit 1
}
echo "shuffle proof: control seed=$SEED_CONTROL order=$(tr '\n' ',' < "$probe_dir/order") -> PASS"

# caught: reader first — the run FAILS with the probe's panic
if caught_out="$(run_pair "$SEED_CAUGHT")"; then
  echo "SHUFFLE PROOF: caught seed $SEED_CAUGHT PASSED — the shuffle did not catch the dependency (re-pin the seeds)" >&2
  exit 1
fi
grep -q 'TDD-033: order dependency caught' <<<"$caught_out" || {
  echo "SHUFFLE PROOF: the caught run failed for the wrong reason:" >&2
  echo "$caught_out" | tail -20 >&2
  exit 1
}
[ "$(head -n1 "$probe_dir/order")" = "reader" ] || {
  echo "SHUFFLE PROOF: the caught run's order log does not show the reader first" >&2
  exit 1
}
echo "shuffle proof: caught seed=$SEED_CAUGHT order=$(tr '\n' ',' < "$probe_dir/order") -> FAIL (dependency caught)"
echo "shuffle behavioral proof — OK"
