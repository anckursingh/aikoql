#!/usr/bin/env bash
# The image's entrypoint (docker-entrypoint.sh): images 0.1.18–0.2.3 kept
# the aikoql-v2 directory at /data/aikoql.redb; the new default is
# /data/aikoql, so an upgraded container must move the old directory once —
# or "pull + recreate" would open an empty database. Run against a stub
# `aikoql` and a temp dir standing in for /data.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
ep="$root/docker-entrypoint.sh"
[ -f "$ep" ] || { echo "docker-entrypoint: $ep missing" >&2; exit 1; }
t="$(mktemp -d)"
trap 'rm -rf "$t"' EXIT
mkdir -p "$t/bin"
printf '#!/bin/sh\necho "$@" > "%s/args"\nexit 7\n' "$t" > "$t/bin/aikoql"
chmod +x "$t/bin/aikoql"
fail=0
bad() { echo "docker-entrypoint: $*" >&2; fail=1; }

# run DATA ARGS… — the entrypoint with /data → DATA, the stub on PATH
run() {
  local data="$1"; shift
  sed "s|/data|$data|g" "$ep" > "$t/ep.sh"
  rc=0
  PATH="$t/bin:$PATH" sh "$t/ep.sh" "$@" 2>/dev/null || rc=$?
}

# 1. an upgrade: the old directory moves to the new name, once
d="$t/up"; mkdir -p "$d/aikoql.redb"; touch "$d/aikoql.redb/CURRENT"
run "$d" serve "$d/aikoql" --listen 127.0.0.1:9090
[ -f "$d/aikoql/CURRENT" ] && [ ! -e "$d/aikoql.redb" ] || bad "upgrade: /data/aikoql.redb not moved to /data/aikoql"
[ "$rc" = 7 ] || bad "upgrade: exit $rc, not the server's 7"
[ "$(cat "$t/args")" = "serve $d/aikoql --listen 127.0.0.1:9090" ] || bad "upgrade: args not passed through ($(cat "$t/args"))"

# 2. the old path named explicitly: left where it is
d="$t/explicit"; mkdir -p "$d/aikoql.redb"
run "$d" serve "$d/aikoql.redb"
[ -d "$d/aikoql.redb" ] && [ ! -e "$d/aikoql" ] || bad "explicit old path: moved anyway"

# 3. both present: neither touched (the new one is the database)
d="$t/both"; mkdir -p "$d/aikoql.redb" "$d/aikoql"; touch "$d/aikoql/NEW"
run "$d" serve "$d/aikoql"
[ -d "$d/aikoql.redb" ] && [ -f "$d/aikoql/NEW" ] && [ ! -e "$d/aikoql/aikoql.redb" ] || bad "both present: touched"

# 4. a fresh volume: nothing to move, the server still runs
d="$t/fresh"; mkdir -p "$d"
run "$d" serve "$d/aikoql"
[ "$rc" = 7 ] && [ ! -e "$d/aikoql.redb" ] || bad "fresh volume: rc $rc"

# 5. a non-serve command (model install, --version) passes straight through
d="$t/other"; mkdir -p "$d/aikoql.redb"
run "$d" --version
[ "$(cat "$t/args")" = "--version" ] && [ -d "$d/aikoql.redb" ] || bad "--version: not a plain pass-through"

[ "$fail" = 0 ] || exit 1
echo "docker entrypoint — OK"
