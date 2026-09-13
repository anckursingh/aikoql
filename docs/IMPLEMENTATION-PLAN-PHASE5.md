# AIKOQL Phase 5 — Implementation Plan (Database 1.0)

Source: `AIKOQL_Next_Level_Database_TDD_Roadmap.md` (architect-agent program, ND-00..ND-14) reviewed 2026-09-13 as Chief Architect against the shipped Phase 3/4 ledger (P3-M0..M9 + P4-M1..M7 all SHIPPED — see `docs/IMPLEMENTATION-PLAN-PHASE3.md`). Companion: `docs/TESTING-PLAN-PHASE5.md`. Work lands directly on `feature/storage-enhancements-phase3` (user directive 2026-09-13 — no Phase-5 branch). Commit per milestone, NO push (user pushes). TDD loop per milestone: current-state analysis → RED (fail for the stated reason) → root-cause GREEN → regression → gates (`cargo fmt --all` + `cargo clippy --all-targets --all-features -- -D warnings`).

## Chief Architect review of the roadmap

**Verdict: the program is right; its baseline is stale.** ND-00 and the ND-01 P0 are already shipped (P3-M6/SE2-M28 1M matrix, SE2-M36/37 crash+recovery, SE2-M39 identity oracle, P4-M3 invariant validator, P4-M1 planner fix). Verified against code 2026-09-13: the executor is materialization-oriented (`Vec<KnowledgeObject>` in `kernel/transaction/kernel.rs`, `runtime/lib.rs`, mcp tools — no `PhysicalOperator`/`next_batch` anywhere), the planner is rule-only (`planner.rs`: one `optimize()` with the P4-M1 full-tuple consecutive-only `dedup_scans`, no cost model), the compiler has no GROUP BY / JOIN surface (one lexer token), and there is no statistics subsystem. **The real program is ND-02..ND-14.**

**Amendments the roadmap needs:**

1. **ND-09 (catalog) moves ahead of ND-07 (unified indexes) and ND-08 (CBO).** The roadmap's own acceptances create the dependency ("catalog integration" in ND-07, "statistics persistence" in ND-08) but its sequence puts the consumers first. Postgres order: catalog → statistics → optimizer. Without the swap, ND-07 ships a throwaway registry and ND-08 ships non-durable statistics.
2. **ND-00/ND-01 become guard-remainder milestones.** ND-00's missing deliverables are CI automation + the plan-equivalence oracle — not the certification itself, which is committed evidence. ND-01's P0 was closed by P4-M1 (ppl001–006); the remainder is property-based tests + the operator-algebra doc.
3. **One cancellation mechanism, not two.** ND-04 and ND-11 both test cancellation; one `CancellationToken` threaded through the operator runtime serves both. Two mechanisms would drift.
4. **Encryption is absent from the roadmap — it must not be.** MRFC-0020 field-level encryption decrypts transparently on read today; streaming operators must define the batch-decrypt boundary or an encrypted DB behaves differently from a plaintext one. Every M4+ RED runs against an encrypted database too.
5. **Temporal semantics in aggregation/join are where knowledge DBs break.** ND-05/ND-06 acceptances say "snapshots" — GROUP BY/JOIN over AS OF needs explicit REDs (aggregate the snapshot, never the version chain).
6. **Naming: ship 1.0 as `aikoql-mcp`.** The roadmap's `aikoql init` / `aikoqld` churns the just-shipped UX unification (QUICKSTART/website/AGENTIC-QUICKSTART all lead with `aikoql-mcp`). P5-M11/M12 acceptances use the existing binary + subcommands; the rename is a 1.0-cutover product decision with aliases, owned by the user, not a Phase-5 milestone.
7. **The roadmap's §10 TDD contract is satisfied by the existing structure** (current-state analysis = per-milestone Current state; design alternatives = per-milestone Design note; rollback = milestone-revert + honest ledger). No new 10-point bureaucracy — the 11 binding rules in the testing plan stay the contract, and they are stronger (crash windows, artifact discipline, evidence-gated batch work).

**Build order (DAG, amendment 1 applied):**

```text
P5-M0 (guard automation) → P5-M1 (planner semantics remainder) → P5-M2 (KOQL) → P5-M3 (logical/physical)
P5-M3 → P5-M4 (streaming execution) → { P5-M5 (aggregation+sort), P5-M6 (joins) }   parallel after M4
P5-M3 → P5-M7 (catalog) → P5-M8 (unified indexes) → P5-M9 (statistics+CBO)
P5-M3 → P5-M10 (transaction contract)                                             parallel track
P5-M2 + P5-M10 → P5-M11 (standalone server) → P5-M12 (CLI/SDK)
P5-M4 + P5-M8 + P5-M9 → P5-M13 (hybrid certification H1–H6)
all of the above → P5-M14 (DB-* certification)
```

**Non-goals carried forward (each with a reopen gate):** the Phase-3 ledger rows stay closed except **cost-based planner — REOPENED by this roadmap (2026-09-13, user directive)**. Aggregation/joins are new surface, not a reopen. Replication/Raft/sharding/GPU/distributed-vector stay closed (roadmap §11 agrees). New Phase-5 non-goals: PG-wire protocol (reopen when a SQL-ecosystem deployment demands it); READ COMMITTED isolation (reopen when a workload needs it — M10 ships SNAPSHOT only); temporal/provenance index types (reopen when a workload profile shows the need); `aikoqld` rename (1.0-cutover decision, reopen at the 1.0 release gate).

## Milestones

### P5-M0 — Baseline certification guard (ND-00)

Current state: 1M matrix SHIPPED (P3-M6: gate 5 W1 7.96× REDLINE, W2 5.81× GREEN, RSS 386.5 MB vs 6.3 GB, identity divergence 0); crash/recovery windows SHIPPED (SE2-M36/37); invariant validator SHIPPED (P4-M3). Missing: the gates run as manual/nightly QA, not per-PR; no differential plan oracle; no machine-readable CI harness.

Deliver: a CI guard job that runs the 1M chain + gate-5 asserts on any PR touching `crates/storage` + `crates/kernel` + `crates/compiler` + `crates/runtime` (path-triggered, so ordinary PRs stay fast); the plan-equivalence oracle harness (optimized vs unoptimized plans over a seeded differential corpus); W1 redline surfaced in CI output, not a spreadsheet.

TDD REDs: gd001 — a PR diff touching those paths with the guard job disabled fails the check (the "cannot bypass" pin — the DAG job requires the guard job); gd002 — oracle harness exists and runs the corpus (compile-error RED on the missing surface); gd003 — a deliberately-wrong rewrite fixture makes the oracle fail.

Acceptance: guard job green on a no-op PR; corpus reproducible from clean checkout; gate-5 asserts visible in CI; 1M re-run procedure documented as a script.

Status: ✅ Shipped 2026-09-13 — gd001 RED→GREEN (dependency-dag pin: baseline-guard.yml must exist, trigger on the four protected paths, run `V2ADOPT_NIGHTLY=1m`; RED captured pre-workflow as "DAG VIOLATION: missing"); `.github/workflows/baseline-guard.yml` (windows-latest, path-triggered, 6h cap, `shell: bash` 1m v2 leg + `scripts/gate5-check.py`); gd002 RED→GREEN (E0432 on missing `aikoql_runtime::plan_oracle`, then module shipped — fingerprint = koid+version per RowSet variant, any side-error is itself a divergence); gd003 + gd003b + gd002b 4/4 (evil constant-flip and predicate-drop both detected; dedup-vs-reference divergence 0); `scripts/gate5-check.py` computes the ratio the harness leaves null by design — on the committed artifacts: W1 7.96× REDLINE, W2 5.81×, GATE 5 PASS. One acceptance pending push: the actual CI 1M run (not executable in-session) — first PR evidence lands with it. Bonus catch: the oracle immediately found a real pre-existing defect — numeric literals lower to `Value::Float` (see P5-M2).

### P5-M1 — Planner semantic correctness remainder (ND-01)

Current state: P0 SHIPPED (P4-M1): dedup = full-tuple (type, subject, roles, tenant) + consecutive-adjacency only; ppl001–006 pins; Search-then-Filter ordering pinned (TDD-COMP-003). Remaining from ND-01: property-based optimizer tests; the operator-algebra doc.

Deliver: `docs/compiler/operator-algebra.md` (each legal rewrite with preconditions and a proof; the P4-M1 dedup destructure stays the compile-time guard); proptest suite over Scan identity — random (type, subject, tenant, roles) tuples, dedup allowed only on full-tuple match; property test that `optimize()` is result-preserving over generated small plans vs the unoptimized executor.

TDD REDs: alg001 — algebra doc enumerates every rewrite with its precondition (doc pin, grep-checked); alg002 — proptest seeds producing cross-tenant Scan pairs fail pre-fix (RED captured from an injected regression).

Acceptance: proptest suite green in CI (seeded, deterministic); every documented rewrite has an executable test; optimize-vs-unoptimized corpus divergence = 0 (feeds the M0 oracle).

Status: ✅ Shipped 2026-09-13 — alg001 RED→GREEN (doc-pin test fails on the missing `docs/compiler/operator-algebra.md`, then the doc ships: the three rewrites with preconditions, proof sketches, pinned-by test ids, and MUST-NOTs; `crates/compiler/tests/operator_algebra_pin.rs` keeps it honest — a rewrite without its section fails the build). alg002 compiler proptest 3/3 (512 cases each, deterministic proptest seed): dedup iff full-tuple match, output scan sequence = input with consecutive duplicates collapsed, Filter between scans blocks dedup — RED captured from an injected regression (tenant dropped from the dedup key → both scan-identity properties fail; restore → green). alg002 runtime oracle proptest (256 cases): generated Scan+Filter pipelines through `Planner::optimize` vs the unoptimized executor, divergence 0 — RED captured from an injected regression (merge_filters dropping the second predicate → caught after 9 cases). Every documented rewrite has a live test id (merge_two_filters, ppl006, ppl001–005, alg002). Suites: compiler 151/0 (84 lib + golden 13 + grammar 42 + doc pin + proptest 3 + 4/4 other), runtime 28/0; fmt + workspace clippy -D warnings green.

### P5-M2 — KOQL v1 remainder (ND-02)

Current state: SHIPPED. Full KOQL v1 grammar: ORDER BY field [ASC|DESC] (+ LIMIT/OFFSET from M1), GROUP BY keys + COUNT/SUM/AVG/MIN/MAX, JOIN … ON — all clauses parse, lower to new `IrOp::{Sort,Aggregate,Join}`, and validate semantically with precise errors. Execution of Sort/Aggregate lands in P5-M5, Join in P5-M6 — the runtime arms fail closed with `UnsupportedOperation("… executes in P5-Mx")` until then. Stable-AST contract shipped (AST_VERSION=1, `VersionedStatement`, serde round-trip pin). Numeric literals lower to `Value::Int` when integral (the P5-M0 oracle defect, fixed by kq010).

Deliver: lexer/parser/AST for ORDER BY, LIMIT/OFFSET, GROUP BY + aggregates, JOIN … ON (all lowered to new IrOp variants that M5/M6 implement — compile-error REDs); AST versioning field + serde round-trip pins; precise semantic errors (unknown type vs unknown property vs security violation fail-closed).

TDD REDs: kq001–kq008 — the roadmap's ND-02 RED list verbatim: syntax, precedence, invalid constructs, temporal expressions, graph traversal, semantic search, provenance/evidence, pagination/ordering (each RED is a parse/lowering failure for the stated reason — none of these constructs exist yet); kq009 — invalid tenant/security reference fails closed at semantic analysis; kq010 — numeric literals lower to `Value::Int` when integral (`Value::Float` otherwise) and the runtime compares them against Int properties (found by the M0 oracle: `WHERE temp == 35` silently returns empty today).

Acceptance: the roadmap's ND-02 list; existing query APIs (MCP `aikoql` tool) untouched — the parser is extended, not replaced (grammar_coverage + golden suites stay green, extended).

Status: ✅ Shipped 2026-09-13 — kq001–kq012 RED→GREEN (RED captured: 26 compile errors on the missing surface — no `order_by`/`group_by`/`join` fields, no `IrOp::{Sort,Aggregate,Join}`, no `parse_versioned`/`AST_VERSION`, no `Code::SecurityViolation`). Syntax + precedence (kq001/2): ORDER BY multi-key with per-key ASC/DESC (direction binds to the key it follows), GROUP BY mixing keys and all five aggregates (COUNT(*) vs COUNT(field) distinguished), JOIN…ON; invalid constructs rejected with precise parser errors (kq003). Composition (kq004–007): temporal/traverse/search/provenance all compose with ORDER BY. Pipeline (kq008): Filter→Aggregate, Join after Filter, Sort after Project and before Limit (ordering-before-pagination). Security fail-closed (kq009): the guard lives in BOTH compile paths — `compile_match` and `compile_scoped` (the MCP `aikoql` tool calls `compile_scoped` with NO semantic pass, so semantic.rs alone was insufficient) — plus semantic `Code::SecurityViolation` (AIKOQL1035); single source of truth = the kernel's `ROLE_TYPE`/`POLICY_TYPE` constants; both sides of a JOIN checked; `aikoql:document` stays queryable. Numeric literals (kq010): `Value::Int` when integral in ±2^63, else `Value::Float` — fixes the P5-M0 oracle defect where `WHERE temp == 35` silently returned empty. Stable AST (kq011): `AST_VERSION = 1`, `VersionedStatement { version, statement }`, full serde round-trip with `"version":1` in the wire form; `parse_versioned` is the stable entry point, `parse` unchanged for internal callers. Precise semantic errors (kq012): ORDER/GROUP unknown field → UnknownProperty (AIKOQL1031), JOIN unknown right type → UnknownType (AIKOQL1030), JOIN ON field → UnknownProperty; closed-schema checks on aggregate arguments too. New `Statement` lint allow (`large_enum_variant`) — transient parse products, same tradeoff as the ingestion AST. Suites: compiler 201/0 (84 lib + golden 16 + grammar 49 + kq 40 + fuzz 4 + doc pin + proptest 3 + 4 other), full workspace green (kernel, v2, runtime, mcp, services — exit 0); fmt + clippy -D warnings green. Existing query APIs untouched (grammar/golden suites extended, not replaced).

### P5-M3 — Logical/physical query model (ND-03)

Current state: compiler lowers AST → single `IrPlan` (semantic.rs + planner.rs, one rule pass). No logical/physical split; EXPLAIN exists as a surface (tool + studio) but prints the IR plan.

Deliver: `IrPlan` → `LogicalPlan` (typed, storage-independent; each operator declares schema/cardinality/ordering/snapshot/authorization behavior per the roadmap) → `PhysicalPlan` (index/scan strategy selection — one strategy per operator initially, the seam exists); EXPLAIN shows logical + physical; deterministic versioned plan serialization.

TDD REDs: qm001 — LogicalPlan compiles from every existing query form (golden corpus extended); qm002 — physical plan selects the vector index path for SIMILAR TO, scan path for plain MATCH (strategy visible in EXPLAIN); qm003 — plan serialization round-trips byte-stable (golden byte-pin python-first, rule 4); qm004 — optimizer never touches storage internals (module-boundary pin: no `aikoql-v2` imports in the logical layer).

Acceptance: the roadmap's ND-03 list; all existing compiler suites (146/0 baseline + P5-M2 additions) green against the split.

Status: ⬜ Proposed

### P5-M4 — Streaming/batch execution (ND-04)

Current state: executor materializes `Vec<KnowledgeObject>` (verified 2026-09-13). The write path streams (P4-M6) — the read path does not. Snapshot/relocation safety groundwork shipped: PhysicalHandle batch resolver (P4-M2: `resolve_many`, stale fail-closed).

Deliver: `trait PhysicalOperator { open / next_batch / close }` runtime; Scan/Filter/Projection/Limit operators streaming over P4-M2 handles with snapshot pinning (operators pin the snapshot at open; relocation = Stale → re-resolve, never old data); one `CancellationToken` threaded through (amendment 3); configurable batch size; encryption batch-decrypt boundary (amendment 4 — operators read decrypted rows through the existing transparent path; boundary pinned).

TDD REDs: st001 empty input; st002 single row; st003 batch boundaries (row count ≡ materialized, order-preserving); st004 huge-result env-gated RSS cell (RSS independent of result cardinality — gate 7); st005 cancellation mid-scan (token drop → operator stops, no partial write); st006 authorization (role-scoped rows excluded identically to materialized); st007 snapshot (relocation during streaming → re-resolve, zero divergence); st008 backpressure (bounded channel, slow consumer never unbounds memory); st009 encrypted-DB parity (same results as plaintext).

Acceptance: the roadmap's ND-04 list; W1/W2 cells re-run — M4 touches the read path, gate 5 applies.

Status: ⬜ Proposed

### P5-M5 — Aggregation + sorting (ND-05)

Current state: no aggregation surface in the compiler (verified 2026-09-13). PostgreSQL semantics are the oracle.

Deliver: COUNT/SUM/AVG/MIN/MAX + GROUP BY + DISTINCT + ORDER BY executing on the M4 runtime; hash aggregation with a documented spill strategy (the roadmap requires it before any production claim — an env-gated spill cell, not a silent OOM); authorization BEFORE aggregation (filter-then-aggregate, pinned); temporal: aggregates run over the snapshot (amendment 5 — AS OF pins).

TDD REDs: ag001 nulls (null key grouping, null payload skipped by AVG); ag002 empty set (COUNT = 0, SUM = NULL); ag003 duplicate values; ag004 mixed types (semantic-analysis error, fail-closed); ag005 large groups env-gated (memory bound holds — gate 7); ag006 deterministic ordering (stable sort, tie-break pinned); ag007 snapshot (AS OF aggregate ≡ materialized at that timestamp); ag008 authorization (aggregates never see unauthorized rows).

Acceptance: the roadmap's ND-05 list; existing suites green; EXPLAIN shows the aggregation node.

Status: ⬜ Proposed

### P5-M6 — Join engine (ND-06)

Current state: no join surface (verified — one lexer token). Graph traversal ships (TRAVERSE) — joins are the relational complement, not a replacement.

Deliver: nested-loop + hash join on the M4 runtime (index join lands with P5-M8's property index — the strategy seam is P5-M3's); inner + left joins; cross-tenant join FAILS CLOSED at semantic analysis (the roadmap's tenant-boundary acceptance, implemented as an error, not a runtime filter); both sides pinned to one snapshot.

TDD REDs: jn001 inner join baseline; jn002 left join preserves unmatched rows; jn003 empty side; jn004 duplicate keys (cartesian multiplication pinned); jn005 null keys (null never matches — documented semantics); jn006 tenant boundary (cross-tenant JOIN = error at analysis); jn007 snapshot boundary (both sides from one snapshot; relocation mid-join test); jn008 authorization (join cannot leak rows either side alone denies); jn009 skew (one hot key, env-gated, bounded — gate 7).

Acceptance: the roadmap's ND-06 list; strategy selection visible in EXPLAIN; correctness oracle = cross-check vs nested-loop on a seeded corpus.

Status: ⬜ Proposed

### P5-M7 — Database catalog (ND-09, moved ahead of ND-07/08)

Current state: ontology registry exists (per-type metadata) but is in-memory/partial — no durable transactional catalog subsystem, no migration framework, no versioning.

Deliver: catalog = KOs stored in the SAME engine (the database stores its own metadata — no second store; reuse the kernel transaction path, catalog rows are a reserved tenant); entities: database/schema/type/property/relationship/constraint/index/tenant/user/role/policy/model/statistics — only the ones P5-M8/M9 consume are fully transactional at first (type, property, relationship, index, statistics); the rest are schema rows with a documented gap; catalog version + migration framework (deterministic upgrades); system-catalog query API (kernel surface first, MCP tool with M12).

TDD REDs: ct001 create/drop type round-trips across restart; ct002 schema evolution (add property → old rows read, new rows see it); ct003 corruption → fail-closed open (P4-M3 invariant-validator pattern); ct004 concurrent metadata changes serialize (kernel single-writer — pin, not new machinery); ct005 migration vN→vN+1 idempotent + deterministic; ct006 version compatibility (old DB opens, migrates, never silently rewrites).

Acceptance: the roadmap's ND-09 list, with the honesty amendment: only the entities needed by M8/M9 are transactional; the rest are schema rows (honest-ledger row).

Status: ⬜ Proposed

### P5-M8 — Unified index subsystem (ND-07, on the catalog)

Current state: vector engine (vec001/002, `upsert_many`, tombstone health — P4-M7) and text engine exist as separate engines with their own maintainers; no common Index API, no catalog integration, no property/relationship/temporal indexes; no rebuild/consistency tooling beyond vector health.

Deliver: `trait Index { create/drop/rebuild/upsert/remove/scan/verify }` over catalog-registered indexes (M7 rows); vector + text engines become implementations — their existing behavior is the contract (vec001/002 pins stay green); NEW property + composite indexes (hash, on the v2 engine, post-M4 read path); relationship/temporal/provenance index types stay catalog-schema-only until a workload demands them (non-goal row); batch APIs (`upsert_many`/`remove_many`/`commit_batch`) evidence-gated per rule 11; stale-entry detection + consistency checker (`verify` — reconcile index vs store); online rebuild.

TDD REDs: the roadmap's ND-07 list — idx2-001..010: create, drop, rebuild, insert, update, delete, recovery, snapshot, stale entries, corruption, consistency verification; property-index REDs run on the M4 read path.

Acceptance: the roadmap's ND-07 list; existing vector/text suites untouched-green; index commits stay OFF the database commit critical path (async maintainer, P4-M7 lag semantics — pinned).

Status: ⬜ Proposed

### P5-M9 — Statistics + cost-based optimizer (ND-08)

Current state: no statistics subsystem (verified 2026-09-13); planner = rule-based (P4-M1 dedup only).

Deliver: statistics collection (type/property cardinality, selectivity, relationship degree, vector candidate density — the roadmap's list) persisted as catalog rows (M7); stale-statistics detection; cost model over M3's physical operators; optimizer = cost-based where statistics exist, rule-based fallback where not (never worse than today — the M0 oracle pins this); EXPLAIN COST.

TDD REDs: the roadmap's ND-08 list — cbo001 highly selective property → index scan over type scan; cbo002 low selectivity → scan over index; cbo003 high graph fanout → traversal strategy change; cbo004 vector-heavy → vector path; cbo005 text-heavy → text path; cbo006 hybrid → modality order pinned; cbo007 temporal → snapshot-aware cost; cbo008 stale statistics detected, plan not silently wrong.

Acceptance: the roadmap's ND-08 list + the plan-equivalence oracle (M0 harness) green over every CBO change — the oracle is the gate, not vibes.

Status: ⬜ Proposed

### P5-M10 — Transaction and isolation contract (ND-10)

Current state: kernel has OCC/MVCC/HLC/atomic write-batch/single-writer commit — internal mechanics, no public contract. P4-M7 bounded async and P3-M7 job tables are adjacent, not a user-facing transaction API.

Deliver: public transaction API (begin/commit/rollback) as kernel surface + MCP tools; documented isolation = **SNAPSHOT only** — the roadmap offers READ COMMITTED, but the kernel implements snapshot semantics; advertising an untested second level violates the roadmap's own "only expose what is tested" (READ COMMITTED becomes a reopen-gate row, not a Phase-5 deliverable); idempotent retry semantics; deterministic conflict errors; transaction metrics.

TDD REDs: tx001 write/write conflict → deterministic conflict error; tx002 read/write (snapshot read sees pre-write state); tx003 concurrent readers; tx004 concurrent writers serialize (kernel single-writer — pin, not new machinery); tx005 failed transaction → rollback leaves zero residue; tx006 crash during commit → zero committed-loss (child-kill park window, rule 5); tx007 idempotent retry (same txn id re-applied = no-op).

Acceptance: the roadmap's ND-10 list minus the second isolation level (honest-ledger row); "no async suspension inside consistency-critical locks" — grep pin + the P4-M7 permit pattern.

Status: ⬜ Proposed

### P5-M11 — Standalone database server (ND-11)

Current state: `aikoql-mcp serve` IS a standalone server (TCP MCP + HTTP/REST/metrics/Studio, auth fail-closed, P3-M1). Missing vs the roadmap: a KOQL-native protocol adapter (KOQL over a wire protocol — today it rides the MCP `aikoql` tool), lifecycle polish (graceful shutdown, request timeouts, connection limits, resource limits).

Deliver: KOQL protocol adapter on the existing server (a plain TCP listener speaking framed KOQL — the roadmap's adapter list has it first; PG-wire stays a non-goal); lifecycle: graceful shutdown (drain → flush → exit), request timeout, connection limit, resource limits wired to existing config; health endpoint already shipped.

TDD REDs: sv001 start/stop (clean shutdown, zero-loss — WAL pin); sv002 restart recovery; sv003 multiple clients (concurrent sessions, isolated); sv004 authentication (bad token fail-closed — the existing exit-2 pin extended); sv005 authorization (role-scoped query via the protocol); sv006 malformed requests (framing errors never crash the server — fuzz); sv007 cancellation (client disconnect → query cancelled via the M4 token); sv008 backpressure (slow client, bounded memory — gate 7); sv009 graceful shutdown mid-query; sv010 crash recovery (child-kill, rule 5).

Acceptance: the roadmap's ND-11 list under the existing binary name (amendment 6 — `aikoqld` is a 1.0-cutover decision).

Status: ⬜ Proposed

### P5-M12 — CLI and SDK (ND-12)

Current state: CLI ships (shell/serve/keygen/backup/restore/import/ingest-dir/audit/model + shell commands); Python SDK ships (sdk001 parity, contract suite 18/18); error codes exist but are not a documented stable contract.

Deliver: stable error-code contract (numbered, documented, versioned); version compatibility contract (SDK ↔ server matrix, fail-fast on mismatch); CLI gains the missing verbs (status, query, explain, index, schema — thin wrappers over existing tools); docs generated from real interfaces (the QUICKSTART/AGENTIC pattern — no hand-maintained API reference).

TDD REDs: cl01 each documented error code is produced by a test (code-table pin); cl02 SDK version mismatch fails fast with the documented error; cl03 each new CLI verb round-trips through the repo-built binary (dogfood rule).

Acceptance: the roadmap's ND-12 list (Python SDK fundamentals already shipped — the gap is contracts, not coverage); time-to-first-query (install → start → ingest → query → connect agent) stays under 5 commands — QUICKSTART already measures this, keep it green.

Status: ⬜ Proposed

### P5-M13 — AI-native hybrid query certification (ND-13)

Current state: the query IR already fuses structured+graph+vector+text+temporal+epistemic+provenance (P3-M4/P4-M1); `find_similar` does hybrid recall (RRF/weighted). Missing: H1–H6 canonical workloads as a reproducible suite; cost-aware index selection (lands with M9); EXPLAIN showing every modality (lands with M3).

Deliver: H1–H6 benchmark suite (the roadmap's six workloads) as seeded, machine-readable benchmarks + correctness assertions per modality; authorization throughout the plan (every H6 sub-query runs with restricted roles and pins the result set); evidence returned deterministically (pinned).

TDD REDs: h1 structured→vector; h2 vector→graph; h3 graph→temporal; h4 temporal→provenance; h5 structured+vector+graph; h6 full stack + ACL — each RED asserts the modality combination against a hand-computed oracle first (correctness), then env-gated latency cells (performance, reported).

Acceptance: the roadmap's ND-13 list; EXPLAIN shows every modality in the plan; benchmarks reproducible from clean checkout.

Status: ⬜ Proposed

### P5-M14 — AIKOQL Database certification (ND-14)

Current state: QA/gate-5/1M suites ship and are reproduced on demand; no public-facing DB-OLTP/DB-GRAPH/DB-VECTOR/DB-KNOWLEDGE/DB-AGENT suite.

Deliver: the five DB-* suites per the roadmap, each with: deterministic datasets, reproducible seeds, machine-readable results, cold/warm, p50/p95/p99, throughput, RSS, disk, correctness parity, no cherry-picked workloads (the roadmap's list is the acceptance); comparisons vs PostgreSQL/Neo4j/vector DBs run as published reports, not CI gates (CI pins AIKOQL-only regressions — competitor numbers are documentation).

TDD REDs: cert001 each suite runs from clean checkout and writes a machine-readable result artifact; cert002 a regression injected into the suite data path fails the CI pin; cert003 DB-AGENT evidence-coverage/provenance-completeness assertions.

Acceptance: the roadmap's ND-14 list; results published in `docs/certification/` with run-date + commit hash.

Status: ⬜ Proposed

## Gates (carried + new)

- **Gate 5** (≤8× W1/W2 at 1M) — carried. W1 is REDLINE at 7.96× (0.04× headroom): every milestone touching storage/kernel/runtime re-runs the 1M matrix before merge (M0's CI job automates this).
- **Gate 6** (new): plan-equivalence oracle — optimized ≡ unoptimized on the differential corpus; zero divergence for every rewrite and CBO change.
- **Gate 7** (new): bounded-memory streaming — RSS independent of result cardinality on the 100k-row cell (envelope set in M4).
- **Gate 8** (new): certification reproducibility — every DB-* suite runs from clean checkout with pinned seeds.
- **Gate 9** (new): tenant/security invariance — no optimizer or executor change may cross tenant/security boundaries (M1 proptest pins this).

## Definition of 1.0

The roadmap's §8 checklist, with the Chief Architect amendments recorded: isolation = SNAPSHOT only; binary name = `aikoql-mcp`; PG-wire + READ COMMITTED + the `aikoqld` rename = reopen-gate rows, not 1.0 blockers.
