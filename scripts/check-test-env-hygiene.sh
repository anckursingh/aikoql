#!/usr/bin/env bash
# PR6-F4 (P1-4): seed-determinism gate. The two known flake classes both
# trace to uncontrolled test-process state: a test mutating the
# process-global environment (set_var/remove_var) leaks into parallel
# siblings in the same binary and into every spawned child (the
# AIKOQL_BACKEND redb leak, 2026-09-18), and an unseeded RNG makes a test
# non-reproducible. New tests must use the per-child env helpers (spawn
# with a clean env) and seeded RNGs — this gate fails any test-code
# set_var/remove_var or unseeded-RNG use.
#
# Existing sites are pinned in the allowlist below: (file, raw first arg)
# pairs with an honest one-line reason each. A NEW var in an allowlisted
# file, or any use in a new file, fails — the allowlist is the review
# point, updated deliberately, never silently. The pins migrate to
# per-child env over time; each carries its reason here.

set -euo pipefail
root="${TESTS_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
cd "$root"

allowed() { # file|token -> 0 if pinned (patterns fully quoted: the | is literal)
  case "$1|$2" in
    'crates/kernel/tests/index_subsystem.rs|"INDEX_REBUILD_PARK"') return 0 ;; # crash park armed in-process for the sibling binary under test
    'crates/kernel/tests/index_subsystem.rs|"INDEX_REBUILD_PARK_AT"') return 0 ;; # the park's armed-at timestamp, polled by the same sibling
    'crates/runtime/tests/cbo.rs|"INDEX_DROP_PARK"') return 0 ;; # crash park armed in-process; dropped thread consumes it before cleanup
    'crates/runtime/tests/cbo.rs|"INDEX_DROP_PARK_AT"') return 0 ;; # the park's armed-at timestamp, polled by the same sibling
    'crates/services/api/mcp/tests/mcp_real_world.rs|"AIKOQL_BACKEND"') return 0 ;; # BackendEnvGuard + immediate clean — the 2026-09-18 leak's fixed site; child spawns under it
    'crates/storage/aikoql/tests/report_gating.rs|"AIKOQL_REPORT_WRITE"') return 0 ;; # gates the report writer itself; single-test binary
    'crates/storage/aikoql-v2/tests/checkpoint_streaming.rs|"AIKOQL_V2_PLACE_PARK"') return 0 ;; # crash park armed in-process for the library under test
    'crates/storage/aikoql-v2/tests/report_gating.rs|"AIKOQL_REPORT_WRITE"') return 0 ;; # gates the report writer itself; single-test binary
    'crates/storage/aikoql-v2/tests/compaction_lock_scope.rs|PARK_ENV') return 0 ;; # const AIKOQL_V2_COMPACT_PARK; park armed/cleared around the compactor stage under test
    'crates/storage/aikoql-v2/tests/flush_lock_scope.rs|PARK_ENV') return 0 ;; # const AIKOQL_V2_FLUSH_IO_PARK; park armed/cleared around the M38 flush phase under test
    'crates/storage/aikoql-v2/tests/snapshot_cells.rs|"AIKOQL_V2_SNAP_CELLS"') return 0 ;; # M34 cell-recorder path — armed once for the binary's single test
    'crates/storage/aikoql-v2/tests/snapshot_redesign.rs|PARK_ENV') return 0 ;; # const AIKOQL_V2_SNAP_PARK; park armed/cleared around each stage
    'crates/storage/aikoql-v2/tests/snapshot_matrix.rs|PARK_ENV') return 0 ;; # const AIKOQL_V2_SNAP_PARK; park armed/cleared around each stage
    *) return 1 ;;
  esac
}

fail=0

# env-mutation leg
hits="$(grep -rnE '(set_var|remove_var)\(' crates tests --include='*.rs' 2>/dev/null | grep '/tests/' || true)"
if [ -n "$hits" ]; then
  while IFS= read -r hit; do
    file="${hit%%:*}"
    rest="${hit#*:}"; rest="${rest#*:}"
    raw="$(printf '%s' "$rest" | sed -nE 's/.*(set_var|remove_var)\(([^,)]*).*/\2/p' | head -1)"
    token="$(printf '%s' "$raw" | tr -d '[:space:]')"
    if ! allowed "$file" "$token"; then
      echo "ENV HYGIENE: unpinned env mutation: $file ($(printf '%s' "$token" | head -c 40))" >&2
      fail=1
    fi
  done <<< "$hits"
fi

# seed-determinism leg — no allowlist: the tree is clean today, and any
# unseeded RNG in new test code fails.
rng="$(grep -rnE 'thread_rng\(\)|rand::random\(|from_entropy\(' crates tests --include='*.rs' 2>/dev/null | grep '/tests/' || true)"
if [ -n "$rng" ]; then
  echo "$rng" | while IFS= read -r hit; do
    echo "SEED DETERMINISM: unseeded RNG: ${hit%%:*} ($(printf '%s' "${hit#*:}" | head -c 40))" >&2
  done
  fail=1
fi

if [ $fail -ne 0 ]; then exit 1; fi
echo "test env hygiene + seed determinism — OK"
