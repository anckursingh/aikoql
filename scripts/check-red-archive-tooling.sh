#!/usr/bin/env bash
# The RED-archive tooling, tested on itself in a throwaway directory:
# 1. a command containing double quotes and a backslash must still produce
#    a manifest that parses as JSON (red-archive.sh wrote "command": "$*"
#    unescaped — an archive captured with quotes was invalid JSON);
# 2. check-red-archives.sh must reject a manifest that is not JSON (it only
#    grepped for the key names, so the invalid one above passed as OK).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail=0

RED_ARCHIVE_DIR="$tmp/a" bash "$root/scripts/red-archive.sh" capture quoted HEAD -- \
  bash -c 'echo "a \"quoted\" arg and a back\\slash"; exit 3' >/dev/null
if ! python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$tmp/a/quoted.json" 2>/dev/null; then
  echo "RED ARCHIVE TOOLING: red-archive.sh wrote a manifest that is not JSON for a quoted command" >&2
  fail=1
fi

mkdir -p "$tmp/b"
printf '%s\n' '{' '  "id": "broken",' '  "pre_fix_commit": "HEAD",' \
  '  "command": "bash -c "unescaped"",' '  "captured_at": "x",' \
  '  "captured_head": "x",' '  "exit_code": 1' '}' > "$tmp/b/broken.json"
printf 'output\nEXIT=1\n' > "$tmp/b/broken.red.log"
if RED_ARCHIVE_DIR="$tmp/b" bash "$root/scripts/check-red-archives.sh" >/dev/null 2>&1; then
  echo "RED ARCHIVE TOOLING: check-red-archives.sh accepted a manifest that is not JSON" >&2
  fail=1
fi

[ $fail -eq 0 ] || exit 1
echo "red-archive tooling (quoted commands, JSON validation) — OK"
