#!/bin/sh
# The image's entrypoint: `aikoql "$@"`, after one upgrade step. Images
# 0.1.18–0.2.3 kept the aikoql-v2 directory at /data/aikoql.redb (a
# redb-era name); the default is now /data/aikoql. Serving that default on a
# volume that only has the old directory moves it once (a rename within the
# volume), so pull + recreate keeps the knowledge base.
set -e
if [ "$1" = serve ] && [ "$2" = /data/aikoql ] &&
   [ -d /data/aikoql.redb ] && [ ! -e /data/aikoql ]; then
  mv /data/aikoql.redb /data/aikoql
  echo "aikoql: moved the database /data/aikoql.redb to /data/aikoql (its new name)" >&2
fi
# With TLS (AIKOQL_TLS_CERT) the token travels encrypted, so the image's
# default loopback --listen opens to every interface and a published port
# works; the HTTP surface too when armed (AIKOQL_ALLOW_REMOTE_HTTP — the
# server then also requires logins). Only the image defaults are rewritten.
if [ "$1" = serve ] && [ -n "${AIKOQL_TLS_CERT:-}" ]; then
  remote_http=
  case "${AIKOQL_ALLOW_REMOTE_HTTP:-}" in 1 | [Tt][Rr][Uu][Ee]) remote_http=1 ;; esac
  prev=
  for a in "$@"; do
    shift
    case "$prev $a" in
      "--listen 127.0.0.1:9090") a=0.0.0.0:9090 ;;
      "--metrics-addr 127.0.0.1:9091") [ -n "$remote_http" ] && a=0.0.0.0:9091 ;;
    esac
    set -- "$@" "$a"
    prev=$a
  done
fi
exec aikoql "$@"
