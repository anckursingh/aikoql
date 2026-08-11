# Aikoql MVP — multi-stage Docker build
# Stage 1: build
FROM rust:1.80-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    clang libclang-dev librocksdb-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY benchmarks/ benchmarks/

RUN cargo build -p aikoql-mcp --features storage-rocksdb --release \
    && strip target/release/aikoql-mcp

# Stage 2: runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates librocksdb9.1 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/aikoql-mcp /usr/local/bin/aikoql
COPY aikoql.toml /etc/aikoql/aikoql.toml

RUN mkdir -p /data
VOLUME /data

EXPOSE 9090 9091

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD aikoql health 2>/dev/null || exit 1

ENTRYPOINT ["aikoql"]
CMD ["serve", "/data/kb", "--listen", "0.0.0.0:9090", "--metrics-addr", "0.0.0.0:9091"]
