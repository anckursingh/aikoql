#!/usr/bin/env bash
# AG-51 gate — a kind cluster, the Helm chart, and the failover story:
# remember through the leader, kill the leader pod, the standby promotes
# and serves the write, and an OAuth sign-in issued before the kill keeps
# working against the new leader (grants live in the replicated db).
# Nightly owner (benchmark.yml), not PR CI. Needs: docker, kind, kubectl,
# helm, curl, openssl, python3.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NS="${NS:-aikoql}"
RELEASE="${RELEASE:-aikoql}"
KEEP_KIND="${KEEP_KIND:-0}"

die() { echo "kind-failover-e2e: $*" >&2; exit 1; }
for tool in docker kind kubectl helm curl openssl python3; do
  command -v "$tool" >/dev/null || die "$tool is not installed"
done

# --- cluster + image -------------------------------------------------------
kind get clusters | grep -q '^kind$' || kind create cluster --wait 300s
docker build -q -t aikoql:local "$ROOT" || die "docker build failed"
kind load docker-image aikoql:local

# --- install ---------------------------------------------------------------
kubectl create namespace "$NS" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
TOKEN="$(openssl rand -hex 16)"
ADMIN_PW="oauth-pw"
kubectl -n "$NS" delete secret aikoql-auth --ignore-not-found >/dev/null
kubectl -n "$NS" create secret generic aikoql-auth \
  --from-literal=token="$TOKEN" --from-literal=admin-password="$ADMIN_PW"
helm upgrade --install "$RELEASE" "$ROOT/deploy/helm/aikoql" \
  -n "$NS" --set image.tag=local --set auth.adminPasswordKey=admin-password \
  --wait --timeout 300s
kubectl -n "$NS" wait --for=condition=Ready pod \
  -l app.kubernetes.io/name=aikoql --timeout=300s

# --- helpers ---------------------------------------------------------------
leader() { # the lease holder is the pod name (--ha-node)
  kubectl -n "$NS" get lease primary -o jsonpath='{.spec.holderIdentity}'
}

PF_PID=""
pf() { # port-forward pod $1 -> localhost:4318
  [ -n "$PF_PID" ] && kill "$PF_PID" 2>/dev/null || true
  kubectl -n "$NS" port-forward "pod/$1" 4318:4318 >/dev/null 2>&1 &
  PF_PID=$!
  for _ in $(seq 60); do
    (echo > /dev/tcp/127.0.0.1/4318) 2>/dev/null && return 0
    sleep 1
  done
  die "port-forward to $1 never came up"
}
cleanup() {
  [ -n "$PF_PID" ] && kill "$PF_PID" 2>/dev/null || true
  [ "$KEEP_KIND" = "1" ] || kind delete cluster
}
trap cleanup EXIT

mcp_call() { # $1 = bearer; $2 = method; $3 = params json; echoes the response body
  local bearer="$1" method="$2" params="$3" id
  id=$((RANDOM))
  curl -fsS -H "Authorization: Bearer $bearer" \
    -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"$method\",\"params\":$params}" \
    http://127.0.0.1:4318/mcp
}

tool() { # $1 = bearer; $2 = session; $3 = tool; $4 = args json; echoes the tool payload
  mcp_call "$1" "tools/call" "{\"name\":\"$3\",\"arguments\":$4}" |
    python3 -c 'import json,sys; r=json.load(sys.stdin); print(r["result"]["content"][0]["text"])'
}

session() { # $1 = bearer; initialize and echo the Mcp-Session-Id
  curl -fsS -D- -o /dev/null -H "Authorization: Bearer $1" \
    -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"kind-e2e","version":"0"}}}' \
    http://127.0.0.1:4318/mcp |
    awk 'tolower($1)=="mcp-session-id:" {print $2}' | tr -d '\r'
}

jsonfield() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

# --- the leader serves writes ----------------------------------------------
LEADER=""
for _ in $(seq 60); do LEADER="$(leader)"; [ -n "$LEADER" ] && break; sleep 2; done
[ -n "$LEADER" ] || die "no lease holder after install"
pf "$LEADER"

SID="$(session "$TOKEN")"
[ -n "$SID" ] || die "initialize with the static token failed"
KOID="$(tool "$TOKEN" "$SID" "remember" '{"subject":"agent","type_name":"note","properties":{"n":7}}' | jsonfield '["koid"]')"
[ -n "$KOID" ] || die "remember on the leader failed"
echo "leader $LEADER remembered $KOID"

# --- an OAuth sign-in (RFC 9728) against the leader ------------------------
CLIENT="$(curl -fsS -H 'Content-Type: application/json' \
  -d '{"redirect_uris":["http://127.0.0.1:9/callback"],"client_name":"kind-e2e","token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"]}' \
  http://127.0.0.1:4318/oauth/register | jsonfield '["client_id"]')"
[ -n "$CLIENT" ] || die "RFC 7591 registration failed"
VERIFIER="$(openssl rand -hex 32)"
CHALLENGE="$(printf %s "$VERIFIER" | openssl dgst -sha256 -binary | base64 | tr '+/' '-_' | tr -d '=')"
CODE="$(curl -fsS -D- -o /dev/null -H 'Content-Type: application/x-www-form-urlencoded' \
  --data-urlencode "response_type=code" --data-urlencode "client_id=$CLIENT" \
  --data-urlencode "redirect_uri=http://127.0.0.1:9/callback" \
  --data-urlencode "code_challenge=$CHALLENGE" --data-urlencode "code_challenge_method=S256" \
  --data-urlencode "state=st-1" --data-urlencode "scope=mcp" \
  --data-urlencode "resource=http://127.0.0.1:4318/mcp" \
  --data-urlencode "username=admin" --data-urlencode "password=$ADMIN_PW" \
  --data-urlencode "approve=1" http://127.0.0.1:4318/oauth/authorize |
  awk 'tolower($1)=="location:" {print $2}' | tr -d '\r' |
  python3 -c 'import sys,urllib.parse; print(dict(urllib.parse.parse_qsl(urllib.parse.urlparse(sys.stdin.read()).query))["code"])')"
[ -n "$CODE" ] || die "authorization code not issued"
OAUTH_TOKEN="$(curl -fsS -H 'Content-Type: application/x-www-form-urlencoded' \
  --data-urlencode "grant_type=authorization_code" --data-urlencode "code=$CODE" \
  --data-urlencode "redirect_uri=http://127.0.0.1:9/callback" \
  --data-urlencode "client_id=$CLIENT" --data-urlencode "code_verifier=$VERIFIER" \
  http://127.0.0.1:4318/oauth/token | jsonfield '["access_token"]')"
[ -n "$OAUTH_TOKEN" ] || die "token exchange failed"
OSID="$(session "$OAUTH_TOKEN")"
[ -n "$OSID" ] || die "initialize with the OAuth token failed"
OKOID="$(tool "$OAUTH_TOKEN" "$OSID" "remember" '{"subject":"agent","type_name":"note","properties":{"n":8}}' | jsonfield '["koid"]')"
[ -n "$OKOID" ] || die "OAuth remember on the leader failed"
echo "OAuth sign-in issued and served a write ($OKOID)"

# --- failover: kill the leader pod -----------------------------------------
kubectl -n "$NS" delete pod "$LEADER" --grace-period=1 >/dev/null
NEW_LEADER=""
for _ in $(seq 90); do
  NEW_LEADER="$(leader)"
  [ -n "$NEW_LEADER" ] && [ "$NEW_LEADER" != "$LEADER" ] && break
  sleep 2
done
[ -n "$NEW_LEADER" ] && [ "$NEW_LEADER" != "$LEADER" ] || die "no other pod won the lease"
kubectl -n "$NS" wait --for=condition=Ready pod \
  -l app.kubernetes.io/name=aikoql --timeout=300s
echo "standby $NEW_LEADER won the lease"

# --- the pre-failover writes and sign-in survive ---------------------------
pf "$NEW_LEADER"
GOT="$(tool "$TOKEN" "$SID" "get" "{\"subject\":\"agent\",\"koid\":\"$KOID\"}" | jsonfield '["properties"]["n"]')"
[ "$GOT" = "7" ] || die "leader write lost after failover (got $GOT)"
GOT8="$(tool "$TOKEN" "$SID" "get" "{\"subject\":\"agent\",\"koid\":\"$OKOID\"}" | jsonfield '["properties"]["n"]')"
[ "$GOT8" = "8" ] || die "OAuth write lost after failover (got $GOT8)"
# the OAuth sign-in from before the kill still authorizes the new leader
NEW_KOID="$(tool "$OAUTH_TOKEN" "$OSID" "remember" '{"subject":"agent","type_name":"note","properties":{"n":9}}' | jsonfield '["koid"]')"
[ -n "$NEW_KOID" ] || die "the pre-failover OAuth token no longer works"
echo "PASS: failover served both writes; the OAuth sign-in survived ($NEW_KOID)"
