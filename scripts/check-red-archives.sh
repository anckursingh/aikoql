#!/usr/bin/env bash
# PR6-F1 (P0-1): every archive under docs/red-archive/ must be a complete
# RED artifact — a manifest carrying the required fields and a log carrying
# a non-zero exit marker. The archive logs NAME the deleted estate by
# design (they capture the estate gate's own RED output), so the SDK/proxy
# reference grep in ci.yml excludes this directory.
set -euo pipefail
dir="$(dirname "$0")/../docs/red-archive"
if [ ! -d "$dir" ]; then
  echo "RED ARCHIVES: missing $dir — the P0-1 archive directory must exist" >&2
  exit 1
fi
shopt -s nullglob
mans=("$dir"/*.json)
if [ ${#mans[@]} -eq 0 ]; then
  echo "RED ARCHIVES: no archives yet — nothing to validate"
  exit 0
fi
fail=0
for m in "${mans[@]}"; do
  id="$(basename "$m" .json)"
  log="$dir/$id.red.log"
  if [ ! -f "$log" ]; then
    echo "RED ARCHIVES: $id missing $id.red.log" >&2
    fail=1
    continue
  fi
  for k in '"id"' '"pre_fix_commit"' '"command"' '"captured_at"' '"captured_head"' '"exit_code"'; do
    if ! grep -q "$k" "$m"; then
      echo "RED ARCHIVES: $id manifest missing $k" >&2
      fail=1
    fi
  done
  if ! grep -q '^EXIT=[1-9][0-9]*$' "$log"; then
    echo "RED ARCHIVES: $id log lacks a non-zero EXIT marker" >&2
    fail=1
  fi
done
[ $fail -eq 0 ] || exit 1
echo "red archives well-formed — OK"
