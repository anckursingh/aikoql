#!/usr/bin/env bash
# MRFC-0080 — the Kubernetes lease (k8s://NAMESPACE/NAME) against a real
# kube-apiserver: envtest's apiserver + etcd binaries run natively on
# loopback — no cluster, no Docker, no VM. The `real_k8s` tests skip
# themselves unless AIKOQL_TEST_K8S_API/_TOKEN/_CA are set; this script
# sets them.
#
#   scripts/k8s-it.sh        needs `setup-envtest` (go install
#                            sigs.k8s.io/controller-runtime/tools/setup-envtest@latest)
#                            or KUBEBUILDER_ASSETS pointing at the binaries
#
# Throwaway credentials: a static bearer token in system:masters, a CA and
# serving certificate for 127.0.0.1, all in a temp dir removed on exit.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
pids=()
stop() {
  # the apiserver drains slowly on TERM: give each process 5 s, then KILL
  for p in "${pids[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  for p in "${pids[@]:-}"; do
    [ -n "$p" ] || continue
    for _ in $(seq 1 25); do kill -0 "$p" 2>/dev/null || break; sleep 0.2; done
    kill -9 "$p" 2>/dev/null || true
  done
  rm -rf "$work"
}
trap stop EXIT

if [ -z "${KUBEBUILDER_ASSETS:-}" ]; then
  command -v setup-envtest >/dev/null ||
    { echo "k8s-it: setup-envtest not on PATH (go install sigs.k8s.io/controller-runtime/tools/setup-envtest@latest)" >&2; exit 2; }
  KUBEBUILDER_ASSETS="$(setup-envtest use -p path --bin-dir "${ENVTEST_BIN_DIR:-$HOME/.cache/aikoql-envtest}")"
fi
bin="$KUBEBUILDER_ASSETS"
free_port() { python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }

# --- credentials ---
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 \
  -keyout "$work/ca.key" -out "$work/ca.pem" -subj "/CN=aikoql k8s-it CA" \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign" 2>/dev/null
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout "$work/apiserver.key" -out "$work/apiserver.csr" -subj "/CN=kube-apiserver" 2>/dev/null
printf 'subjectAltName=IP:127.0.0.1,DNS:localhost\nextendedKeyUsage=serverAuth\n' > "$work/ext"
openssl x509 -req -in "$work/apiserver.csr" -CA "$work/ca.pem" -CAkey "$work/ca.key" -CAcreateserial \
  -days 1 -out "$work/apiserver.pem" -extfile "$work/ext" 2>/dev/null
openssl genrsa -out "$work/sa.key" 2048 2>/dev/null
openssl rsa -in "$work/sa.key" -pubout -out "$work/sa.pub" 2>/dev/null
token="aikoql-k8s-it-$(date +%s)"
printf '%s,aikoql-it,1,"system:masters"\n' "$token" > "$work/tokens.csv"

# --- etcd + kube-apiserver ---
etcd_port="$(free_port)"; peer_port="$(free_port)"; api_port="$(free_port)"
"$bin/etcd" --data-dir "$work/etcd" \
  --listen-client-urls "http://127.0.0.1:$etcd_port" --advertise-client-urls "http://127.0.0.1:$etcd_port" \
  --listen-peer-urls "http://127.0.0.1:$peer_port" --initial-advertise-peer-urls "http://127.0.0.1:$peer_port" \
  --initial-cluster "default=http://127.0.0.1:$peer_port" > "$work/etcd.log" 2>&1 &
pids+=($!)
"$bin/kube-apiserver" --etcd-servers "http://127.0.0.1:$etcd_port" \
  --bind-address 127.0.0.1 --advertise-address 127.0.0.1 --secure-port "$api_port" \
  --tls-cert-file "$work/apiserver.pem" --tls-private-key-file "$work/apiserver.key" \
  --token-auth-file "$work/tokens.csv" --authorization-mode RBAC \
  --service-account-issuer https://kubernetes.default.svc \
  --service-account-key-file "$work/sa.pub" --service-account-signing-key-file "$work/sa.key" \
  --service-cluster-ip-range 10.0.0.0/24 --cert-dir "$work/certs" \
  --endpoint-reconciler-type none > "$work/apiserver.log" 2>&1 &
pids+=($!)
api="https://127.0.0.1:$api_port"
for _ in $(seq 1 150); do
  curl -sf --cacert "$work/ca.pem" -H "Authorization: Bearer $token" "$api/readyz" >/dev/null 2>&1 && break
  sleep 0.2
done
curl -sf --cacert "$work/ca.pem" -H "Authorization: Bearer $token" "$api/readyz" >/dev/null ||
  { echo "k8s-it: kube-apiserver never became ready" >&2; tail -20 "$work/apiserver.log" >&2; exit 1; }
# the "default" namespace is created by the apiserver's bootstrap controller
for _ in $(seq 1 50); do
  curl -sf --cacert "$work/ca.pem" -H "Authorization: Bearer $token" "$api/api/v1/namespaces/default" >/dev/null 2>&1 && break
  sleep 0.2
done

export AIKOQL_TEST_K8S_API="$api" AIKOQL_TEST_K8S_TOKEN="$token" AIKOQL_TEST_K8S_CA="$work/ca.pem"
cd "$root"
cargo build -p aikoql-mcp
cargo test -p aikoql-mcp --bin aikoql-mcp real_k8s -- --nocapture
cargo test -p aikoql-mcp --test replication_failover real_k8s -- --nocapture
echo "k8s-it: OK against kube-apiserver $("$bin/kube-apiserver" --version 2>/dev/null | awk '{print $2}') at $api"
