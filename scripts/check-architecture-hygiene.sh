#!/usr/bin/env bash
# Launch S-01: architecture hygiene gate — the storage leg (review 2 §8/§22,
# docs/IMPLEMENTATION-PLAN-LAUNCH.md). Five assertions, all RED against the
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
# The CI-01 workflow leg joins this script later; CI-04 wires it into the
# dag job (wiring rides the milestone whose GREEN makes the gate pass).
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
pins="$(grep -rnE 'AIKOQL_BACKEND|V2ADOPT' benchmarks scripts/competitor_bench \
  --include='*.rs' --include='*.py' --include='*.sh' 2>/dev/null || true)"
if [ -n "$pins" ]; then
  echo "$pins" | sed 's/^/ARCH: harness pins the v1 backend selection: /' >&2
  fail=1
fi

if [ $fail -ne 0 ]; then exit 1; fi
echo "architecture hygiene (storage leg) — OK"
