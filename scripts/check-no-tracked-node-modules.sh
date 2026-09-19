#!/usr/bin/env bash
# PR6-R2-012: CI fails if tracked paths match **/node_modules/**.
# Vendored node_modules = enormous diffs, review noise, lockfile drift;
# dependencies install through lockfiles, never a committed tree.
#
# Usage: check-no-tracked-node-modules.sh [ref]   (default: HEAD)
# With a ref, the gate checks that ref's tree instead of the index —
# used to prove the gate RED against the pre-deletion tree (origin/main).
set -euo pipefail
ref="${1:-HEAD}"
if [ "$ref" = "HEAD" ]; then
  tracked="$(git ls-files)"
else
  tracked="$(git ls-tree -r --name-only "$ref")"
fi
bad="$(printf '%s\n' "$tracked" | grep -E '(^|/)node_modules/' || true)"
if [ -n "$bad" ]; then
  echo "DAG VIOLATION: tracked node_modules paths (vendor through lockfiles):" >&2
  printf '%s\n' "$bad" >&2
  exit 1
fi
echo "no tracked node_modules paths — OK"
