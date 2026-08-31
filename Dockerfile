# Aikoql MVP — multi-stage Docker build
# PRR-1: redb is the default storage backend — no rocksdb (the
# `storage-rocksdb` feature never existed; see MVP-001).
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

RUN mkdir -p /data
VOLUME /data

EXPOSE 9090 9091

# PRR-1a: no `aikoql health` subcommand exists — probe the HTTP /health
# endpoint on the metrics port instead.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:9091/health || exit 1

ENTRYPOINT ["aikoql"]
# PRR-2 + PRR-4: TCP requires >=1 token; the config pipeline reads
# AIKOQL_TCP_TOKEN (TOKEN[:TENANT[:ROLES]]) directly, so no shell expansion
# is needed (an exec-form CMD with ${...} is NOT expanded by Docker, and a
# sh -c CMD here would run `aikoql sh -c ...` — double invocation).
# Unset AIKOQL_TCP_TOKEN -> no tokens -> TCP listener refuses (fail-closed).
# R1/R3 (review round 3): loopback-only plaintext TCP — the server rejects
# non-loopback binds fail-closed (see validate_listen), so the container
# binds 127.0.0.1. Remote access is post-MVP: terminate TLS at a sidecar
# proxy that shares the container's network namespace (the loopback bind is
# reachable there), or use the stdio/npm path.
# Container contract: everything mutable lives under the /data volume —
# redb file, memory dir, and the local embedding model store (PRR-3 installs
# there via `docker exec aikoql aikoql model install`; the image itself stays
# stateless, no model baked in). CLI args win over aikoql.toml, so the paths
# below are authoritative in the container.
CMD ["serve", "/data/aikoql.redb", "--listen", "127.0.0.1:9090", "--metrics-addr", "127.0.0.1:9091", "--model-dir", "/data/models"]
