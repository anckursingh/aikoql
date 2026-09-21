#!/usr/bin/env bash
# Residual-file cleaning (2026-09-21): target/ had grown to ~312 GB —
# debug/incremental 100 GB (407k cached compile units), debug/deps 184 GB
# (24k stale per-revision test binaries + their .pdb files), release
# 4.7 GB, llvm-cov-target 4.3 GB (144 accumulated .profraw files from the
# coverage workflow's custom target dir), plus stale aikoql temp dbs in
# %TEMP% left by killed test runs.
#
# Run between builds (a clean rebuild costs ~1h once). The %TEMP% sweep is
# age-capped at one day — the same cutoff the in-test startup sweepers
# (v2 common/mod.rs, kernel durability.rs) use — so a concurrent live
# run's fresh files are never touched. CI runners are ephemeral and
# self-clean; this script is for the local workstation.

set -euo pipefail
cd "$(dirname "$0")/.."

echo "== cargo clean (target/debug + target/release build output)"
cargo clean

# Custom target dirs cargo clean does not know about.
echo "== custom target-dir residue (coverage profraw, rust-analyzer, benches)"
rm -rf target/llvm-cov-target target/flycheck0 target/tmp \
       target/hybrid_bench target/maturin

# Stale test dbs and snapshots in %TEMP% (killed test runs never reach
# their TLS sweepers; the next test startup only purges its own pattern).
echo "== stale aikoql temp residue (>1 day old)"
if command -v cygpath >/dev/null 2>&1 && [ -n "${TEMP:-}" ]; then
  tmp="$(cygpath -u "$TEMP")" # Git Bash: TEMP is the Windows path
else
  tmp="${TMPDIR:-/tmp}"
fi
find "$tmp" -maxdepth 1 \
  \( -name 'aikoql-*' -o -name 'aikoql_*' -o -name '*.redb' \
     -o -name 'mcp-tcp-auth-*' -o -name 'qa2_prop*' \) \
  -mtime +1 -exec rm -rf {} + 2>/dev/null || true

echo "clean-residue done"
