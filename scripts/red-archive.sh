#!/usr/bin/env bash
# PR6-F1 (P0-1): capture a review RED as a reproducible artifact
# (docs/ARCHITECT-REVIEW-2026-09.md). Every disposition that cites a RED
# can point at docs/red-archive/<id>.red.log instead of prose.
#
#   red-archive.sh capture <id> <pre-fix-commit> -- <command...>
#
# Runs <command...> (from the current directory) and asserts it exits
# NON-zero — that is the RED. Writes the evidence pair:
#   docs/red-archive/<id>.red.log   command output + an EXIT= line
#   docs/red-archive/<id>.json      manifest: id, pre_fix_commit, command,
#                                   captured_at, captured_head, exit_code,
#                                   optional note
# Set RED_ARCHIVE_DIR to override the output directory (used when capturing
# from inside a worktree). Refuses to overwrite an existing archive unless
# FORCE=1.
#
# Test REDs whose test only exists in the fix commit: prepare a detached
# worktree at the fix commit, revert the fixed sources to the parent inside
# it, cd there, and invoke capture with the fix commit as <pre-fix-commit>
# (the RED was observed in that worktree state) and a note naming the
# reverted paths.
set -euo pipefail
if [ "${1:-}" != "capture" ]; then
  echo "usage: red-archive.sh capture <id> <pre-fix-commit> -- <command...>" >&2
  exit 2
fi
shift
id="$1"; commit="$2"; shift 2
[ "${1:-}" = "--" ] || { echo "expected -- before the command" >&2; exit 2; }
shift
[ $# -gt 0 ] || { echo "no command given" >&2; exit 2; }
root="$(git rev-parse --show-toplevel)"
dir="${RED_ARCHIVE_DIR:-$root/docs/red-archive}"
mkdir -p "$dir"
log="$dir/$id.red.log"; man="$dir/$id.json"
if [ -f "$log" ] || [ -f "$man" ]; then
  [ "${FORCE:-0}" = "1" ] || { echo "archive $id exists (set FORCE=1 to overwrite)" >&2; exit 2; }
fi
head="$(git rev-parse HEAD)"
start=$(date -u +%Y-%m-%dT%H:%M:%SZ)
set +e
"$@" > "$log" 2>&1
code=$?
set -e
echo "EXIT=$code" >> "$log"
if [ "$code" -eq 0 ]; then
  echo "archive $id: command exited 0 — not a RED" >&2
  exit 1
fi
cat > "$man" <<EOF
{
  "id": "$id",
  "pre_fix_commit": "$commit",
  "command": "$*",
  "captured_at": "$start",
  "captured_head": "$head",
  "exit_code": $code
}
EOF
echo "RED archived: $dir/$id.red.log (exit $code)"
