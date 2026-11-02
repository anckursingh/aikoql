#!/usr/bin/env bash
# AG-62: one-command public trial — the AGENTIC-QUICKSTART Cloudflare-tunnel
# recipe in one command. Starts a cloudflared quick tunnel to the loopback
# serve surface, aikoql-mcp under it (AIKOQL_PUBLIC_URL = the tunnel URL),
# and prints the connector URL. Ctrl-C tears both down.
# Usage: public-trial.sh <db-path>
# Requires: cloudflared on PATH, aikoql-mcp on PATH (or set AIKOQL_MCP_BIN).
set -euo pipefail

DB=${1:?usage: public-trial.sh <db-path>}
BIN=${AIKOQL_MCP_BIN:-aikoql-mcp}

if ! command -v cloudflared >/dev/null 2>&1; then
  echo "cloudflared not found — install it first (brew install cloudflared, or the Cloudflare site)" >&2
  exit 1
fi

TMP=$(mktemp -d)
TUNNEL_PID=
cleanup() {
  [ -n "$TUNNEL_PID" ] && kill "$TUNNEL_PID" 2>/dev/null || true
  rm -rf "$TMP"
}
trap cleanup EXIT

cloudflared tunnel --url http://127.0.0.1:9091 >"$TMP/tunnel.log" 2>&1 &
TUNNEL_PID=$!

URL=
for _ in $(seq 1 60); do
  URL=$(sed -nE 's#.*(https://[a-z0-9-]+\.trycloudflare\.com).*#\1#p' "$TMP/tunnel.log" | head -1)
  [ -n "$URL" ] && break
  if ! kill -0 "$TUNNEL_PID" 2>/dev/null; then
    echo "cloudflared exited before the tunnel came up:" >&2
    cat "$TMP/tunnel.log" >&2
    exit 1
  fi
  sleep 0.5
done
if [ -z "$URL" ]; then
  echo "no tunnel URL from cloudflared after 30s:" >&2
  cat "$TMP/tunnel.log" >&2
  exit 1
fi
echo "tunnel: $URL"

if [ -z "${AIKOQL_ADMIN_PASSWORD:-}" ]; then
  if command -v openssl >/dev/null 2>&1; then
    export AIKOQL_ADMIN_PASSWORD=$(openssl rand -hex 12)
  else
    export AIKOQL_ADMIN_PASSWORD=$(od -An -N12 -tx1 /dev/urandom | tr -d ' \n')
  fi
  echo "generated admin password: $AIKOQL_ADMIN_PASSWORD (set AIKOQL_ADMIN_PASSWORD to choose)"
fi

export AIKOQL_PUBLIC_URL="$URL"
echo "connector URL: $URL/mcp (add as a custom connector on claude.ai)"
echo "press Ctrl-C to stop"
# Foreground child, not exec: the EXIT trap must tear the tunnel down
# when serve ends.
"$BIN" serve --metrics-addr 127.0.0.1:9091 "$DB"
