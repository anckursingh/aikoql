#!/usr/bin/env bash
# CI-07 — the competitor harness's five containers, one command. The names
# are what bench.py's docker_rss/docker_du probe (bench-pg/bench-neo4j/
# bench-qdrant/bench-mongo/bench-ch); the images are the composed stacks the
# report method pins. PG is the pgvector image (postgres 16 + the extension —
# the knowledge_query cell's vector leg). CI-08 (§18): explicit version tags
# only — bench.py records the pulled digest into result.json.
#
# Usage: bash scripts/competitor_bench/containers.sh start|stop
set -euo pipefail

case "${1:-}" in
  start)
    docker rm -f bench-pg bench-neo4j bench-qdrant bench-mongo bench-ch 2>/dev/null || true
    docker run -d --name bench-pg -p 5433:5432 \
      -e POSTGRES_USER=bench -e POSTGRES_PASSWORD=bench -e POSTGRES_DB=bench \
      pgvector/pgvector:pg16
    docker run -d --name bench-neo4j -p 7474:7474 -p 7687:7687 \
      -e NEO4J_AUTH=neo4j/benchmarkpass neo4j:5-community
    docker run -d --name bench-qdrant -p 6333:6333 qdrant/qdrant:v1.19.1
    docker run -d --name bench-mongo -p 27017:27017 mongo:7
    docker run -d --name bench-ch -p 8123:8123 -p 9000:9000 \
      -e CLICKHOUSE_USER=bench -e CLICKHOUSE_PASSWORD=bench \
      clickhouse/clickhouse-server:24.8
    echo "started bench-pg (pgvector/pgvector:pg16 @5433), bench-neo4j (7687), bench-qdrant (6333), bench-mongo (27017), bench-ch (8123)"
    echo "give neo4j ~15s to boot, then: .venv/bin/python scripts/competitor_bench/bench.py"
    ;;
  stop)
    docker rm -f bench-pg bench-neo4j bench-qdrant bench-mongo bench-ch
    ;;
  *)
    echo "usage: containers.sh start|stop" >&2
    exit 2
    ;;
esac
