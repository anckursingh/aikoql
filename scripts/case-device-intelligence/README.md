# Device-intelligence case study (illustrative)

Seeds ClickHouse and Postgres with a made-up device-intelligence dataset and walks a customer's
questions through the same REST routes Studio uses. Findings: `docs/ONTOLOGY-IN-PRACTICE.md`.

    docker compose --profile full --profile olap up -d --no-deps postgres clickhouse   # TCP_TOKEN=x if compose asks
    docker exec -i <proj>-clickhouse-1 clickhouse-client --password aikoql-dev-only --multiquery < clickhouse.sql
    docker exec <proj>-postgres-1 psql -U aikoql -d knowledge -c "CREATE DATABASE crm"
    docker exec -i <proj>-postgres-1 psql -U aikoql -d crm < postgres.sql
    # a server with [[sources]] device-intel (clickhouse, database device_intel) and crm (postgres), then:
    python3 walk.py <ok-file>            # discover, preview, publish, import
    python3 questions.py .               # the four questions (edit BASE in walk.py for your port)
