#!/usr/bin/env bash
# Launch S-01: architecture hygiene gate — the storage leg (review 2 §8/§22,
# docs/IMPLEMENTATION-PLAN-LAUNCH.md). Six assertions, all RED against the
# pre-S-02 tree, green when the phase ends:
#   1. aikoql-storage-v2 is a workspace member and no other storage backend
#      crate is (the deprecated members are crates/storage/aikoql + rocksdb).
#   2. No production Cargo.toml depends on a deprecated backend
#      (aikoql-storage / aikoql-rocksdb).
#   3. No production code references the deprecated v1 API (aikoql_storage).
#   4. No deprecated backend in production: no redb references, no
#      AIKOQL_BACKEND/BackendEnvGuard backend-selection machinery, and the
#      kernel carries no store_redb.rs.
#   5. The benchmark harness (benchmarks/, scripts/competitor_bench/) uses
#      the current storage API: no v1 backend-selection pins.
#   6. The harness language itself is v2-only: no redb name, no backend=
#      kwarg-style selection in the rust or python harness (4a/4b cover
#      `crates benchmarks`; this adds scripts/competitor_bench and the kwarg).
#
# Workflow leg (CI-01, review 2 §22 TDD): five tests prescribing the
# POST-consolidation workflow estate, asserted by name (never via file
# count). RED against the live tree at CI-01 — benchmark.yml does not
# exist yet (CI-02 merges baseline-guard + benchmark-nightly into the one
# benchmark owner) and the perf smoke carries 3 of the review's 5 cells
# (CI-03 grows it to W1–W5). The tests flip green through CI-02/CI-03.
# The dag wiring rides CI-04 (a RED gate must not enter CI).
set -euo pipefail
root="${TESTS_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
cd "$root"
fail=0

# 1. workspace members: v2 present, no deprecated storage crate
if ! grep -q '"crates/storage/aikoql-v2"' Cargo.toml; then
  echo "ARCH: aikoql-storage-v2 is not a workspace member" >&2
  fail=1
fi
for bad in aikoql rocksdb; do
  if grep -q "\"crates/storage/$bad\"" Cargo.toml; then
    echo "ARCH: deprecated storage crate is a workspace member: crates/storage/$bad" >&2
    fail=1
  fi
done

# 2. no production dependency on a deprecated backend (the deprecated
# crates' own manifests are excluded — they are deleted wholesale in S-02)
deps="$(grep -rnE '^(aikoql-storage|aikoql-rocksdb)[[:space:]]*=' crates benchmarks \
  --include='Cargo.toml' 2>/dev/null \
  | grep -vE '^crates/storage/(aikoql|rocksdb)/Cargo.toml:' || true)"
if [ -n "$deps" ]; then
  echo "$deps" | sed 's/^/ARCH: production dep on a deprecated backend: /' >&2
  fail=1
fi

# 3. no v1 API references in production code (identifier boundary, so
# aikoql_storage_v2 never matches)
api="$(grep -rnE 'aikoql_storage([^_a-z0-9]|$)' crates benchmarks \
  --include='*.rs' --include='*.py' 2>/dev/null \
  | grep -v '/tests/' || true)"
if [ -n "$api" ]; then
  echo "$api" | sed 's/^/ARCH: v1 API reference in production code: /' >&2
  fail=1
fi

# 4a. no redb references in production code
redb="$(grep -rn '\bredb\b' crates benchmarks \
  --include='*.rs' --include='*.toml' --include='*.py' 2>/dev/null \
  | grep -v '/tests/' || true)"
if [ -n "$redb" ]; then
  echo "$redb" | sed 's/^/ARCH: redb reference in production code: /' >&2
  fail=1
fi

# 4b. no backend-selection machinery in production code
sel="$(grep -rn 'AIKOQL_BACKEND\|BackendEnvGuard' crates benchmarks \
  --include='*.rs' --include='*.py' 2>/dev/null \
  | grep -v '/tests/' || true)"
if [ -n "$sel" ]; then
  echo "$sel" | sed 's/^/ARCH: v1 backend-selection machinery in production: /' >&2
  fail=1
fi

# 4c. the kernel carries no redb backend module
if [ -f crates/kernel/src/storage/store_redb.rs ]; then
  echo "ARCH: kernel carries the deprecated redb backend (store_redb.rs)" >&2
  fail=1
fi

# 5. the benchmark harness pins no v1 backend selection
pins="$(grep -rnE 'AIKOQL_BACKEND|STORAGE_BACKEND' benchmarks scripts/competitor_bench \
  --include='*.rs' --include='*.py' --include='*.sh' 2>/dev/null || true)"
if [ -n "$pins" ]; then
  echo "$pins" | sed 's/^/ARCH: harness pins the v1 backend selection: /' >&2
  fail=1
fi

# 6. the harness language is v2-only (S-05): no redb name, no backend=
# kwarg-style selection in the rust or python harness
harness="$(grep -rnE '\bredb\b|AIKOQL_BACKEND|backend[[:space:]]*=' benchmarks scripts/competitor_bench \
  --include='*.rs' --include='*.py' 2>/dev/null || true)"
if [ -n "$harness" ]; then
  echo "$harness" | sed 's/^/ARCH: v1 backend language in the harness: /' >&2
  fail=1
fi

# ── Workflow leg (CI-01, review 2 §22) ──────────────────────────────
CIWF=.github/workflows/ci.yml

# workflow test 1 — test_required_ci_jobs_exist: the required CI jobs
# (fmt/clippy/check/test + the gates) are named jobs in ci.yml
for job in check test-linux lint dependency-dag; do
  if ! grep -qE "^  $job:" "$CIWF"; then
    echo "ARCH: ci.yml is missing the required job: $job" >&2
    fail=1
  fi
done
for step in 'cargo fmt --check' 'cargo clippy --workspace' 'cargo check --workspace' 'cargo test --workspace'; do
  if ! grep -qF "$step" "$CIWF"; then
    echo "ARCH: ci.yml is missing the required step: $step" >&2
    fail=1
  fi
done

# workflow test 2 — test_benchmark_workflow_exists: benchmark.yml is the
# one benchmark owner — the 1M self-regression and the competitor matrix
# live there, and the pre-consolidation homes are merged away (CI-02)
BENCH=.github/workflows/benchmark.yml
if [ ! -f "$BENCH" ]; then
  echo "ARCH: $BENCH missing — CI-02 merges baseline-guard + benchmark-nightly into the one benchmark owner" >&2
  fail=1
else
  if ! grep -q 'STORAGE_REGRESSION=1m' "$BENCH"; then
    echo "ARCH: $BENCH must run the 1M self-regression (STORAGE_REGRESSION=1m)" >&2
    fail=1
  fi
  if ! grep -q 'competitor_bench/scale.py' "$BENCH"; then
    echo "ARCH: $BENCH must run the competitor scale harness" >&2
    fail=1
  fi
  for other in ci perf-smoke coverage-floor release; do
    # run-signature patterns: the dag job's own pins quote these strings
    # (a pin reference is not a run — post-CI-02 the guard pins in ci.yml
    # name the 1M regime as a grep pattern)
    if grep -qE 'export STORAGE_REGRESSION=1m|competitor_bench/scale.py' ".github/workflows/$other.yml"; then
      echo "ARCH: $other.yml carries a 1M/competitor leg — benchmark.yml owns it alone" >&2
      fail=1
    fi
  done
fi
for gone in baseline-guard benchmark-nightly; do
  if [ -f ".github/workflows/$gone.yml" ]; then
    echo "ARCH: .github/workflows/$gone.yml still exists — CI-02 merges it into benchmark.yml" >&2
    fail=1
  fi
done

# workflow test 3 — test_competitor_matrix_exists: the engine column set
# is declared in bench.py and a workflow job runs the scale harness
if ! grep -qE 'postgresql|neo4j|qdrant' scripts/competitor_bench/bench.py; then
  echo "ARCH: competitor matrix declares no external engines (bench.py)" >&2
  fail=1
fi
if ! grep -q 'scripts/competitor_bench/scale.py' .github/workflows/*.yml; then
  echo "ARCH: no workflow job runs the competitor scale harness" >&2
  fail=1
fi

# workflow test 4 — test_perf_smoke_remains_wired: the perf smoke carries
# the review's five cells (point lookup, write throughput, scan, hot-cache,
# small compaction) under the 3x budget — CI-03 grows the 3 committed
# cells to W1–W5
SMOKE=.github/workflows/perf-smoke.yml
if [ ! -f "$SMOKE" ]; then
  echo "ARCH: $SMOKE missing — the per-commit perf smoke job must exist" >&2
  fail=1
elif ! grep -q 'perf-smoke.sh' "$SMOKE"; then
  echo "ARCH: $SMOKE must run scripts/perf-smoke.sh" >&2
  fail=1
elif ! grep -q '3x' "$SMOKE"; then
  echo "ARCH: $SMOKE must declare the 3x budget" >&2
  fail=1
fi
for cell in kse_m7_v2_workloads hot_head_gate throughput scan compact; do
  if ! grep -q "$cell" scripts/perf-smoke.sh; then
    echo "ARCH: perf smoke is missing the review cell: $cell" >&2
    fail=1
  fi
done

# workflow test 5 — test_release_workflow_remains_wired: the release
# workflow keeps the version gate (tag == Cargo/npm/plugin/python) and
# the identity verification (published versions + the MCP binary smoke)
REL=.github/workflows/release.yml
if [ ! -f "$REL" ]; then
  echo "ARCH: $REL missing — the release workflow must exist" >&2
  fail=1
fi
if ! grep -q 'Validate versions vs tag' "$REL"; then
  echo "ARCH: $REL must keep the version gate (validate-versions)" >&2
  fail=1
fi
if ! grep -qE '^  verify-release-identity:' "$REL"; then
  echo "ARCH: $REL must keep the identity verification job (PR6-009)" >&2
  fail=1
fi
if ! grep -q 'smoke-mcp.js' "$REL"; then
  echo "ARCH: $REL must keep the MCP binary smoke (version + initialize + tools)" >&2
  fail=1
fi

if [ $fail -ne 0 ]; then exit 1; fi
echo "architecture hygiene (storage + workflow legs) — OK"
