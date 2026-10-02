#!/usr/bin/env bash
# Launch S-01: architecture hygiene gate — the storage leg (review 2 §8/§22,
# docs/IMPLEMENTATION-PLAN-LAUNCH.md). Seven assertions, all RED against the
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
#   7. The adoption-era env language is gone (S-04, folded here at CI-04):
#      no V2ADOPT-era names on the functional surfaces (crates/scripts/
#      .github/tests/gated.toml/AGENTS.md; historical docs keep the old
#      names by design).
#
# Workflow leg (CI-01, review 2 §22 TDD): seven tests prescribing the
# POST-consolidation workflow estate, asserted by name (never via file
# count). RED against the live tree at CI-01 — benchmark.yml does not
# exist yet (CI-02 merges baseline-guard + benchmark-nightly into the one
# benchmark owner) and the perf smoke carries 3 of the review's 5 cells
# (CI-03 grows it to W1–W5). The tests flip green through CI-02/CI-03;
# CI-05 adds test 6 (build jobs cached), CI-06 adds test 7 (required
# checks never path-filter), CI-07 adds test 8 (the hybrid knowledge
# workload wired), CI-08 adds test 9 (reproducible results + reports),
# CI-09 adds test 10 (the Tier-3 release certification), CI-10 adds test
# 11 (guard RSS = weekly evidence; per-PR guards stay slim), CI-11 adds
# test 12 (the aikoql compose service publishes no host ports), CI-12 adds
# test 13 (the coverage floor skips the alloc-budget pin under
# instrumentation), CI-13 adds tests 14-15 (the docker job parses the
# compose files; the go-sdk binary path is pinned at the right depth),
# CI-14 adds test 16 (the committed 1M v2 baseline validates against the
# §13/M47 schema). F-04 adds test 17 (the fuzz-estate pins, PR #7 fuzz
# review F-TDD-07: the nightly proptest arm, the storage-mutation job +
# all eleven §12 mutants, the F-02 lifecycle state machine).
# Wired into the dag job at CI-04 (a RED gate must not enter CI).
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

# 7. the adoption-era env language is gone (S-04): V2ADOPT-era names must
# be ABSENT from the functional surfaces (crates/scripts/.github/
# tests/gated.toml/AGENTS.md; historical docs keep the old names by
# design). The bracket in the pattern keeps the gate from self-matching.
langbad="$(git grep -nE 'V2ADOPT[_]' -- crates scripts .github tests/gated.toml AGENTS.md || true)"
if [ -n "$langbad" ]; then
  echo "$langbad" | sed 's/^/ARCH: V2ADOPT-era language present (S-04 rename to STORAGE_*): /' >&2
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
  for other in ci release; do
    # run-signature patterns: the dag job's own pins quote these strings
    # (a pin reference is not a run — post-CI-02 the guard pins in ci.yml
    # name the 1M regime as a grep pattern)
    if grep -qE 'export STORAGE_REGRESSION=1m' ".github/workflows/$other.yml"; then
      echo "ARCH: $other.yml carries the gate-5 1M regime — benchmark.yml owns it alone" >&2
      fail=1
    fi
  done
  # CI-09 amends the one-owner rule: release.yml may carry the benchmark
  # legs as Tier-3 tag-gated certification — but ONLY inside the tier3
  # jobs. ci.yml never runs a competitor leg, and the release legs must
  # not drift out of their tier3 job.
  if grep -q 'competitor_bench/scale.py' ".github/workflows/ci.yml"; then
    echo "ARCH: ci.yml carries a competitor leg — benchmark.yml owns it (CI-02)" >&2
    fail=1
  fi
  if grep -q 'competitor_bench/scale.py' ".github/workflows/release.yml" && \
     ! sed -n '/^  tier3-scale:/,/^  [a-z][a-z0-9_-]*:$/p' ".github/workflows/release.yml" | grep -q 'competitor_bench/scale.py'; then
    echo "ARCH: release.yml's scale leg must live in the tier3-scale job (CI-09)" >&2
    fail=1
  fi
  if grep -q 'competitor_bench/bench.py' ".github/workflows/release.yml" && \
     ! sed -n '/^  tier3-matrix:/,/^  [a-z][a-z0-9_-]*:$/p' ".github/workflows/release.yml" | grep -q 'competitor_bench/bench.py'; then
    echo "ARCH: release.yml's matrix leg must live in the tier3-matrix job (CI-09)" >&2
    fail=1
  fi
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

# workflow test 4 — test_perf_smoke_remains_wired: the perf smoke is a
# ci.yml job (CI-03 — folded from perf-smoke.yml, fast exit on
# non-matching paths so the required check never pends) carrying the
# review's five cells (point lookup, write throughput, scan, hot-cache,
# small compaction) under the 3x budget
SMOKE=.github/workflows/ci.yml
if ! grep -qE '^  perf-smoke:' "$SMOKE"; then
  echo "ARCH: ci.yml is missing the perf-smoke job — CI-03 folds perf-smoke.yml into ci.yml" >&2
  fail=1
elif ! grep -q 'perf-smoke.sh' "$SMOKE"; then
  echo "ARCH: the perf-smoke job must run scripts/perf-smoke.sh" >&2
  fail=1
elif ! grep -q '3x' "$SMOKE"; then
  echo "ARCH: the perf-smoke job must declare the 3x budget" >&2
  fail=1
fi
for gone in perf-smoke coverage-floor; do
  if [ -f ".github/workflows/$gone.yml" ]; then
    echo "ARCH: .github/workflows/$gone.yml still exists — CI-03 folds it into ci.yml" >&2
    fail=1
  fi
done
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

# workflow test 6 — test_build_jobs_cached (CI-05): every cargo build
# job in the three workflows carries Swatinem/rust-cache (the action's
# default key covers OS + rust version + Cargo.lock) — a cache step can
# drop silently in a bad merge and every job pays the full compile
# again. The dependency-dag job never compiles (grep-only) and the
# docker job builds inside the image — neither is a build job.
for spec in "ci check test-linux lint build-release connectors python-sdk perf-smoke coverage-floor" \
            "benchmark shuffle benchmark guard self-regression-main competitor-scale competitor-matrix storage-mutation" \
            "release windows linux-gnu linux-musl macos-intel macos-arm pypi-wheel-matrix tier3-correctness tier3-correctness-windows tier3-coverage tier3-scale tier3-matrix"; do
  wf="${spec%% *}"
  for job in ${spec#* }; do
    if ! sed -n "/^  $job:/,/^  [a-z][a-z0-9_-]*:$/p" ".github/workflows/$wf.yml" | grep -q 'Swatinem/rust-cache'; then
      echo "ARCH: $wf.yml job $job builds without Swatinem/rust-cache (CI-05)" >&2
      fail=1
    fi
  done
done

# workflow test 7 — test_required_checks_never_path_filter (CI-06): the
# required-check invariant — ci.yml must carry NO workflow-level path
# filter (a path-gated ci.yml skips, and a required skipped check pends
# forever — the review's §16 trap); its gates run always and decide
# inside (the fast exits). And the benchmark owner's trigger set is the
# CI-06 protected paths: storage/kernel/engines/benchmarks/
# competitor_bench/Cargo.lock (the paths that can move the gate-5 ratio)
# + the wiring self-paths — crates/compiler + crates/runtime are gone
# (they cannot move the 1M storage ratio, and a non-matching PR must not
# pay a 1M guard run).
if grep -qE '^  paths:|^    paths:' "$CIWF"; then
  echo "ARCH: ci.yml carries a workflow-level path filter — a required check can pend (§16)" >&2
  fail=1
fi
for path in crates/storage crates/kernel crates/engines benchmarks scripts/competitor_bench Cargo.lock; do
  # BOTH trigger blocks (pull_request AND push) must carry each path: a
  # path dropped from one block keeps a whole-file grep green while the
  # per-PR guard silently dies for it — the L-20 mutation harness caught
  # exactly that (m7-remove-protected-path).
  if [ "$(grep -c "'$path" "$BENCH")" -lt 2 ]; then
    echo "ARCH: $BENCH trigger paths lack the CI-06 protected path: $path" >&2
    fail=1
  fi
done
for gone in crates/compiler crates/runtime; do
  if grep -q "'$gone" "$BENCH"; then
    echo "ARCH: $BENCH trigger paths still carry $gone — outside the CI-06 protected set" >&2
    fail=1
  fi
done

# workflow test 8 — test_hybrid_knowledge_workload_wired (CI-07): the
# flagship hybrid knowledge-query cell (identity resolution -> metadata
# filter -> traversal -> semantic retrieval -> ranking end-to-end) lives
# in bench.py, and the nightly competitor-matrix job runs the harness
# against the composed stacks (pgvector PG, Neo4j, qdrant, Mongo).
if ! grep -q 'knowledge_query' scripts/competitor_bench/bench.py; then
  echo "ARCH: bench.py lacks the knowledge_query cell (CI-07)" >&2
  fail=1
fi
if ! grep -qE '^  competitor-matrix:' "$BENCH"; then
  echo "ARCH: $BENCH lacks the competitor-matrix job (CI-07)" >&2
  fail=1
fi
if ! sed -n '/^  competitor-matrix:/,/^  [a-z][a-z0-9_-]*:$/p' "$BENCH" | grep -q 'competitor_bench/bench.py'; then
  echo "ARCH: the competitor-matrix job must run bench.py (CI-07)" >&2
  fail=1
fi
for img in pgvector/pgvector neo4j:5-community qdrant/qdrant mongo:7; do
  if ! sed -n '/^  competitor-matrix:/,/^  [a-z][a-z0-9_-]*:$/p' "$BENCH" | grep -q "$img"; then
    echo "ARCH: the competitor-matrix job lacks the composed-stack image: $img (CI-07)" >&2
    fail=1
  fi
done

# workflow test 9 — test_reproducible_results_and_reports (CI-08): the §13
# schema (cpu/mem/disk/environment + harness-SHA, enforced by
# artifact_schema.validate_competitor), the §18 version pins (no :latest
# anywhere the harness or the job names images), and the §14 report trio
# (json/md/csv — the csv leg written by bench.py and uploaded by the job).
if ! grep -q 'cpu_seconds' scripts/competitor_bench/bench.py; then
  echo "ARCH: bench.py lacks the §13 cpu_seconds field (CI-08)" >&2
  fail=1
fi
if ! grep -q 'result.csv' scripts/competitor_bench/bench.py; then
  echo "ARCH: bench.py lacks the §14 csv report leg (CI-08)" >&2
  fail=1
fi
if ! grep -q 'validate_competitor' scripts/artifact_schema.py; then
  echo "ARCH: artifact_schema.py lacks the §13 competitor validator (CI-08)" >&2
  fail=1
fi
if grep -q ':latest' scripts/competitor_bench/containers.sh; then
  echo "ARCH: containers.sh carries an unpinned :latest image (§18, CI-08)" >&2
  fail=1
fi
matrix=$(sed -n '/^  competitor-matrix:/,/^  [a-z][a-z0-9_-]*:$/p' "$BENCH")
if printf '%s\n' "$matrix" | grep -q ':latest'; then
  echo "ARCH: the competitor-matrix job carries an unpinned :latest image (§18, CI-08)" >&2
  fail=1
fi
if ! printf '%s\n' "$matrix" | grep -q 'artifact_schema.py docs/certification/competitors/result.json'; then
  echo "ARCH: the competitor-matrix job must schema-validate its artifact (§13, CI-08)" >&2
  fail=1
fi
if ! printf '%s\n' "$matrix" | grep -q 'result.csv'; then
  echo "ARCH: the competitor-matrix job must upload the §14 csv leg (CI-08)" >&2
  fail=1
fi

# workflow test 10 — test_release_tier3_certification (CI-09): the release
# workflow carries the TESTING-PLAN §6 evidence pack — full correctness on
# both OSes (the CI invocation verbatim, gated cells included), the
# coverage floor, the full-scale harness, and the competitor matrix with
# its §13 schema check + §18 pinned images.
for job in tier3-correctness tier3-correctness-windows tier3-coverage tier3-scale tier3-matrix; do
  if ! grep -qE "^  $job:" "$REL"; then
    echo "ARCH: $REL lacks the Tier-3 job: $job (CI-09)" >&2
    fail=1
  fi
done
if ! grep -q 'cargo test --workspace -- $(bash scripts/skip-list.sh)' "$REL"; then
  echo "ARCH: the Tier-3 correctness jobs must reuse the CI suite invocation (CI-09)" >&2
  fail=1
fi
if ! grep -q 'check-coverage-floor.sh' "$REL"; then
  echo "ARCH: $REL lacks the Tier-3 coverage-floor leg (CI-09)" >&2
  fail=1
fi
if ! grep -q 'competitor_bench/scale.py' "$REL"; then
  echo "ARCH: $REL lacks the Tier-3 scale harness leg (CI-09)" >&2
  fail=1
fi
# L-27 release round 3: the 1M embedded leg cannot fit a 360-minute job on
# the public 2-core runner (measured: the 100k cells alone cost ~50 min;
# the 1M leg ran 5h00m without finishing before the job cap killed it).
# The release-path scale job carries 100k + MCP + txn; the 1M storage cells
# ride the weekly benchmark.yml guard (CI-10) and the full 1M embedded
# numbers live in the published dev-machine report. The --no-1m pin keeps
# the 1M leg out of the release path — a drift back fails this gate. The
# pin is a one-line live-invocation match (the L-20 lesson), so the flag
# must sit on the scale.py line itself.
t3s=$(sed -n '/^  tier3-scale:/,/^  [a-z][a-z0-9_-]*:$/p' "$REL")
if ! printf '%s\n' "$t3s" | grep -q -- 'scale.py --no-1m'; then
  echo "ARCH: the tier3-scale job must run the scale harness with --no-1m (the 1M leg cannot fit a 360m CI job, L-27 round 3)" >&2
  fail=1
fi
# L-27 release round 4: the harness's PG txn cells need the pgvector
# extension (bench.py CREATE EXTENSION vector); the tier3-scale job's
# service ran postgres:16-alpine, which lacks it, and the first release-path
# run to reach the PG leg died on FeatureNotSupported (run 36608915855).
# Both CI homes of the full harness must use the matrix job's image.
for scale_home in "release.yml:tier3-scale" "benchmark.yml:competitor-scale"; do
  wf=${scale_home%%:*}; job=${scale_home##*:}
  blk=$(sed -n "/^  $job:/,/^  [a-z][a-z0-9_-]*:$/p" ".github/workflows/$wf")
  if ! printf '%s\n' "$blk" | grep -q 'image: pgvector/pgvector:pg16'; then
    echo "ARCH: the $job job's PG service must use pgvector/pgvector:pg16 (bench.py needs the vector extension, L-27 round 4)" >&2
    fail=1
  fi
done
if ! grep -q 'competitor_bench/bench.py' "$REL"; then
  echo "ARCH: $REL lacks the Tier-3 competitor-matrix leg (CI-09)" >&2
  fail=1
fi
t3m=$(sed -n '/^  tier3-matrix:/,/^  [a-z][a-z0-9_-]*:$/p' "$REL")
if ! printf '%s\n' "$t3m" | grep -q 'artifact_schema.py docs/certification/competitors/result.json'; then
  echo "ARCH: the tier3-matrix job must schema-validate its artifact (§13, CI-09)" >&2
  fail=1
fi
if printf '%s\n' "$t3m" | grep -q ':latest'; then
  echo "ARCH: the tier3-matrix job carries an unpinned image (§18, CI-09)" >&2
  fail=1
fi
for img in pgvector/pgvector neo4j:5-community qdrant/qdrant mongo:7; do
  if ! printf '%s\n' "$t3m" | grep -q "$img"; then
    echo "ARCH: the tier3-matrix job lacks the composed-stack image: $img (CI-09)" >&2
    fail=1
  fi
done

# workflow test 11 — test_guard_rss_weekly_only (CI-10): RSS is evidence,
# not a gate row (gate5-check ratios the workload p50 rows only), and its
# loader child re-seeds the whole 1M dataset (~half the guard's wall time).
# The guard arms it on the weekly/dispatch runs only (the arm step carries
# the schedule/dispatch gate inside the guard slice); the 100K main job
# stays armed (its re-seed is minutes within a 240-min budget).
guard=$(sed -n '/^  guard:/,/^  [a-z][a-z0-9_-]*:$/p' "$BENCH")
if ! printf '%s\n' "$guard" | grep -q 'AIKOQL_RSS=1'; then
  echo "ARCH: the guard job lacks the RSS arm (CI-10)" >&2
  fail=1
fi
if ! printf '%s\n' "$guard" | grep -q "if: github.event_name == 'schedule' || github.event_name == 'workflow_dispatch'"; then
  echo "ARCH: the guard job must gate its RSS arm to the weekly/dispatch runs (CI-10)" >&2
  fail=1
fi
main=$(sed -n '/^  self-regression-main:/,/^  [a-z][a-z0-9_-]*:$/p' "$BENCH")
if ! printf '%s\n' "$main" | grep -q 'AIKOQL_RSS=1'; then
  echo "ARCH: the self-regression-main job lacks the RSS arm (CI-10)" >&2
  fail=1
fi

# workflow test 12 — test_compose_aikoql_no_host_ports (CI-11): the aikoql
# compose service publishes no host ports. The server fails closed on
# non-loopback binds (R1/R3 validate_listen; CI pins `--listen 0.0.0.0`
# must exit 2), so docker's -p proxy arrives on the container IP where
# nothing listens — the mappings were dead as shipped. The host client
# contract is MCP-over-stdio (docker run -i) or a sidecar that shares the
# network namespace.
for cf in docker-compose.yml docker-compose.release.yml; do
  if sed -n '/^  aikoql:/,/^  [a-z][a-z0-9_-]*:$/p' "$cf" | grep -qE '^ *- "90(90|91):'; then
    echo "ARCH: $cf publishes dead aikoql host ports (CI-11)" >&2
    fail=1
  fi
done

# workflow test 13 — test_coverage_floor_skips_alloc_pins (CI-12): the
# instrumented run perturbs the counting allocator (PR #7's first
# llvm-cov run: restart_index_reparse_pin's a2<=4 budget saw 5 under
# instrumentation while both plain suites passed it), so the absolute
# alloc-budget pin is skipped there and stays enforced by the plain CI
# suites.
if ! grep -q -- '--skip restart_index_reparse_pin' scripts/check-coverage-floor.sh; then
  echo "ARCH: check-coverage-floor.sh must skip the alloc-budget pin under instrumentation (CI-12)" >&2
  fail=1
fi

# workflow test 14 — test_docker_job_parses_compose (CI-13): the compose
# files are config-under-test, not docs — the PR #7 watch found the docker
# job never executed them, so the dead host-port mappings shipped
# unnoticed (CI-11). The job now parses both files with the token envs the
# `:?` gates require.
dock=$(sed -n '/^  docker:/,/^  [a-z][a-z0-9_-]*:$/p' "$CIWF")
for cf in docker-compose.yml docker-compose.release.yml; do
  if ! printf '%s\n' "$dock" | grep -q "compose -f $cf config --quiet"; then
    echo "ARCH: the docker job does not parse $cf (CI-13)" >&2
    fail=1
  fi
done

# workflow test 15 — test_gosdk_binary_path_depth (CI-13): the go-sdk
# job's AIKOQL_MCP_BIN must name the workspace-root release binary — the
# first CI run failed at spawn when ../../target resolved to crates/target
# (one level short; the workflow expression had never been executed
# anywhere before the push).
gs=$(sed -n '/^  go-sdk:/,/^  [a-z][a-z0-9_-]*:$/p' "$CIWF")
if ! printf '%s\n' "$gs" | grep -q '"$PWD/../../../target/release/aikoql-mcp"'; then
  echo "ARCH: the go-sdk job's AIKOQL_MCP_BIN is not pinned at the workspace-root depth (CI-13)" >&2
  fail=1
fi

# workflow test 16 — test_1m_baseline_schema (CI-14, lands L-24): the
# committed 1M v2 baseline must satisfy the §13/M47 schema (fresh=False —
# baselines are historical by design). PR #7's gate5-check died on the
# pre-§13 *_us rows AFTER the 3h suite ran: the baseline had never been
# validated by the named-error schema — only the fresh twin was, and the
# stale baseline let the whole guard fail at the last step.
if ! python3 -c "import sys; sys.path.insert(0, 'scripts'); from artifact_schema import validate_1m; validate_1m('artifacts/storage-engine-v2/result-1m-aikoql-v2.json', fresh=False)" >/dev/null 2>&1; then
  echo "ARCH: the committed 1M v2 baseline fails the §13/M47 schema (CI-14)" >&2
  fail=1
fi

# workflow test 17 — test_fuzz_estate_wired (F-04, PR #7 fuzz review
# F-TDD-07 "removing a target or nightly invocation is undetected"): the
# nightly proptest arm, the storage-mutation job (all eleven §12 mutants),
# and the F-02 lifecycle state machine must all stay wired — a silent drop
# of fuzz coverage must fail this gate. The §12 acceptance is "every
# selected mutant is killed by at least one named regression"; a surviving
# mutant is a test-suite defect, so the harness's all-mode exit 0 means
# every killer fired.
if ! grep -q 'PROPTEST_CASES=4096' "$BENCH"; then
  echo "ARCH: $BENCH lost the nightly proptest arm (PROPTEST_CASES=4096, F-04)" >&2
  fail=1
fi
if ! grep -qE '^  storage-mutation:' "$BENCH"; then
  echo "ARCH: $BENCH lost the storage-mutation job (F-04)" >&2
  fail=1
fi
if ! grep -qE '^  storage-mutation:' "$BENCH" || \
   ! sed -n '/^  storage-mutation:/,/^  [a-z][a-z0-9_-]*:$/p' "$BENCH" | grep -q 'storage-mutation-harness.sh all'; then
  echo "ARCH: the storage-mutation job must run the harness in all-mode (F-04)" >&2
  fail=1
fi
for mid in m-s1-restart-count m-s2-checksum m-s3-tombstone m-s4-newest-wins \
           m-s5-duplicate-guard m-s6-delta-coverage m-s7-current-before-manifest \
           m-s8-wal-truncate-early m-s9-cache-transparency m-s10-scan-skip \
           m-s11-placement-direct; do
  if ! grep -q "$mid" scripts/storage-mutation-harness.sh; then
    echo "ARCH: storage-mutation-harness.sh lost the §12 mutant: $mid (F-04)" >&2
    fail=1
  fi
done
if ! grep -q 'prop_lifecycle_state_machine_matches_model_across_reopens' crates/storage/aikoql-v2/tests/proptest_oracles.rs; then
  echo "ARCH: proptest_oracles.rs lost the F-02 lifecycle state machine (F-04)" >&2
  fail=1
fi

# workflow test 18 — test_sdk_compat_wired (D-03, SDK plan §3.2): the dag
# job must run scripts/check-sdk-compat.sh and the machine-readable
# contract must exist — either can rot silently in a bad merge (the
# check-chain precedent: every other gate has a wiring pin here).
if ! grep -q 'bash scripts/check-sdk-compat.sh' "$CIWF"; then
  echo "ARCH: the dag job does not run check-sdk-compat.sh (D-03)" >&2
  fail=1
fi
if [ ! -f protocol/compatibility.json ]; then
  echo "ARCH: protocol/compatibility.json missing (D-03)" >&2
  fail=1
fi

# workflow test 19 — test_conformance_vectors_frozen_schema (D-11, SDK plan
# §7/§23): every vector under tests/sdk-conformance/ uses only the frozen
# api-v1 operation set and SDK-012 error codes — a drift would silently
# fork what "identical across languages" means for the shared runner.
if ! python3 -c "
import glob, json, sys
root = 'tests/sdk-conformance'
ops = {o['name'] for o in json.load(open('protocol/api-v1.json'))['operations']}
codes = {e['code'] for e in json.load(open('protocol/errors.json'))['codes']}
bad = []
for p in sorted(glob.glob(root + '/**/*.json', recursive=True)):
    v = json.load(open(p))
    for i, op in enumerate(v.get('operations', [])):
        name = op.get('op')
        if name not in ops:
            bad.append(f'{p} op {i}: {name} not in api-v1')
        ee = op.get('expect_error')
        if ee and ee not in codes:
            bad.append(f'{p} op {i}: expect_error {ee} not in SDK-012')
if bad:
    print('\n'.join(bad)); sys.exit(1)
" >/dev/null 2>&1; then
  echo "ARCH: a conformance vector drifts from the frozen api-v1/SDK-012 schema (D-11)" >&2
  fail=1
fi

# workflow test 20 — test_go_module_tag_wired (D-18, SDK plan §27): the
# release must tag the Go module at sdk/go/v<version> — pkg.go.dev serves
# the module from that tag; a module that only exists at the repo tag is
# invisible to the Go ecosystem (the D-18 row's RED: "Go has no tagged
# module"). The job gates on github-release success (a module tag for a
# release that never landed is a phantom version).
if ! sed -n '/^  go-tag:/,/^  [a-z][a-z0-9_-]*:$/p' "$REL" | grep -q 'sdk/go/'; then
  echo "ARCH: release.yml has no go-tag job pushing the sdk/go/ module tag (D-18)" >&2
  fail=1
fi
if grep -q '^  go-tag:' "$REL" && \
   ! sed -n '/^  go-tag:/,/^  [a-z][a-z0-9_-]*:$/p' "$REL" | grep -q 'github-release'; then
  echo "ARCH: the go-tag job is not gated on the github-release job (D-18)" >&2
  fail=1
fi

# workflow test 21 — test_crates_io_publish_chain (D-18, SDK plan §27):
# the release publishes the Rust SDK's closure to crates.io in dependency
# order — a path-only dep fails `cargo publish` (no version requirement)
# and an out-of-order publish is rejected by the registry (the dependency
# does not exist yet). The frozen order is the [dependencies] topo sort:
# native → kernel → storage-v2 → graph → vector → scheduler → semantic →
# compiler → runtime → sdk.
if ! grep -q 'CRATES="aikoql-native aikoql-kernel aikoql-storage-v2 aikoql-graph aikoql-vector aikoql-scheduler aikoql-semantic aikoql-compiler aikoql-runtime aikoql-sdk"' "$REL"; then
  echo "ARCH: release.yml loses the frozen crates.io publish chain (D-18)" >&2
  fail=1
fi

# workflow test 22 — test_maven_central_publish (D-18, SDK plan §27):
# the Java SDK must ship to Maven Central (the job's skip guard probes
# repo1.maven.org — the published location — and the pom version must
# equal the release version before any upload; the pom's Central Portal
# plugin does the upload itself). Like every publisher it gates on
# github-release so a failed release never publishes artifacts.
if ! sed -n '/^  maven-central-publish:/,/^  [a-z][a-z0-9_-]*:$/p' "$REL" | grep -q 'repo1.maven.org/maven2/io/aikoql/aikoql-client'; then
  echo "ARCH: the maven-central-publish job loses the Maven Central skip guard (D-18)" >&2
  fail=1
fi
if grep -q '^  maven-central-publish:' "$REL" && \
   ! sed -n '/^  maven-central-publish:/,/^  [a-z][a-z0-9_-]*:$/p' "$REL" | grep -q 'help:evaluate'; then
  echo "ARCH: the maven-central-publish job lost the pom/release version parity assert (D-18)" >&2
  fail=1
fi
if grep -q '^  maven-central-publish:' "$REL" && \
   ! sed -n '/^  maven-central-publish:/,/^  [a-z][a-z0-9_-]*:$/p' "$REL" | grep -q 'github-release'; then
  echo "ARCH: the maven-central-publish job is not gated on the github-release job (D-18)" >&2
  fail=1
fi

# workflow test 23 — test_pypi_wheel_matrix (D-18, SDK plan §27):
# the Python SDK ships abi3-py39 wheels — one wheel per platform covers
# py3.9+, so the release must build on all four (x86_64 linux/windows/
# macos + arm64 macos) and install-check each wheel against the artifact
# itself before upload: a wheel that cannot be installed or imported
# fails the release here, not in the wild.
if ! grep -qF 'os: [ubuntu-latest, windows-latest, macos-13, macos-14]' "$REL"; then
  echo "ARCH: release.yml lost the 4-OS pypi wheel matrix (D-18)" >&2
  fail=1
fi
if grep -q '^  pypi-wheel-matrix:' "$REL" && \
   ! sed -n '/^  pypi-wheel-matrix:/,/^  [a-z][a-z0-9_-]*:$/p' "$REL" | grep -q 'python -c "import aikoql"'; then
  echo "ARCH: the pypi-wheel-matrix job lost the wheel install smoke (D-18)" >&2
  fail=1
fi

# workflow test 24 — test_sdk_release_cert (D-18, SDK plan §27): every
# release must certify the SDKs before the tag means anything — one
# sdk-release-cert job runs the battery (version parity, conformance,
# fuzz smoke, real-server integration, install smokes, example, SBOM)
# after github-release succeeded, or a broken SDK ships green.
if ! grep -q '^  sdk-release-cert:' "$REL"; then
  echo "ARCH: release.yml lost the sdk-release-cert job (D-18)" >&2
  fail=1
fi
if grep -q '^  sdk-release-cert:' "$REL" && \
   ! sed -n '/^  sdk-release-cert:/,/^  [a-z][a-z0-9_-]*:$/p' "$REL" | grep -q 'scripts/sdk-release-cert.sh'; then
  echo "ARCH: the sdk-release-cert job lost its certification script (D-18)" >&2
  fail=1
fi
if grep -q '^  sdk-release-cert:' "$REL" && \
   ! sed -n '/^  sdk-release-cert:/,/^  [a-z][a-z0-9_-]*:$/p' "$REL" | grep -q 'github-release'; then
  echo "ARCH: the sdk-release-cert job lost its github-release gate (D-18)" >&2
  fail=1
fi
if grep -q '^  sdk-release-cert:' "$REL" && \
   ! sed -n '/^  sdk-release-cert:/,/^  [a-z][a-z0-9_-]*:$/p' "$REL" | grep -q 'syft'; then
  echo "ARCH: the sdk-release-cert job lost the SBOM leg (D-18)" >&2
  fail=1
fi

# workflow test 25 — test_ts_sdk_job (D-19, SDK plan §28): the TypeScript
# SDK's real-server suite must run on every PR or a TS regression ships
# without the client ever touching the freshly built server.
if ! grep -q '^  ts-sdk:' "$CIWF"; then
  echo "ARCH: ci.yml lost the ts-sdk job (D-19)" >&2
  fail=1
fi
if grep -q '^  ts-sdk:' "$CIWF" && \
   ! sed -n '/^  ts-sdk:/,/^  [a-z][a-z0-9_-]*:$/p' "$CIWF" | grep -q 'node --test tests/'; then
  echo "ARCH: the ts-sdk job lost its suite command (D-19)" >&2
  fail=1
fi
# workflow test 26 — test_java_sdk_job (D-19, SDK plan §28): the Java
# SDK's surefire suite (real-server pin + fuzz seed sweep included) must
# run on every PR.
if ! grep -q '^  java-sdk:' "$CIWF"; then
  echo "ARCH: ci.yml lost the java-sdk job (D-19)" >&2
  fail=1
fi
if grep -q '^  java-sdk:' "$CIWF" && \
   ! sed -n '/^  java-sdk:/,/^  [a-z][a-z0-9_-]*:$/p' "$CIWF" | grep -q 'mvn -B test'; then
  echo "ARCH: the java-sdk job lost its surefire suite (D-19)" >&2
  fail=1
fi
# workflow test 27 — test_rust_sdk_job (D-19, SDK plan §28): the reference
# SDK's suite (conformance + embedded + fault + native + fuzz pins) must
# run on every PR.
if ! grep -q '^  rust-sdk:' "$CIWF"; then
  echo "ARCH: ci.yml lost the rust-sdk job (D-19)" >&2
  fail=1
fi
if grep -q '^  rust-sdk:' "$CIWF" && \
   ! sed -n '/^  rust-sdk:/,/^  [a-z][a-z0-9_-]*:$/p' "$CIWF" | grep -q 'cargo test -p aikoql-sdk'; then
  echo "ARCH: the rust-sdk job lost its suite command (D-19)" >&2
  fail=1
fi

if [ $fail -ne 0 ]; then exit 1; fi
echo "architecture hygiene (storage + workflow legs) — OK"
