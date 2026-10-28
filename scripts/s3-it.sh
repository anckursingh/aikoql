#!/usr/bin/env bash
# MRFC-0080 — the real-S3 legs (HA lease, continuous archive, PITR,
# retention) against S3-compatible servers. The `real_s3` tests skip
# themselves unless AIKOQL_TEST_S3_ENDPOINT/_BUCKET and AWS_* are set; this
# script sets them.
#
#   scripts/s3-it.sh                    a throwaway MinIO (`minio` + `mc` on
#                                       PATH: brew install minio minio-mc)
#   S3_IT_BACKEND=moto scripts/s3-it.sh a throwaway Moto server (`moto_server`
#                                       on PATH: pip install "moto[server]")
#   S3_IT_BACKEND="minio moto" …        both, one after the other
#   S3_IT_BACKEND=aws AIKOQL_TEST_S3_BUCKET=<throwaway bucket> \
#     AWS_ACCESS_KEY_ID=… AWS_SECRET_ACCESS_KEY=… [AWS_SESSION_TOKEN=…] \
#     AWS_REGION=<the bucket's region> scripts/s3-it.sh
#                                       AWS itself, no endpoint: the
#                                       virtual-hosted URL path. Use a
#                                       throwaway bucket and short-lived,
#                                       bucket-scoped credentials — the certs
#                                       leave small objects under it-*
#   AIKOQL_TEST_S3_ENDPOINT=http://host:9000 scripts/s3-it.sh
#                                       a running store (CI: a MinIO
#                                       container); its bucket must exist or
#                                       `mc` must be on PATH
#
# Moto and MinIO are independent implementations of the S3 API: both must
# enforce If-None-Match / If-Match (the lease cert fails if one ignores
# them). Neither is AWS. Credentials default to throwaway test values.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
# AWS must never see the throwaway defaults below: remember what was given
given_creds="${AWS_ACCESS_KEY_ID:+yes}"
given_bucket="${AIKOQL_TEST_S3_BUCKET:+yes}"
export AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID:-aikoqltest}"
export AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY:-aikoqltest-secret}"
export AIKOQL_TEST_S3_BUCKET="${AIKOQL_TEST_S3_BUCKET:-aikoql-it}"

pid=""
data=""
stop() {
  [ -n "$pid" ] && kill "$pid" 2>/dev/null && wait "$pid" 2>/dev/null || true
  [ -n "$data" ] && rm -rf "$data"
  pid=""
  data=""
}
trap stop EXIT

free_port() { python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }
wait_up() { for _ in $(seq 1 75); do curl -sf "$1" >/dev/null 2>&1 && return 0; sleep 0.2; done; echo "s3-it: $1 never came up" >&2; exit 1; }

run_tests() {
  cd "$root"
  cargo build -p aikoql-mcp
  cargo test -p aikoql-mcp --bin aikoql-mcp real_s3 -- --nocapture
  cargo test -p aikoql-mcp --test replication_failover real_s3 -- --nocapture
  echo "s3-it: OK against $1 ($AIKOQL_TEST_S3_ENDPOINT)"
}

if [ -n "${AIKOQL_TEST_S3_ENDPOINT:-}" ]; then
  if command -v mc >/dev/null; then
    mc alias set s3it "$AIKOQL_TEST_S3_ENDPOINT" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null
    mc mb --ignore-existing "s3it/$AIKOQL_TEST_S3_BUCKET" >/dev/null
  fi
  run_tests "the given store"
  exit 0
fi

for backend in ${S3_IT_BACKEND:-minio}; do
  port="$(free_port)"
  export AIKOQL_TEST_S3_ENDPOINT="http://127.0.0.1:$port"
  case "$backend" in
    minio)
      command -v minio >/dev/null && command -v mc >/dev/null ||
        { echo "s3-it: minio/mc not on PATH (brew install minio minio-mc)" >&2; exit 2; }
      data="$(mktemp -d)"
      MINIO_ROOT_USER="$AWS_ACCESS_KEY_ID" MINIO_ROOT_PASSWORD="$AWS_SECRET_ACCESS_KEY" \
        minio server --quiet --address "127.0.0.1:$port" --console-address "127.0.0.1:0" "$data" >/dev/null 2>&1 &
      pid=$!
      wait_up "$AIKOQL_TEST_S3_ENDPOINT/minio/health/ready"
      mc alias set s3it "$AIKOQL_TEST_S3_ENDPOINT" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null
      mc mb --ignore-existing "s3it/$AIKOQL_TEST_S3_BUCKET" >/dev/null
      ;;
    moto)
      command -v moto_server >/dev/null ||
        { echo "s3-it: moto_server not on PATH (pip install \"moto[server]\")" >&2; exit 2; }
      moto_server -H 127.0.0.1 -p "$port" >/dev/null 2>&1 &
      pid=$!
      wait_up "$AIKOQL_TEST_S3_ENDPOINT/moto-api/"
      # Moto accepts an unsigned CreateBucket
      curl -sf -X PUT "$AIKOQL_TEST_S3_ENDPOINT/$AIKOQL_TEST_S3_BUCKET" >/dev/null
      ;;
    aws)
      [ -n "$given_creds" ] && [ -n "$given_bucket" ] && [ -n "${AWS_REGION:-${AWS_DEFAULT_REGION:-}}" ] ||
        { echo "s3-it: aws needs AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, AIKOQL_TEST_S3_BUCKET and AWS_REGION set explicitly" >&2; exit 2; }
      unset AIKOQL_TEST_S3_ENDPOINT
      export AIKOQL_TEST_S3_AWS=1
      echo "s3-it: AWS (virtual-hosted) — bucket $AIKOQL_TEST_S3_BUCKET in ${AWS_REGION:-$AWS_DEFAULT_REGION}; objects stay under it-* (use a throwaway bucket)"
      run_tests aws
      unset AIKOQL_TEST_S3_AWS
      continue
      ;;
    *) echo "s3-it: unknown backend $backend (minio | moto | aws)" >&2; exit 2 ;;
  esac
  run_tests "$backend"
  stop
  unset AIKOQL_TEST_S3_ENDPOINT
done
