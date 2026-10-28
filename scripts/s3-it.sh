#!/usr/bin/env bash
# MRFC-0080 — the real-S3 legs (HA lease, continuous archive, PITR)
# against MinIO, an S3-compatible server. The `real_s3` tests skip
# themselves unless AIKOQL_TEST_S3_ENDPOINT/_BUCKET and AWS_* are set; this
# script sets them.
#
#   scripts/s3-it.sh            start a throwaway MinIO (needs `minio` +
#                               `mc` on PATH, e.g. `brew install minio
#                               minio-mc`), run the tests, stop it
#   AIKOQL_TEST_S3_ENDPOINT=http://host:9000 scripts/s3-it.sh
#                               use a running store (CI: a MinIO container);
#                               the bucket is created if missing
#
# Credentials default to throwaway test values (override with AWS_*).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
export AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID:-aikoqltest}"
export AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY:-aikoqltest-secret}"
export AIKOQL_TEST_S3_BUCKET="${AIKOQL_TEST_S3_BUCKET:-aikoql-it}"

pid=""
data=""
cleanup() {
  [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  [ -n "$data" ] && rm -rf "$data"
}
trap cleanup EXIT

if [ -z "${AIKOQL_TEST_S3_ENDPOINT:-}" ]; then
  command -v minio >/dev/null || { echo "s3-it: minio not on PATH (brew install minio minio-mc)" >&2; exit 2; }
  data="$(mktemp -d)"
  port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
  MINIO_ROOT_USER="$AWS_ACCESS_KEY_ID" MINIO_ROOT_PASSWORD="$AWS_SECRET_ACCESS_KEY" \
    minio server --quiet --address "127.0.0.1:$port" --console-address "127.0.0.1:0" "$data" >/dev/null 2>&1 &
  pid=$!
  export AIKOQL_TEST_S3_ENDPOINT="http://127.0.0.1:$port"
  for _ in $(seq 1 50); do
    curl -sf "$AIKOQL_TEST_S3_ENDPOINT/minio/health/ready" >/dev/null && break
    sleep 0.2
  done
fi

# the bucket: created here when `mc` is around (CI creates it beforehand)
if command -v mc >/dev/null; then
  mc alias set s3it "$AIKOQL_TEST_S3_ENDPOINT" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null
  mc mb --ignore-existing "s3it/$AIKOQL_TEST_S3_BUCKET" >/dev/null
elif [ -n "$pid" ]; then
  echo "s3-it: mc not on PATH (brew install minio-mc)" >&2
  exit 2
fi

cd "$root"
cargo build -p aikoql-mcp
cargo test -p aikoql-mcp --bin aikoql-mcp real_s3 -- --nocapture
cargo test -p aikoql-mcp --test replication_failover real_s3 -- --nocapture
echo "s3-it: OK against $AIKOQL_TEST_S3_ENDPOINT"
