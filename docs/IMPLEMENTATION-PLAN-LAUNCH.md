# AikoQL Launch Plan — factors, milestones, order

Branch: `feature/aikoql-db-launch` (created at the PR #6 merge, a2b6921).

Two reviews consumed:
1. `AIKOQL_PR6_Senior_Developer_TDD_Challenge.md` (TDD-001..034, reviewed
   head ed2df55) — the correctness audit of the M38–M47 optimizations.
2. `AIKOQL_CI_Optimization_and_Performance_Moat_Plan.md` — the CI redesign
   around the decision: **Storage V2 is the only storage engine.**

Sibling plan: `docs/TESTING-PLAN-LAUNCH.md`.

## 1. Launch factors (enlisted, honest)

### Group 1 — correctness challenges (review 1: TDD-001..034)

Verified code sites; the full table with RED/GREEN is in §2 (milestones
L-01..L-23). Summary of what my verification confirmed real:

| class | count | representative |
|---|---|---|
| P0 code bugs | 3 | TDD-001 sorted-publish precondition is `debug_assert!`-only (segment.rs:253/281) — release publishes unsorted silently; TDD-004 memtable `bytes` inflates on `(key,seq)` replacement (memtable.rs:114/128 — premature flush → write amp); TDD-015 WAL capacity arithmetic unchecked (no `checked_*` in wal.rs) |
| P0 test-matrix gaps | 17 | duplicate matrix, byte/object interleave, flush equivalence, restart corruption + concurrency, cache churn + clock wrap, streaming replay semantics, per-replica winners, stale-publication race, lock-scope proof, shutdown matrix, checkpoint/WAL ordering, burned reservations, placement direct-read, boundary fixtures |
| P1 evidence gaps | 12 | VersionChain stress, WAL allocation/replay scaling, compaction allocation adversarial, prefix-scan oracle, cache contention, proptest oracles, length boundaries, claims audit, shuffle behavioral proof |
| P0 infra gaps | 2 | TDD-031 CI mutation harness (half-covered by red archives); TDD-032 registry duplicate-name leg |

### Group 2 — open items at the tip

| factor | disposition |
|---|---|
| dag job RED on main: drift gate's raw grep matched a comment quoting `--skip` | **L-00, fixed this session** — gate is comment-aware (mutation leg proven locally) |
| M47 republish not landed — `*_us` baseline stale by design | L-24 (re-scoped in CI-02: the v1-baseline leg disappears with v1) |
| 28 open code-scanning alerts — sites verified: zero-init nonce buffers (crypto.rs/kms.rs = false positive), test vectors (durability.rs), by-design admin passphrase print (admin.rs:165), KOID print (shell.rs:283) | L-25 |
| R2-008's 7 CodeQL threads; P1-8 kernel loses writes on abrupt MCP close | L-25 |
| PR6-012 db.rs decomposition (DEFERRED by the review's own condition); P2-9 wire-contract golden tests | stays deferred / post-launch |

### Group 3 — product positioning (roadmap Phases 1–5, evidence-gated)

No replication/HA (the architect review's existential gap — Phase 2 needs a
time trigger too), single-node, Python-only drivers, MCP-only wire, ops
tooling stops at backup/restore, filtered scan ~4.3× vs PG (certified bound
≤8×, roadmap target ≤2×), no conversation→knowledge ingestion loop. All
post-launch, evidence-gated — listed so the launch's honest boundary is
written down.

### Group 4 — release hygiene

No LICENSE file (metadata says Apache-2.0), no CHANGELOG, Dependabot cargo
update failing (`lru`, run 35959979070), version decision pending
(v0.2.0 recommended). Release pipeline exists: tag-driven version gate +
5 platform builds + docker + npm + pypi + release-identity verification.

### Group 5 — CI estate (review 2: the workflow analysis)

Current estate, classified against the review's ACTIVE/HISTORICAL/
BENCHMARK/DELETE rule:

| workflow (jobs) | classification | findings verified |
|---|---|---|
| ci.yml (12 jobs: check, test-linux, lint, plugin, docker, build-release, npm-tarball-smoke, e2e-dogfood, connectors, certification-artifacts, python-sdk, dependency-dag) | KEEP + trim + absorb | no cargo caching anywhere (every job rebuilds deps); dag job carries historical estate legs (deleted-estate greps, node_modules) that pin history, not the product; docker smoke serves `/data/aikoql.redb` — a v1-backend instance on a product path; 5 V2ADOPT/v1 comment+env refs |
| baseline-guard.yml (guard, republish) | MERGE → benchmark.yml | guard = 1M `V2ADOPT_BACKEND=aikoql-v2`; republish = the SAME 1M with `V2ADOPT_BACKEND=aikoql` — v1 still consumes a full 1M run per guard; violates "one benchmark, one owner" twice over |
| benchmark-nightly.yml (shuffle, benchmark, competitor-scale) | MERGE → benchmark.yml | the 1M's third home |
| coverage-floor.yml (floor) | FOLD → ci.yml | path-gated today; llvm-cov runtime acceptable |
| perf-smoke.yml (smoke) | FOLD → ci.yml | 3 cells (W1/W2, hot-head, ann004) vs the review's W1–W5 target |
| release.yml (12 jobs) | KEEP + add Tier-3 benchmark | already has the version gate + identity verification; missing the full-scale benchmark + report |

Loop cost today: a workspace test change ≈ 50 min per run (recorded), no
cache, and the 1M runs in three workflows. Target per the review: PR CI
5–10 min, main 10–20, nightly 1–4 h. The 5–10 min PR target is adopted as
the goal with an honest caveat — Windows full-suite builds on shared
runners make it ambitious; measure and record, don't promise.

## 2. Milestones

One milestone = one commit set (test RED → feat → docs). Execution order:
**Phase S (storage consolidation) → Phase CI (workflow consolidation) →
Phase L (correctness matrices) → launch sequence** — the estate is
consolidated first so the L matrices run against the estate that will
exist at launch, and the mutation harness (L-20) targets the final
workflows. The three v2 P0 code fixes (TDD-001/004/015) are independent
of the consolidation; they stay in L for matrix sequencing.

### Phase S — Storage V2 only (review 2, §2/§8)

| id | milestone | RED (against the current tree) | GREEN (after) |
|---|---|---|---|
| S-01 | architecture hygiene gate: `scripts/check-architecture-hygiene.sh` | the review's seven RED assertions fail: v2 not the only backend, active v1 refs exist (aikoql-storage is a workspace member and a dependency of certification/scheduler/runtime/python-sdk/mcp/v2 itself; kernel carries `store_redb.rs` + `redb` dep; rocksdb crate still a member), required jobs/benchmark/smoke/release wiring unproven | every assertion green; gate wired into the dag job |
| S-02 | decommission the v1/rocksdb/redb code | consumers break | `crates/storage/aikoql` + `crates/storage/rocksdb` deleted; kernel storage = aikoql-v2 directly (`store_redb.rs`, `redb`, backend selection, `AIKOQL_BACKEND`, `BackendEnvGuard` deleted); the 6 consumer crates compile against v2 only |
| S-03 | test-estate sweep | v1-only suites still run | v1/adoption-only suites deleted or archived to red-archive; `kse_m7_v2_workloads` re-scoped to v2-only self-regression; gated.toml + env-hygiene pins updated (park vars stay; backend pins go); suite runtime drops materially |
| S-04 | concept rename + docs | V2ADOPT/v1-vs-v2 language everywhere | storage-regression/storage-performance/storage-correctness naming; gate-5 redefined as current-vs-committed-baseline self-regression; historical docs marked historical, not deleted |
| S-05 | benchmark harness v2-only | harness links v1 | `benchmarks/` + `scripts/competitor_bench/` run the v2 API; v1 historical numbers frozen as artifacts |

### Phase CI — three workflows (review 2, §1/§19)

| id | milestone | RED | GREEN |
|---|---|---|---|
| CI-01 | workflow architecture tests (the review's §22 TDD) | `test_required_ci_jobs_exist`, `test_benchmark_workflow_exists`, `test_competitor_matrix_exists`, `test_perf_smoke_remains_wired`, `test_release_workflow_remains_wired` fail | all green, wired into the dag job (never via file count) |
| CI-02 | `benchmark.yml` — the one benchmark owner | three homes for the 1M exist | baseline-guard + benchmark-nightly merged; tiers: PR = cheap smoke (in ci.yml), main = 100K self-regression, nightly = 1M self + competitor matrix, release = full certification; republish drops the v1 leg (freshness gate stays on the committed v2 baseline) |
| CI-03 | fold floor + smoke into ci.yml | two specialized workflows | `coverage-floor` + `perf-smoke` are ci.yml jobs; smoke grows to the review's W1–W5 (point lookup, write throughput, scan, hot-cache, small compaction) under the existing 3× budget; both fast-exit on non-matching paths so required checks never pend (the review's §16 trap) |
| CI-04 | dag job: historical → architectural | estate greps + node_modules legs pin history | historical legs deleted; the ACTIVE gates stay (skip-drift, env-hygiene, red-archives, shuffle-wiring, disposition/dogfood freshness) and the S-01/CI-01 architecture assertions join them |
| CI-05 | cargo caching | no cache in any job | Swatinem/rust-cache keyed OS+rust-version+Cargo.lock in every build job; PR-loop runtime measured and recorded |
| CI-06 | path filters + required-check invariant | benchmark paths unfiltered | benchmark.yml path-gated (storage/kernel/engines/benchmarks/competitor_bench/Cargo.lock); ci.yml always exists so no required check can stay pending |
| CI-07 | the performance moat: hybrid knowledge workload | no workload exercises identity resolution → metadata filter → traversal → semantic retrieval → ranking end-to-end | one flagship knowledge-query workload benchmarked against the composed stacks (PG+pgvector+app traversal, Mongo+vector, Neo4j+vector) — the review's §10 proposition, not microbenchmarks; competitors only where semantically relevant (§11) |
| CI-08 | reproducible results + reports | schema ad-hoc | the §13 output schema (commit/engine/engine_version/workload/dataset/config/throughput/p50/p95/p99/cpu/mem/disk + OS/CPU/RAM/seed/cache-state/harness-SHA) enforced by `artifact_schema.py`; pinned competitor versions (§18); report trio json/md/csv (§14) |
| CI-09 | release tier | release runs no benchmark | release.yml gains the Tier-3 full-scale certification + benchmark report artifact |

### Phase L — correctness matrices (review 1), launch sequence

(L-00 is done this session — the skip-drift gate is comment-aware, and a
real inline `--skip` still trips it, proven locally.)

| id | milestone | review ids | RED (what fails now) | GREEN (after) |
|---|---|---|---|---|
| L-00 | CI base green: comment-aware skip-drift gate | TDD-032 leg | main's dag job RED on the round-4 comment (live, reproduced) | gate green; mutation leg still catches inline `--skip` |
| L-01 | sorted-publish runtime precondition | TDD-001 | unsorted input: debug panics, release publishes silently | `Err(Invalid)` in both profiles; sorted callers unchanged |
| L-02 | duplicate `(key,seq)` matrix | TDD-002 | matrix unpinned (the guard exists, segment.rs:305) | inside-block / across-boundary / run-edge duplicates fail publish; no segment visible |
| L-03 | memtable replacement accounting | TDD-004 | `put(k,10,1KiB)`×3 → bytes ≈ 3 entries | bytes ≈ 1 entry (replacement adjusts by the value-length delta) |
| L-04 | byte/object interleave matrix | TDD-006 | unpinned | the review's 5-row sequence answers byte→seq3 / rid7→seq5 / rid8→seq4 / rid0→None through flush+compact+checkpoint+reopen |
| L-05 | flush equivalence | TDD-007 | unpinned | randomized generic vs sorted: same logical entries, ordering, read/scan answers, duplicate behavior |
| L-06 | restart-index corruption matrix + self-consistency | TDD-009/010 | corpus has no restart legs | each corruption class → Corrupt/Unsupported, never a wrong answer; `debug_restart_metadata` matches independent decode |
| L-07 | cache churn + clock wrap | TDD-011/012 | survivor sets unpinned; wrap semantics undefined | exact survivor sets at caps 1/2/3; 100k churn keeps bytes ≤ cap, no stale-heap eviction, bounded heap garbage; clock at u64::MAX−2 proven |
| L-08 | WAL overflow safety + streaming replay semantics | TDD-015/016 | encode arithmetic unchecked; streaming legs missing | checked arithmetic everywhere; boundary payloads encode or fail safely; torn-tail vs corrupt preserved on the streaming path |
| L-09 | per-replica winner matrix | TDD-019 | unpinned | 5-row matrix + tombstone/Drop/Archive/Retired legs: exact winners |
| L-10 | placement direct-read equivalence | TDD-025 | unpinned | object == placement == direct read through flush/compact/checkpoint/reopen |
| L-11 | checkpoint+WAL ordering + burned reservations | TDD-023/024 | partial (SE2-M33-41, ckp009) | exact interleave windows across all five layers; recycle vs must-not-collide pinned |
| L-12 | restart/block-boundary + length matrices | TDD-027/028 | unpinned | fixture list through point/object/scan/prefix/direct read; 1 B..near-limit round-trips |
| L-13 | `OnceLock` concurrent readers | TDD-008 | unpinned | N concurrent readers, identical answers, no panic, parse count == 1 |
| L-14 | stale publication race + lock-scope proof | TDD-020/021 | unpinned (park hooks exist) | park-hook interleave: CURRENT not advanced, T2 survives, staging cleaned; put completes during compaction I/O |
| L-15 | shutdown matrix + close races | TDD-022 + B 17/18 | unpinned | idle/reading/staged/publishing/checkpointing/group-commit shutdown: no deadlock/panic, no acknowledged write lost, no orphan authoritative |
| L-16 | WAL allocation scaling + VersionChain stress | TDD-014/005 | one-batch claim only; O(v) insert unpinned | allocs constant vs op count at 1..10k ops × size mix; 1 key × 10k reverse-seq: throughput/allocs/p99 recorded (no absolute threshold) |
| L-17 | replay memory + compaction allocation adversarial | TDD-017/018 | unpinned | replay overhead not linear in WAL size (10/100 MB CI, 1 GB nightly); allocation scaling + winners at 1k keys × 1/4/16, 100 keys × 1k versions |
| L-18 | prefix-scan oracle + cache contention | TDD-026/013 | unpinned | zero avoidable allocs, ordering/newest-byte vs oracle; 1..64 readers QPS/hit/p50/p99/mutex-wait — shard only on data |
| L-19 | proptest model oracles | TDD-029 | unpinned | memtable/segment/compaction/WAL property tests (dev-dep on storage-v2, in-tree pattern) |
| L-20 | CI mutation harness | TDD-031 | 7 mutations uncaught | each of remove/rename gated test, remove skip wiring, remove nextest install, remove pre/post residue sweep, remove protected path → captured RED archive (targets the POST-consolidation estate) |
| L-21 | gated registry integrity | TDD-032 | duplicate names unchecked | duplicates fail; shape/dead-entry/inline-skip legs stay green |
| L-22 | shuffle behavioral proof | TDD-033 | wiring grep only | deliberate order dependency caught by shuffle; seed + order recorded |
| L-23 | strong-claims evidence audit | TDD-034 | unpinned | script lists every O(/zero allocation/exactly once/never/always/bounded/must claim; each maps to executable evidence or loses the claim |
| L-24 | republish landing | — | `*_us` baseline stale (by design) | committed v2 baseline refreshed (v1 leg gone per CI-02); guard green; M47 row flips ✅ |
| L-25 | security disposition | 28 alerts + R2-008 + P1-8 | alerts/threads open; kernel loses writes on abrupt MCP close | per-site disposition recorded (fix real, document false-positive/by-design); threads resolved; kernel flushes durably by default |
| L-26 | release hygiene | — | no LICENSE/CHANGELOG; Dependabot red | LICENSE (Apache-2.0), CHANGELOG from v0.1.19, version decision, `lru` update investigated |
| L-27 | launch cut | — | | tag → release pipeline (incl. Tier-3 benchmark report) → artifact smoke → VERIFY.md pass → launch |

## 3. Definition of done (launch boundary)

Everything from review 1's §36, plus review 2's §23:

- [ ] Storage V2 is the only active storage engine; v1 gone from CI; no V2 adoption concept remains.
- [ ] The three-workflow estate exists with one benchmark owner; PR CI materially faster (measured).
- [ ] Architecture hygiene protects the CURRENT architecture (storage + workflows), not history.
- [ ] Self-regression (current vs committed baseline) is separate from competitive benchmarking; competitors are nightly/manual and version-pinned.
- [ ] The hybrid knowledge workload is benchmarked with the §13 schema + report trio; results reproducible.
- [ ] Cargo caching consistent; path filters never leave a required check pending.
- [ ] Release/package/Docker/E2E gates remain covered.
- [ ] All 16 review-1 DoD items (sorted-publish precondition, memtable accounting, flush equivalence, restart safety, cache churn/wrap, WAL checked arithmetic + semantics, per-replica winners, stale-publication race, shutdown preservation, checkpoint/WAL five-layer reconstruction, no identifier collisions, placement equivalence, structural perf counters, CI mutation REDs, correctness before perf claims) — green.
- [ ] Republish landed; security dispositioned; LICENSE + CHANGELOG present; release pipeline green on the tag.

## 4. Post-launch program (recorded, not bundled)

- Phase C evidence milestones L-16..L-19 if any slip past launch (review-1 P1s).
- PR6-012 db.rs decomposition; P2-9 wire-contract golden tests.
- Roadmap amendments: Phase 2 time trigger (architect point 1), ingestion
  automation (point 2), Phase 1 perf parity on demand.
- CI runtime refinement toward the review's 5–10 min PR target as measured
  data arrives (cache hit ratios, Windows split decisions).
