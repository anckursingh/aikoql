#!/usr/bin/env bash
# L-20 (TDD-031) / ARCHITECT-REVIEW P2-10: mutation harness for the gate
# scripts themselves. The gates that guard the CI estate (skip-drift,
# shuffle-wiring, architecture-hygiene) are grep checkers — a bad merge can
# rot a wiring the way a rotten step rots a workflow. Each mutation below
# applies ONE surgical damage to a detached-worktree copy of the tree and
# runs the gate that must catch it. The gate's exit code propagates:
#
#   non-zero = the mutation IS caught (that exit is the RED the archive
#              captures via scripts/red-archive.sh)
#   zero     = UNCAUGHT — the gate stayed green on a damaged tree
#
#   mutation-harness.sh <id>   apply one mutation, run its gate
#   mutation-harness.sh all    run all seven; exit 0 iff all are caught
#
# Every mutation targets the estate as it stands NOW (the post-CI-01..16
# consolidation tree) — the worktree is HEAD, so the teeth stay honest as
# the estate evolves. The captured archives live under docs/red-archive/
# with the mut- prefix.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

mutations() {
  echo "m1-remove-gated-test m2-rename-gated-test m3-remove-skip-wiring \
m4-remove-nextest-install m5-remove-pre-residue-sweep \
m6-remove-post-residue-sweep m7-remove-protected-path"
}

mutate() {
  id="$1"
  tmp="$(mktemp -d)"
  tree="$tmp/tree"
  trap 'git worktree remove --force "$tree" 2>/dev/null || true; rm -rf "$tmp"' EXIT
  git worktree add --detach "$tree" HEAD >/dev/null 2>&1
  cd "$tree"
  case "$id" in
    m1-remove-gated-test)
      # a gated test's fn vanishes from the tree — the registry entry goes
      # dead and the CI skip silently un-skips the name it was guarding
      f="$(grep -rlF 'fn load_encryption_overhead_v2' crates --include='*.rs' --exclude-dir=target)"
      sed -i '/fn load_encryption_overhead_v2()/d' "$f"
      bash scripts/check-skip-drift.sh
      ;;
    m2-rename-gated-test)
      # same class, the other accident shape: the fn is renamed in a merge
      f="$(grep -rlF 'fn load_encryption_overhead_v2' crates --include='*.rs' --exclude-dir=target)"
      sed -i 's/fn load_encryption_overhead_v2()/fn load_encryption_overhead_v2_renamed()/' "$f"
      bash scripts/check-skip-drift.sh
      ;;
    m3-remove-skip-wiring)
      # the test jobs drop the registry derivation — every skip rots at
      # once (the registry comment survives; the gate must match live
      # invocation lines, not prose)
      sed -i 's/cargo test --workspace -- \$(bash scripts\/skip-list.sh)/cargo test --workspace/' .github/workflows/ci.yml
      bash scripts/check-skip-drift.sh
      ;;
    m4-remove-nextest-install)
      # the shuffle job loses the nextest install — the nightly
      # randomized-order run silently degrades to the default runner
      sed -i '/tool: nextest/d' .github/workflows/benchmark.yml
      bash scripts/check-shuffle-wiring.sh
      ;;
    m5-remove-pre-residue-sweep)
      # the before snapshot is dropped — residue "after" diffs against
      # nothing and the sweep can never fail
      sed -i '/check-residue.sh before/d' .github/workflows/benchmark.yml
      bash scripts/check-shuffle-wiring.sh
      ;;
    m6-remove-post-residue-sweep)
      # the after assert is dropped — residue is never checked
      sed -i '/check-residue.sh after/d' .github/workflows/benchmark.yml
      bash scripts/check-shuffle-wiring.sh
      ;;
    m7-remove-protected-path)
      # ONE protected path drops from the pull_request trigger block only
      # (the per-PR 1M guard class) — the push block keeps a whole-file
      # grep green, so the gate must pin each path in BOTH trigger blocks
      awk 'BEGIN { in_pr=0; done=0 }
           /^  pull_request:/ { in_pr=1 }
           in_pr && !done && index($0, "crates/kernel") { done=1; next }
           { print }' .github/workflows/benchmark.yml > .github/workflows/benchmark.yml.tmp \
        && mv .github/workflows/benchmark.yml.tmp .github/workflows/benchmark.yml
      bash scripts/check-architecture-hygiene.sh
      ;;
    *)
      echo "unknown mutation: $id" >&2
      exit 2
      ;;
  esac
}

case "${1:-}" in
  all)
    fail=0
    for m in $(mutations); do
      if mutate "$m"; then
        echo "UNCAUGHT: $m" >&2
        fail=1
      else
        echo "caught: $m"
      fi
    done
    if [ $fail -ne 0 ]; then
      echo "mutation harness: gates stayed green on a damaged tree" >&2
      exit 1
    fi
    echo "mutation harness: all seven caught"
    ;;
  "")
    echo "usage: mutation-harness.sh <id>|all" >&2
    exit 2
    ;;
  *)
    mutate "$1"
    ;;
esac
