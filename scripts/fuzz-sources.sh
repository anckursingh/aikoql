#!/usr/bin/env bash
# Seeded fuzz / adversarial suites for source-schema discovery at high volume.
# Every suite uses fixed seeds; a failure prints its seed. Not part of the fast
# gates: run it before releases or after touching crates/providers, crates/ingestion
# (connector_bridge, ontology_draft, source_schema) or crates/services/api/mcp/src/sources.
#
#   bash scripts/fuzz-sources.sh [iterations]      # default 50000
set -euo pipefail
cd "$(cd "$(dirname "$0")/.." && pwd)"
export AIKOQL_FUZZ_ITERS="${1:-50000}"
echo "== source discovery fuzz, AIKOQL_FUZZ_ITERS=$AIKOQL_FUZZ_ITERS"
cargo test -p aikoql-ingestion --release --test source_fuzz
cargo test -p aikoql-mongodb -p aikoql-neo4j -p aikoql-sqlite -p aikoql-clickhouse
cargo test -p aikoql-mcp --bin aikoql-mcp -- sources
cargo test -p aikoql-mcp --test source_hardening --test source_discovery
echo "== done (live Postgres/Mongo/Neo4j/ClickHouse cells run when AIKOQL_TEST_* are set)"
