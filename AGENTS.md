# AGENTS.md — working on aikoql

Read this first when starting work on this repository. The canonical
documents live in `docs/` — this file is an index with a
current-vs-historical status, not a second copy. Agent-facing product docs
(using aikoql as an MCP server) are `AGENTIC-QUICKSTART.md` /
`QUICKSTART.md`; this file is for developing the system itself.

## What this is

AikoQL is an agent-first knowledge database: one kernel combining graph,
vector, and text storage with epistemic machinery (authority-graded
assertions, KOID version lineage, contradiction → Conflict → resolution,
hash-chained audit, `AS_OF`/`HISTORICAL`/`EPISTEMIC` queries) on the
aikoql-v2 LSM storage engine, with an aikoql query language, an MCP server,
and a Python SDK. The competitive position is measured, not claimed — see
the architect review.

## Start here (current docs)

| doc | what it is |
|---|---|
| `docs/IMPLEMENTATION-PLAN-LAUNCH.md` | **the active plan** (branch `feature/aikoql-db-launch`) — launch factors + phases S (storage-V2-only consolidation), CI (three-workflow redesign), L (TDD-001..034 correctness matrices) |
| `docs/TESTING-PLAN-LAUNCH.md` | the active test plan — RED→GREEN mechanics, standing rules, benchmark schema |
| `docs/PR6-TDD-DISPOSITIONS.md` | the review-disposition state machine (R3-001..005, R2-008, follow-up milestones); CI gates pin its freshness |
| `docs/ARCHITECT-REVIEW-2026-09.md` | project-level assessment vs the agentic-DB field (2026-09-20) |
| `docs/VISION-AND-STRATEGY.md` | goals and strategy |
| `docs/first-class-db-roadmap.md` | the honest primary-DB roadmap, phases 0–5, evidence-gated triggers |
| `docs/MRFC-0005-System-Architecture.md` | system architecture |
| `docs/MRFC-0001-Knowledge-Object-Model.md` | the knowledge object model |
| `docs/MRFC-0011-Knowledge-Syscall-ABI.md` | syscall ABI |
| `docs/AIKOQL_Storage_Engine_V2_Production_Design.md` | the v2 storage engine design (the only engine, per the launch plan phase S) |
| `docs/STORAGE-BACKENDS.md`, `docs/STORAGE-ENGINE-ARCHITECTURE-DECISION.md` | backend history — **being superseded** by the storage-V2-only decision; treat as historical once phase S lands |
| `docs/server-contract.md`, `docs/transaction-contract.md`, `docs/version-compatibility.md` | wire/transaction/version contracts |
| `docs/MRFC-0020-Encryption-Key-Management-Architecture.md`, `docs/checksum-threat-model.md` | security: key management, checksum guarantees and non-guarantees |
| `docs/MRFC-0060-Constraint-Engine-HLD-LLD.md`, `docs/MRFC-0070-Agent-Knowledge-Interface-and-Engineering-Knowledge-Compiler.md` | constraint engine; knowledge compiler |
| `docs/sdk-proxy-decision.md` | why the Go/TS/Java SDKs and cluster proxy were deleted (re-adopt triggers included) |
| `docs/IMPLEMENTATION-PLAN-PHASE5.md`, `docs/TESTING-PLAN-PHASE5.md` | the M0–M47 campaign record (shipped; ledger rows and traps) |

## Repo conventions (agents must follow)

- **TDD**: RED first (real failing evidence — an assert or a measured
  counter), then GREEN, then docs. One milestone = one commit set
  (test(RED) → feat → docs).
- **The re-stamp ritual**: `scripts/check-disposition-head.sh` fails any
  commit that moves the branch tip without re-stamping
  `docs/PR6-TDD-DISPOSITIONS.md` (R3-005) and its dogfood-compiled section
  (F8). ANY new commit — including CI fixes — must be followed by:
  `python scripts/dogfood-review-loop.py full`, then commit the re-emitted
  doc AS THE TIP. The tip commit is always the re-stamp.
- **Gates** (run before declaring anything done): the `scripts/check-*.sh`
  chain (node-modules, estate hygiene, red archives, skip drift, test-env
  hygiene, shuffle wiring, disposition head) + `cargo fmt --check` +
  `cargo clippy --workspace -- -D warnings`.
- **Test hygiene**: no unseeded RNG or bare `set_var` in test code
  (`scripts/check-test-env-hygiene.sh`); env-gated cells register in
  `tests/gated.toml` with a reason and an `ungated_by` home.
- **Benchmarks**: laptop runs only the quick cells; 1M-scale rides CI.
  No performance claim without a structural metric (allocs/bytes/decodes)
  or a reproducible benchmark; no absolute timing pins on shared runners.
- **Rust guidance**: `.claude/skills/rust-coding/SKILL.md` (invoked as
  `/rust-coding` in Claude Code) — error classification, optimization
  discipline, scalability patterns, the verified trap ledger.
- **Pushing**: commits are made locally; the user pushes.
- **Residue**: commit-message files and smoke sidecars are deleted after
  the commit lands; `scripts/clean-residue.sh` exists for target/ sweeps.

## Layout

| path | what |
|---|---|
| `crates/kernel/` | types + storage + security + transactions — every other crate depends on it |
| `crates/storage/aikoql-v2/` | **the storage engine** (LSM: memtable/WAL/segments/compaction/snapshot) |
| `crates/engines/` | graph, vector, scheduler, semantic, reasoning |
| `crates/compiler/`, `crates/runtime/` | aikoql language → KIR → planner → runtime |
| `crates/services/api/mcp/` | the MCP server (stdio/TCP, auth, tenancy) |
| `crates/sdk/python/` | the first-party SDK |
| `crates/ingestion/`, `crates/providers/` | document ingestion; postgres/sqlite/mongo/neo4j import providers |
| `crates/certification/`, `benchmarks/` | certification suites; benchmark harness |
| `scripts/` | every CI gate + the dogfood loop + benchmark tooling |
| `kb/` | the dogfood knowledge base — the repo's own aikoql plugin serves it over MCP (P1-8); `kb.artifacts` is untracked local state |
| `.github/workflows/` | ci.yml (correctness + gates), baseline-guard.yml, benchmark-nightly.yml, coverage-floor.yml, perf-smoke.yml, release.yml — being consolidated to ci/benchmark/release by the launch plan's phase CI |

## Historical (cite, don't edit)

`docs/archive/`, `docs/red-archive/` (captured RED evidence), the
`AIKOQL_*_TDD.md` certification docs, `docs/certification/`, `docs/qa/`,
and the phase plans `IMPLEMENTATION-PLAN{,-PHASE3,-V2}.md` record shipped
history. Phase S of the launch plan will archive further v1-era artifacts;
check its status before trusting any v1-era doc.

## CI map (current)

- `ci.yml` — fmt, clippy, tests (Linux+Windows), dependency-dag gates,
  SDK contract, docker/plugin/e2e/connector smokes
- `baseline-guard.yml` — 1M gate-5 self-regression vs the committed v2 baseline (fresh-twin upload, S-03)
- `benchmark-nightly.yml` — weekly: shuffle, benchmark, competitor scale
- `coverage-floor.yml`, `perf-smoke.yml` — path-gated on storage changes
- `release.yml` — tag-driven version gate + builds + npm/ghcr/pypi
