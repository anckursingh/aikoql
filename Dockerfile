# Mnemosyne MVP — multi-stage Docker build
# Stage 1: build
FROM rust:1.80-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    clang libclang-dev librocksdb-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY benchmarks/ benchmarks/

RUN cargo build -p mnemosyne-mcp --features storage-rocksdb --release \
    && strip target/release/mnemosyne-mcp

# Stage 2: runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates librocksdb9.1 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/mnemosyne-mcp /usr/local/bin/mnemosyne
COPY mnemosyne.toml /etc/mnemosyne/mnemosyne.toml

RUN mkdir -p /data
VOLUME /data

EXPOSE 9090 9091

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD mnemosyne health 2>/dev/null || exit 1

ENTRYPOINT ["mnemosyne"]
CMD ["serve", "/data/kb", "--listen", "0.0.0.0:9090", "--metrics-addr", "0.0.0.0:9091"]
