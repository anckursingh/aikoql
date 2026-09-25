#!/usr/bin/env bash
# PR6-F2 (P0-2): the gated-cell registry (tests/gated.toml) must be the
# only source of skips and every registered name must exist in the tree.
#   1. the workflow wires the derivation (scripts/skip-list.sh) and carries
#      no inline --skip args — a skip added outside the registry would
#      drift from the accounting;
#   2. every registry entry is well-formed (test/reason/ungated_by/
#      verified_at, one each per entry);
#   3. every registered test name still exists in the tree — a renamed or
#      deleted test silently un-skips itself on CI (the dangerous drift).
# GATED_TOML overrides the registry path — used by the RED capture.
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
toml="${GATED_TOML:-$root/tests/gated.toml}"
ci="$root/.github/workflows/ci.yml"
[ -f "$toml" ] || { echo "SKIP DRIFT: missing $toml" >&2; exit 1; }
fail=0

# 1. wiring: the derivation exists and no inline --skip remains
if ! grep -q 'scripts/skip-list.sh' "$ci"; then
  echo "SKIP DRIFT: ci.yml does not use scripts/skip-list.sh — the registry is not wired" >&2
  fail=1
fi
# Comment lines are prose, not args: a comment quoting the old error text
# (ci.yml round 4) re-tripped this raw grep on main's dag job — the gate
# matches live argument lines only.
if grep -n -- '--skip' "$ci" 2>/dev/null | grep -vE '^[0-9]+:\s*#'; then
  echo "SKIP DRIFT: inline --skip args in ci.yml — the registry is the only skip source" >&2
  fail=1
fi

# 2. well-formed entries: one of each key per [[gated]] block
n="$(grep -c '^\[\[gated\]\]' "$toml" || true)"
if [ "$n" -eq 0 ]; then
  echo "SKIP DRIFT: no [[gated]] entries in $toml" >&2
  fail=1
fi
for k in '^test = ' '^reason = ' '^ungated_by = ' '^verified_at = '; do
  c="$(grep -c "$k" "$toml" || true)"
  if [ "$c" != "$n" ]; then
    echo "SKIP DRIFT: $k count $c != entries $n" >&2
    fail=1
  fi
done

# 3. every registered name exists in the tracked tree
dead="$(grep '^test = ' "$toml" | sed 's/^test = "\(.*\)"$/\1/' | while read -r t; do
  grep -rlF "fn $t" "$root/crates" --include='*.rs' --exclude-dir=target 2>/dev/null | grep -q . || echo "$t"
done)"
if [ -n "$dead" ]; then
  echo "SKIP DRIFT: gated test(s) no longer exist in the tree:" >&2
  printf '%s\n' "$dead" >&2
  fail=1
fi

[ $fail -eq 0 ] || exit 1
echo "gated-cell registry clean — OK"
