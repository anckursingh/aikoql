#!/usr/bin/env bash
# PR6-F7 (P1-7): coverage floor on the codec/replay trio.
#
# The next "it round-trips but nobody asserts it" shows up RED: the three
# PR6-003 files (checkpoint/snapshot/wal) must hold their committed
# coverage baseline on every storage-touching change. A decrease fails;
# the tolerance absorbs report rounding only, not real regressions.
#
# Baseline: artifacts/coverage/coverage-baseline.json — per-file
# line-coverage percentages from the storage-v2 suite under
# cargo-llvm-cov, recorded with the toolchain that produced them (rustc
# and cargo-llvm-cov versions), so drift is explainable and the
# re-baseline is deliberate.
set -euo pipefail
cd "$(dirname "$0")/.."

BASE=artifacts/coverage/coverage-baseline.json
if [ ! -f "$BASE" ]; then
  echo "COVERAGE FLOOR: baseline missing: $BASE" >&2
  exit 1
fi
command -v cargo-llvm-cov >/dev/null 2>&1 || {
  echo "COVERAGE FLOOR: cargo-llvm-cov not on PATH" >&2
  exit 1
}
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Coverage floor — instrumented storage-v2 suite run..."
cargo llvm-cov test -p aikoql-storage-v2 --all-features --no-report
cargo llvm-cov report --summary-only > "$tmp/report.txt"

python3 - "$BASE" "$tmp/report.txt" <<'EOF'
import json
import sys

base = json.load(open(sys.argv[1], encoding="utf-8"))
tolerance = float(base.get("tolerance_pct", 0.05))
cells = base.get("cells", {})

by_name = {}
for line in open(sys.argv[2], encoding="utf-8"):
    parts = line.split()
    if len(parts) < 10:
        continue
    # Summary row shape: filename, then regions/missed/cover, functions/
    # missed/executed, lines/missed/cover — line cover is the 9th field
    # after the name, and may be "-" when no lines are instrumented.
    if parts[9] in ("-", "Cover"):
        continue
    by_name[parts[0].replace("\\", "/")] = parts[9]

failed = False
for path, want in sorted(cells.items()):
    key = next((k for k in by_name if k.endswith("/" + path)), None)
    if key is None:
        print(f"COVERAGE FLOOR FAIL: {path} missing from the report")
        failed = True
        continue
    got = float(by_name[key].rstrip("%"))
    floor = float(want) - tolerance
    ok = "OK" if got >= floor else "BELOW FLOOR"
    if got < floor:
        failed = True
    print(f"{path}: {got:.2f}% line coverage (floor {floor:.2f}%, "
          f"baseline {float(want):.2f}%) — {ok}")
sys.exit(1 if failed else 0)
EOF
