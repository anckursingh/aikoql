#!/usr/bin/env bash
# PR6-F2 (P0-2): derive the ci.yml --skip args from tests/gated.toml — the
# registry is the single source of truth for the gated cells.
#
#   skip-list.sh             -> "--skip load_encryption_overhead ..."
#   skip-list.sh --ungated   -> the names with ungated_by = "none" (space-
#                              separated test-name filters) — the cells the
#                              benchmark-nightly "Gated cells ungated" job
#                              re-runs so their limits stay honest
set -euo pipefail
mode="${1:-skip}"
toml="$(dirname "$0")/../tests/gated.toml"
[ -f "$toml" ] || { echo "SKIP LIST: missing $toml" >&2; exit 1; }
if [ "$mode" = "--ungated" ]; then
  awk '/^\[\[gated\]\]/{t=""} /^test = /{t=$3; gsub(/"/,"",t)} /^ungated_by = "none"/{print t}' "$toml" | tr '\n' ' '
  echo
  exit 0
fi
if [ "$mode" = "--nextest" ]; then
  # PR6-F5 (P1-5): the same registry as a nextest filter expression —
  # nextest has no --skip; -E 'not test(a) and not test(b)' is the shape.
  # awk, not paste -d: paste cycles its -d list one CHARACTER at a time.
  # gsub strips the quotes and any \r (CRLF working tree on Windows — a
  # no-op on the LF Linux checkout).
  awk '/^test = /{gsub(/["\r]/,"",$3); printf "%snot test(%s)", (n++ ? " and " : ""), $3} END{print ""}' "$toml"
  exit 0
fi
if [ "$mode" != "skip" ]; then
  echo "usage: skip-list.sh [--ungated|--nextest]" >&2
  exit 2
fi
grep '^test = ' "$toml" | sed 's/^test = "\(.*\)"$/\1/' | while read -r t; do
  printf -- '--skip %s ' "$t"
done
echo
