#!/usr/bin/env bash
# PR6-F5 (P1-5): residue sweepers around the nightly shuffle run. The
# cross-test interference the shuffle exists to catch also leaves residue:
# leaked listeners (the aikoql metrics corpse on 9091, 2026-09-20), temp
# suite dirs, and mutated committed files (the artifacts/ clobber class).
# "before" snapshots the runner state; "after" diffs against the snapshot
# and fails on anything new — even when the shuffle itself failed
# (the sweep may explain the failure).

set -euo pipefail
cd "$(dirname "$0")/.."
tmp="${TMPDIR:-/tmp}"
state="$tmp/aikoql-shuffle-residue"

case "${1:-}" in
  before)
    ss -tln 2>/dev/null | sort > "$state.ports" || true
    find "$tmp" -maxdepth 1 -name 'aikoql_*' 2>/dev/null | sort > "$state.dirs" || true
    git status --porcelain > "$state.tree" || true
    echo "residue snapshot armed"
    ;;
  after)
    [ -f "$state.ports" ] || { echo "RESIDUE: no before snapshot — run check-residue.sh before first" >&2; exit 1; }
    ss -tln 2>/dev/null | sort | comm -13 "$state.ports" - | grep . && {
      echo "RESIDUE: a listener appeared during the shuffle run" >&2; exit 1; }
    find "$tmp" -maxdepth 1 -name 'aikoql_*' 2>/dev/null | sort | comm -13 "$state.dirs" - | grep . && {
      echo "RESIDUE: a temp dir survived the shuffle run" >&2; exit 1; }
    git status --porcelain | comm -13 "$state.tree" - | grep . && {
      echo "RESIDUE: the shuffle run mutated tracked files" >&2; exit 1; }
    echo "residue sweep — clean"
    ;;
  *)
    echo "usage: check-residue.sh before|after" >&2
    exit 2
    ;;
esac
