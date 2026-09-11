# AikoQL as a First-Class Database — Roadmap

Requested 2026-09-11 (P3-M9): what it would take for aikoql to become a
primary database — the thing an app stores its core data in — in the way
Postgres, Neo4j, and MongoDB are. This is a plan, not a commitment: every
phase starts on evidence, never speculatively.

## Where we are today (v0.1.19, 2026-09)

| Layer | State |
|---|---|
| Storage | aikoql-v2 segmented LSM — tiered compaction, background compaction + backpressure (P3-M8), snapshot/restore (P3-M3), crash-window certifications (SE2-M33–41) |
| Kernel | Graph + vector + text in one; MVCC/HLC; bitemporal; epistemic status; constraints (P3-M5); knowledge transactions; provenance |
| Query | aikoql (SQL-like) — lexer → parser → semantic → KIR → planner → runtime |
| Surface | MCP (stdio/TCP) + REST + Studio; token auth, tenancy, rate limits (P3-M1) |
| Positioning | Knowledge layer *beside* existing DBs — the providers import *from* Postgres/SQLite/Mongo/Neo4j |
| Drivers | Python only (P3-M9 deleted the unversioned Go/TS/Java stubs) |

The current architecture is embedded-first and single-node. Nothing in the
repo builds toward primary-DB use — the cluster proxy (the one sharding
artifact) was deleted in P3-M9 with zero demand evidence.

## What "first-class DB" requires (the gap, honestly)

1. **Performance envelope.** Primary stores compete on the hot path. The
   gate-5 history shows aikoql-v2 at ~6–8× slower than v1/redb in some
   workload cells (SE2-M19/M22 evidence). Incumbents are the benchmark, not
   "good enough for knowledge tasks."
2. **Replication + HA.** None today — a single embedded instance. Failover,
   leader election, and rolling upgrades are table stakes for any DB a
   product depends on.
3. **Multi-tenancy at scale.** Beginnings exist (P3-M1 TCP tokens, tenant
   isolation), but single-process only.
4. **Ops tooling.** Backup/restore exist. Missing: fleet monitoring,
   rebalancing, rolling upgrades, capacity tooling.
5. **Driver program.** First-party drivers per language, rebuilt against the
   current contract with CI + contract tests + versioned releases (see the
   re-adoption trigger below).
6. **Wire protocol.** MCP JSON-RPC is fine for agents; a primary DB needs a
   binary protocol for hot paths (cf. PG wire protocol, Bolt).
7. **Ecosystem.** ORMs, connection pools, migration tooling, and — for
   incumbent-level adoption — a hosted offering.

## Phases (each evidence-gated)

- **Phase 0 — Knowledge layer (now).** Embedded or single-node `serve`;
  MCP as the universal client surface; Python SDK first-party.
- **Phase 1 — Performance parity.** Close the gap to ≤2× on representative
  OLTP + graph workloads vs Postgres/Neo4j, using the existing gate-5
  machinery. *Gate: the benchmark matrix passes at ≤2×.*
- **Phase 2 — Single-node operational maturity.** Replication (log
  shipping or Raft), failover, rolling upgrades, ops tooling. *Gate:
  crash-window certs on a 2-node setup.*
- **Phase 3 — Multi-node.** Sharding — rebuild the cluster proxy properly
  (route by tenant/shard key, retry/backoff, rebalancing). *Gate:
  federation benchmark + failure injection.*
- **Phase 4 — Driver program.** First-party Go/TS/Java drivers (rebuilt
  from git history, current contract, CI + contract tests + versioned
  releases) + binary wire protocol + ORM support. *Gate: three external
  consumers or an explicit product commitment.*
- **Phase 5 — Primary-DB positioning.** Migration tooling from
  Postgres/Neo4j, hosted offering, ecosystem marketing.

## Triggers

- **Start Phase 1** when: a real workload wants aikoql as system of record,
  or this doc is promoted to an official roadmap item.
- **Re-adopt drivers (Phase 4)** when: the P3-M9 deletion triggers fire —
  see `docs/sdk-proxy-decision.md` §Re-adopt triggers. The deleted code is
  recoverable from git history and must be rebuilt against the then-current
  MCP contract anyway.
- **Do nothing** while: aikoql's value is the agent knowledge layer — in
  that world, MCP + one well-maintained SDK is the right-sized investment.
