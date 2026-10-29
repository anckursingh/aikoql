# Aikoql MVP — multi-stage Docker build
# Storage is aikoql-v2, the only engine since launch S-02 (no redb, no
# rocksdb).
# Stage 1: build (pin matches the repo MSRV — rust 1.80 cannot parse
# edition-2024 registry crates, e.g. crypto-common 0.2.2).
FROM rust:1.97-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY benchmarks/ benchmarks/

RUN cargo build -p aikoql-mcp --release \
    && strip target/release/aikoql-mcp

# Stage 2: runtime
FROM debian:bookworm-slim

# curl is for the HEALTHCHECK below.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/aikoql-mcp /usr/local/bin/aikoql
COPY aikoql.toml /etc/aikoql/aikoql.toml
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

RUN mkdir -p /data
VOLUME /data
# agent memory (MRFC-0070) is mutable state: in the volume, not ./memory
# (= /memory, lost on recreate); --memory-dir still wins
ENV AIKOQL_MEMORY_DIR=/data/memory

EXPOSE 9090 9091

# PRR-1a: no `aikoql health` subcommand exists — probe the HTTP /health
# endpoint on the metrics port instead: HTTPS when TLS is configured (-k:
# the probe is loopback inside the container, the certificate names the
# published host), plain HTTP otherwise.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -fsSk https://127.0.0.1:9091/health || curl -fsS http://127.0.0.1:9091/health || exit 1

# `aikoql "$@"` after the one-time /data/aikoql.redb → /data/aikoql move
# (docker-entrypoint.sh); `--entrypoint aikoql` still runs the binary bare.
ENTRYPOINT ["docker-entrypoint.sh"]
# PRR-2 + PRR-4: TCP requires >=1 token; the config pipeline reads
# AIKOQL_TCP_TOKEN (TOKEN[:TENANT[:ROLES]]) directly, so no shell expansion
# is needed (an exec-form CMD with ${...} is NOT expanded by Docker, and a
# sh -c CMD here would run `aikoql sh -c ...` — double invocation).
# Unset AIKOQL_TCP_TOKEN -> no tokens -> TCP listener refuses (fail-closed).
# R1/R3 (review round 3): plaintext TCP is loopback-only — the server
# rejects a non-loopback plaintext bind fail-closed (validate_listen), so by
# default the container binds 127.0.0.1. Mount a certificate and set
# AIKOQL_TLS_CERT / AIKOQL_TLS_KEY and the entrypoint opens 9090 on every
# interface (TLS) — `-p 9090:9090` then works; 9091 (HTTPS) also needs
# AIKOQL_ALLOW_REMOTE_HTTP=1 and a login (AIKOQL_ADMIN_PASSWORD or [auth]).
# Container contract: everything mutable lives under the /data volume —
# the aikoql-v2 database directory, memory dir, and the local embedding model store (PRR-3 installs
# there via `docker exec aikoql aikoql model install`; the image itself stays
# stateless, no model baked in). CLI args win over aikoql.toml, so the paths
# below are authoritative in the container.
CMD ["serve", "/data/aikoql", "--listen", "127.0.0.1:9090", "--metrics-addr", "127.0.0.1:9091", "--model-dir", "/data/models"]
