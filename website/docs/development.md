---
title: Development
description: Build, test and extend aikoql from source
---

# Development Guide

## Prerequisites

- **Rust** — stable toolchain. On Windows: MSVC (Build Tools) — the storage
  engine intentionally avoids asm-shim dependencies that fail on MSVC.
- **Node.js** — only for npm packaging of the MCP plugin.
- **Python 3.9+** — only for the PyO3 SDK (`crates/sdk/python`).

No database server, no service dependencies. Everything is embedded.

## Clone & Build

```bash
git clone https://github.com/anckursingh/aikoql.git
cd aikoql
cargo build --release
# binary: target/release/aikoql-mcp(.exe)
```

## Test

```bash
cargo test --workspace        # full suite (245+ tests across ~60 binaries)
cargo fmt --all --check       # formatting guardrail
cargo clippy --workspace -- -D warnings   # lint guardrail
```

Guardrails are the definition of done: no milestone is complete without a
green fmt + clippy + suite run.

## Run from Source

```bash
cargo run -p aikoql-mcp -- serve ./kb        # fresh path = aikoql-v2 directory
cargo run -p aikoql-mcp -- shell :memory:
```

Storage is v2-only: `aikoql-v2` is the sole engine (redb/v1 were
decommissioned, S-02) and there is no backend knob. The adoption ADR lives
in `docs/STORAGE-ENGINE-ARCHITECTURE-DECISION.md`.

## Project Layout

```
crates/
├── kernel/           Knowledge Kernel (MVCC, OCC, HLC, RBAC, audit, constraints)
├── storage/          aikoql-v2 — the only engine (segmented LSM)
├── compiler/         aikoql parser, semantic analyzer, planner
├── runtime/          Physical plan interpreter
├── engines/          graph, vector, scheduler, reasoning, semantic
├── ingestion/        Document pipeline (D1-D9) + knowledge compilers (A1-A9)
├── providers/        postgres, sqlite, mongodb, neo4j, sdk
├── services/         api/mcp (MCP + REST + Studio)
├── protocol/         native (the §6 framed wire)
├── certification/    certification harness
├── sdk/              python, rust, fault-proxy (+ TypeScript, Go, Java SDKs)
```

See [Architecture](/docs/architecture) for the full crate map and the
[Benchmarks](/docs/benchmarks) for the certified numbers.

## Engineering Conventions

- **TDD per milestone.** Storage-engine work ships as SE2-M0…M40 milestones,
  each with its own test suite (`crates/storage/aikoql-v2/tests/`) — every
  capability is pinned by a test before the code exists (RED → GREEN).
- **Evidence packs.** Measured results land in `artifacts/` (workload
  matrices, checkpoint probes, crash-window certs); the certification history
  lives in `docs/TESTING-PLAN-V2.md` and
  `docs/testing/STORAGE_ENGINE_MVP_CERTIFICATION_CLOSURE.md`.
- **Architecture decisions** are recorded as ADRs in `docs/` (see
  `STORAGE-ENGINE-ARCHITECTURE-DECISION.md`).
- Commits land on feature branches, one commit per milestone; the maintainer
  pushes and tags releases.
