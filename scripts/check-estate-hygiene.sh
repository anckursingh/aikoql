#!/usr/bin/env bash
# PR6-R3-002: the estate deletions must stay deleted — the forbidden paths
# must be ABSENT from the tracked tree, not merely unreferenced in code.
# The reference greps in the dependency-dag job catch textual regressions;
# this catches a re-added file nothing references yet.
#
# Usage: check-estate-hygiene.sh [ref]   (default: HEAD)
# With a ref, the gate checks that ref's tree instead of the index —
# used to prove the gate RED against the pre-deletion tree (origin/main).
set -euo pipefail
ref="${1:-HEAD}"
if [ "$ref" = "HEAD" ]; then
  tracked="$(git ls-files)"
else
  tracked="$(git ls-tree -r --name-only "$ref")"
fi
bad="$(printf '%s\n' "$tracked" | grep -E '^(crates/cluster/proxy/|crates/sdk/go/|crates/sdk/java/|tests/universal_test_harness\.py|benchmarks/tests/load_test\.rs|tests/e2e/)' || true)"
if [ -n "$bad" ]; then
  echo "DAG VIOLATION: deleted-estate paths present in the tree (P3-M0/P3-M9 deletions — docs/sdk-proxy-decision.md):" >&2
  printf '%s\n' "$bad" >&2
  exit 1
fi
echo "deleted-estate paths absent — OK"

# S-04: the adoption-era env language is gone — V2ADOPT-era names must be
# ABSENT from the functional surfaces (crates/scripts/.github/gated.toml/
# AGENTS.md; historical docs keep the old names by design — this covers the
# active ones). The bracket in the pattern keeps the gate from self-matching.
langbad="$(git grep -nE 'V2ADOPT[_]' "$ref" -- crates scripts .github tests/gated.toml AGENTS.md || true)"
if [ -n "$langbad" ]; then
  echo "DAG VIOLATION: V2ADOPT-era language present (S-04 rename to STORAGE_*):" >&2
  printf '%s\n' "$langbad" >&2
  exit 1
fi
echo "V2ADOPT-era language absent from functional surfaces — OK"
