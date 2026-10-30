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

# TLS: with AIKOQL_TLS_CERT the default loopback --listen opens to every
# interface (the token is encrypted, so a published port works); the HTTP
# port only with AIKOQL_ALLOW_REMOTE_HTTP too (it needs logins); anything
# not the image default is left alone. ENV passes through `run`'s sh.
DEF="serve /data/aikoql --listen 127.0.0.1:9090 --metrics-addr 127.0.0.1:9091 --model-dir /data/models"
d="$t/tls"; mkdir -p "$d"
args() { echo "$DEF" | sed "s|/data|$d|g"; }
# shellcheck disable=SC2046
AIKOQL_TLS_CERT=/c.pem AIKOQL_TLS_KEY=/k.pem run "$d" $(args)
[ "$(cat "$t/args")" = "serve $d/aikoql --listen 0.0.0.0:9090 --metrics-addr 127.0.0.1:9091 --model-dir $d/models" ] ||
  bad "tls: --listen not opened, or HTTP opened without allow_remote_http ($(cat "$t/args"))"
# shellcheck disable=SC2046
AIKOQL_TLS_CERT=/c.pem AIKOQL_TLS_KEY=/k.pem AIKOQL_ALLOW_REMOTE_HTTP=true run "$d" $(args)
[ "$(cat "$t/args")" = "serve $d/aikoql --listen 0.0.0.0:9090 --metrics-addr 0.0.0.0:9091 --model-dir $d/models" ] ||
  bad "tls + allow_remote_http: both not opened ($(cat "$t/args"))"
# shellcheck disable=SC2046
run "$d" $(args)
[ "$(cat "$t/args")" = "serve $d/aikoql --listen 127.0.0.1:9090 --metrics-addr 127.0.0.1:9091 --model-dir $d/models" ] ||
  bad "no tls: the loopback binds changed ($(cat "$t/args"))"
AIKOQL_TLS_CERT=/c.pem run "$d" serve "$d/aikoql" --listen 127.0.0.1:7000 --replicate-listen 127.0.0.1:9090
[ "$(cat "$t/args")" = "serve $d/aikoql --listen 127.0.0.1:7000 --replicate-listen 127.0.0.1:9090" ] ||
  bad "tls: a non-default address was rewritten ($(cat "$t/args"))"

[ "$fail" = 0 ] || exit 1
echo "docker entrypoint — OK"
