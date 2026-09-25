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

S-01 shipped 2026-09-24 (6bdcb3b) — the gate exists with its five
storage-leg assertions; RED archived as `arch-hygiene-storage-v1-estate`
(exit 1 against the live tree: deprecated members, four v1 deps, redb +
backend-selection machinery, the scale.py pin). The assertions flip green
through S-02 (decommission) and S-05 (harness); dag wiring rides CI-04 —
a red gate must not enter CI.

S-02 shipped 2026-09-24 — `crates/storage/aikoql` + `crates/storage/rocksdb`
deleted (12.1k lines), kernel storage = aikoql-v2 directly: `store_redb.rs`,
the `redb` dep, backend selection, `AIKOQL_BACKEND` and `BackendEnvGuard` are
gone; the 6 consumer crates (kernel, runtime, mcp, python-sdk, ingestion,
benchmarks) compile against v2 only. The v1 WAL migrator survives as the
vendored `legacy_envelope.rs` in aikoql-v2 (frozen parser). Backup/restore =
the v2 `StorageAdminApi` snapshot/restore path everywhere (the old trait
defaults are deleted). The v1 legs of benchmark-nightly (smoke, KSE-12/13/19
jobs) and the v1 `gated.toml` entries are dropped. RED archived as
`s02-v1-decommission` (exit 101: the workspace fails to resolve with the
crates deleted at the pre-fix head; pre-fix 4f4721a). All five hygiene
assertions green; S-03 (test-estate sweep) and S-05 (harness re-scope)
remain.

S-03 shipped 2026-09-25 — the test estate is v2-only. `kse_m7_v2_workloads`
is storage self-regression: the matrix is memory (in-RAM reference) +
aikoql-v2; gate 5 = the fresh W1/W2 P50s vs the committed v2 baseline at
the same scale (result.json at 100K, result-1m-aikoql-v2.json at 1M, smoke
NOT_EVIDENCED), bound 1.5× (GATE5_SELF_REGRESSION_BOUND — same-runner P50s
are stable; the per-commit smoke keeps its 3× budget). `AIKOQL_REPORT_FRESH=1`
(strict opt-in; requires STORAGE_REGRESSION + AIKOQL_REPORT_WRITE=1) writes the
-fresh twin beside the committed baseline so a gate run can never clobber
it; `STORAGE_BACKEND` accepts only memory/aikoql-v2 (the dead aikoql leg
fails closed at the filter — RED `s03-v1-estate-sweep`, exit 101 at
b1e90e2). baseline-guard: the v1 republish job is deleted (guard cost
halves); the guard arms FRESH and uploads the fresh twin as the
next-baseline evidence; the gd001 pin forbids a re-added aikoql leg. The
committed baselines predate the M37 *_ns rename, so the guard is RED on
the stale 1M baseline BY DESIGN until the maintainer commits the first
uploaded fresh twin (scripts/gate5-check.py stays strict; the suite's hand
parser accepts p50_us for the historical rows). S-05 (benchmark harness)
remains.

S-04 shipped 2026-09-25 — the adoption-era language is gone from the
functional surfaces. The env names are renamed: `V2ADOPT_NIGHTLY` →
`STORAGE_REGRESSION`, `V2ADOPT_BACKEND` → `STORAGE_BACKEND`,
`V2ADOPT_PERF_SMOKE` → `STORAGE_PERF_SMOKE`, `V2ADOPT_LOADER` →
`STORAGE_LOADER`, `V2ADOPT_LOADER_BACKEND` → `STORAGE_LOADER_BACKEND`
(Rust const names like `NIGHTLY_ENV` are internal and stay; the neutral
`AIKOQL_REPORT_WRITE`/`AIKOQL_REPORT_FRESH` keep their names). Gate 5 is
redefined in §26 as current-vs-committed-baseline self-regression (the
1.5× bound on same-scale W1/W2 P50s — the v1 baseline died with S-02). The
pre-launch plans/reviews (`ARCHITECT-REVIEW-2026-09.md`, the PHASE3/PHASE5
implementation + testing plans, the V2 implementation + testing plans) are
marked HISTORICAL at the top, not deleted; `PR6-TDD-DISPOSITIONS.md` and
the benchmark artifacts stay untouched as frozen evidence. The language
leg lives in `scripts/check-estate-hygiene.sh` (crates/scripts/.github/
tests/gated.toml/AGENTS.md must carry no V2ADOPT-era env names — the
bracket pattern keeps the gate from self-matching). RED archived as
`s04-v2adopt-language` (exit 1 at 75a5d89). S-05 (benchmark harness)
remains.

S-05 shipped 2026-09-25 — the benchmark harness is v2-only, pinned. The
functional re-pointing was consumed by S-02 (`benchmarks/` = one of the 6
consumer crates) and P5-M26 (the competitor_bench python columns call the
embedded SDK); this milestone closes the pin: `check-architecture-hygiene.sh`
gains a sixth leg — no redb name, no `backend=` kwarg-style selection in
the rust or python harness (`benchmarks/`, `scripts/competitor_bench/`) —
and the v1 historical numbers are formally frozen by
`artifacts/storage-engine-v2/V1-HISTORICAL.md` (result-1m-{aikoql,redb,
memory}.json + workloads-1m-{aikoql,redb,memory}.md; nothing regenerates
them; the live baselines stay result.json + result-1m-aikoql-v2.json).
All six hygiene assertions green; the 3 bench targets compile against v2
(`cargo bench --no-run`). RED archived as `s05-harness-v1-pin-missing`
(exit 1 at c6ee00a: the gate carried no harness backend-kwarg pin).
Phase CI (CI-01..09) remains.

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

CI-01 shipped 2026-09-25 — the workflow architecture tests (review 2 §22)
join `check-architecture-hygiene.sh` as the workflow leg, five tests
prescribing the post-consolidation estate by name (never via file count):
`test_required_ci_jobs_exist` (check/test-linux/lint/dependency-dag +
fmt/clippy/check/test steps), `test_benchmark_workflow_exists`
(benchmark.yml owns the 1M + competitor matrix alone; baseline-guard +
benchmark-nightly merged away), `test_competitor_matrix_exists` (the
engine column set + a workflow job runs scale.py), `test_perf_smoke_
remains_wired` (the five review cells under the 3× budget),
`test_release_workflow_remains_wired` (version gate + identity
verification). RED archived as `ci01-workflow-estate` (exit 1 at 4cc965f:
benchmark.yml absent, both pre-consolidation homes still exist, the
smoke carries 3 of the 5 cells). Tests 1/3/5 hold already against the
live estate; 2 flips at CI-02, 4 at CI-03. Dag wiring rides CI-04 (a RED
gate must not enter CI).

CI-02 shipped 2026-09-25 — `benchmark.yml` is the one benchmark owner:
baseline-guard + benchmark-nightly merged into it (deleted), the dag's
gd001 pins re-pointed to its guard job, the shuffle-wiring gate follows.
Tiers: PR = the per-commit perf smoke (perf-smoke.yml; folded into ci.yml
by CI-03); main = `self-regression-main` (NEW) — 100K gate-5 on push to
main, the suite self-asserts the 1.5× bound at that scale so no
gate5-check.py leg; nightly = the 1M guard (fresh twin vs the committed
v2 baseline, fresh-1m-v2 upload) + shuffle/competitor-scale on the weekly
cron (event-guarded schedule|dispatch); release = the weekly full
certification (R5 `--ignored`, skipped-suite smoke, gated cells ungated,
criterion baseline regression). The plan's "three homes" were already two
at merge time: the republish job died at S-03 (L-24's `*_us` baseline
stays stale by design; the guard RED on it is unchanged). The arch gate's
test 2 flips green (only the 3 perf-smoke cell lines stay RED — CI-03);
the other-workflows sweep now uses run signatures (`export
STORAGE_REGRESSION=1m`) — a pin reference is not a run. RED archived as
`ci02-benchmark-owner-missing` (exit 1 at ef1a5b1: the 1M/competitor run
legs lived in 2 files, the owner absent).

CI-03 shipped 2026-09-25 — `coverage-floor` + `perf-smoke` folded into
ci.yml and the smoke grows to the review's W1–W5. Both jobs are always-run
with a fast exit on non-matching paths (the base-sha diff; the fetch
failure falls through to the full leg) — a path-gated required workflow
skips and a required skipped check pends forever (§16). The smoke cells:
W1 point lookup + W2 write throughput + W3 scan (the 2K matrix rows under
`STORAGE_PERF_SMOKE=1` — KO get, ingestion mean-commit-wall, type scan),
W4 hot-cache (SE2M11 hot-head), W5 small compaction (NEW
`compaction_smoke` — one merge of two 25K-key segments under a counting
allocator: wall + allocs, stamped with the tested HEAD), plus the
self-asserting recall cell; all seven budgeted at the existing 3× vs the
committed baseline (4 new cells measured on first genuine run). The dag's
SMOKE/COV pins and the arch gate's test 4 re-point to the ci.yml job keys
and a gone-check keeps the old files merged away; test 2's sweep is now
`ci release`. RED archived as `ci03-two-specialized-workflows` (exit 1 at
5919fdd: both specialized workflows existed; the gate flips to exit 0 with
the fold — jobs present, files gone).

CI-04 shipped 2026-09-25 — the dag job goes historical → architectural.
Deleted from it: the inline P3-M0 deleted-harness grep, the inline P3-M9
deleted-SDK/proxy grep, the `check-estate-hygiene.sh` step, and the
`check-no-tracked-node-modules.sh` step (both scripts deleted — the
R2-012/R3-002 estate pins were history). The S-04 language leg folded
into `check-architecture-hygiene.sh` as storage leg 7 (the bracket trick
keeps the gate from self-matching) so the rename guarantee survives the
estate script's retirement. Joined: `check-architecture-hygiene.sh` runs
in the dag job (the S-01/CI-01 architecture assertions — a RED gate
could not enter CI before, and CI-03's end made it fully green). The
active gates stay: disposition head, red-archives, skip-drift, test-env
hygiene, shuffle wiring, the benchmark/smoke/floor wiring pins, and the
dogfood freshness pin. RED archived as `ci04-dag-pins-history` (exit 1
at ea9aa77: the dag still pinned the historical estate and lacked the
architecture gate; flips to exit 0 with the legs deleted and the gate
wired).

CI-05 shipped 2026-09-25 — cargo caching. `Swatinem/rust-cache@v2` added
after the toolchain step in all 19 cargo build jobs: ci.yml (check,
test-linux, lint, build-release, connectors, python-sdk, perf-smoke,
coverage-floor), benchmark.yml (shuffle, benchmark, guard,
self-regression-main, competitor-scale), release.yml (windows, linux-gnu,
linux-musl, macos-intel, macos-arm, pypi-publish). The action's default
key covers OS + rust version + Cargo.lock — the plan's keying, no
explicit key block. The dependency-dag job never compiles (grep-only)
and the docker job builds inside the image — neither gets one. The arch
gate's workflow test 6 (`test_build_jobs_cached`) pins the cache per job
by name so a bad merge can't silently drop it. RED archived as
`ci05-no-build-cache-anywhere` (exit 1 at 582eacc: grep for the action
in the three workflows found nothing; flips to exit 0 with the wiring).
PR-loop runtime: pre-cache measured 23m51s (CI run 36099891181, the
last pre-CI-05 run); the post-cache number rides the first CI run after
this push and gets recorded when it lands. Also in this commit set: the
CI-03 smoke's first CI-run RED fixed — run 36099891181 measured the
write cell at 4.11x the laptop baseline (3.07 ms vs 0.75 ms), the
documented shared-runner fsync class is 4-7x, so the fsync-heavy wall
cells (write, compact wall) get an 8x budget in perf-smoke-check.py
while the structural cells keep 3x (the smoke's charter — O(n^2)-class
regressions — still holds). The same run's Test (Linux) attribution-10%
cell and python-sdk ConnectionRefused failures are pre-existing at the
CI-03 tip (run 36098365429 shows both) — tracked, not caused by CI-04.

CI-06 shipped 2026-09-25 — path filters + the required-check invariant.
`benchmark.yml`'s trigger set re-pointed to the CI-06 protected paths:
`crates/storage`, `crates/kernel`, `crates/engines`, `benchmarks`,
`scripts/competitor_bench`, `Cargo.lock` (the paths that can move the
gate-5 ratio) + the wiring self-paths; `crates/compiler` +
`crates/runtime` leave the set — they cannot move the 1M storage ratio,
and a non-matching PR must not pay a 1M guard run. The dag's gd001 pin
loops the same set. The arch gate's workflow test 7
(`test_required_checks_never_path_filter`) pins the invariant: ci.yml
carries NO workflow-level path filter (a path-gated ci.yml skips and a
required skipped check pends forever — §16; the gates run always and
decide inside via the fast exits), and the benchmark trigger set is the
protected set — compiler/runtime presence fails the gate. RED archived
as `ci06-benchmark-paths-unfiltered` (exit 1 at e7a9366: the trigger
paths lacked engines/benchmarks/competitor_bench/Cargo.lock; flips to
exit 0 with the set).

CI-07 shipped 2026-09-25 — the performance moat: the hybrid knowledge
workload. One flagship `knowledge_query` cell walks the whole pipeline —
identity resolution (seq → KOID through a unique index) → metadata
filter (topic scope) → traversal (mentions + derived_from provenance,
outbound) → semantic retrieval (text "cats" + vector) → ranking (RRF
k0=60) — end-to-end on aikoql and the composed stacks (PG+pgvector+app
traversal, Mongo+vector via the bolted-on qdrant mirror, Neo4j+vector);
qdrant alone carries no_analog (a vector store is not a knowledge
stack, §11). The cell rides the nightly Tier 2 `competitor-matrix` job
(bench.py's full matrix + the four service containers; the result lands
in the uploaded artifact — nothing committed, P3-M0 rule 6); the arch
gate's workflow test 8 pins the cell, the job, and the composed-stack
images. RED archived as `ci07-no-hybrid-knowledge-workload` (exit 1 at
23359e2: bench.py carried no knowledge_query cell; flips to exit 0 with
the cell). The stamp exposed two engine behaviors, documented in the
REPORT.md CI-07 section: duplicate-coordinate clusters collapse the
HNSW (the dataset now carries seeded per-note jitter — class geometry
unchanged, scalar fields byte-identical), and the kernel's default
traverse direction merges inbound + outbound (the cell filters to
outbound via the per-hit direction tag).

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
- Goal 2 ecosystem plan (`docs/GOAL-2-ECOSYSTEM-PLAN.md`): ECO-1..15 —
  Database API extraction, native wire protocol, SQL/PG/JDBC-ODBC/Cypher
  adapters, migrations. Starts after launch per its own §32; conflicts
  with the P3-M9 driver re-adopt triggers to resolve first; evidence-gated
  like the roadmap phases.
- CI runtime refinement toward the review's 5–10 min PR target as measured
  data arrives (cache hit ratios, Windows split decisions).
