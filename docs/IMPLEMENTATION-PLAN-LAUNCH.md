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
| M47 republish landed (CI-14/L-24, 2026-09-27) — the `*_us` baseline was stale by design | CLOSED — PR #7's uploaded fresh twin is the committed 1M baseline; guard green; arch test 16 pins the baseline schema |
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
parser accepts p50_us for the historical rows). Landed CI-14 (2026-09-27):
PR #7's uploaded fresh twin is the committed 1M baseline; gate5-check
validates it, and arch test 16 pins the baseline schema on the gate chain.
S-05 (benchmark harness)
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

CI-08 shipped 2026-09-25 — reproducible results + reports. The competitor
artifact is machine-checked against the §13 contract with named errors
and a freshness stamp: `artifact_schema.py` gains `validate_competitor`
(per-engine cpu_seconds/memory_mb/disk_bytes + environment os/cpu/ram/
cache_state/harness_sha + config/dataset + §18 engine_versions) and
dispatches from the artifact's own shape (engines → competitor, backends
→ 1M); bench.py emits the fields (CPU = the cgroup delta across the bench
call — v2 cpu.stat with a v1 cpuacct.usage fallback, since Docker
Desktop's WSL2 VM mounts cgroup v1; aikoql's is in-process), records
image+digest evidence per composed stack, and writes the §14 result.csv
leg (the trio: json/md/csv — the nightly job uploads json+csv and runs
the same validator, so nightly evidence satisfies the same contract).
§18 pins: no `:latest` anywhere — qdrant moves to `qdrant/qdrant:v1.19.1`
in benchmark.yml + containers.sh (same digest as what :latest pulled),
digests recorded in the artifact; the arch gate's workflow test 9 pins
the §13 fields, the validator, the csv upload, and fails on any unpinned
image (mutation-verified). Schema pins: 9 new competitor tests (23
total). RED archived as `ci08-no-section13-schema` (exit 1 at b7ffac1:
bench.py carried no cpu_seconds; flips to exit 0 with the field).

CI-09 shipped 2026-09-25 — the release tier: release.yml gains the
TESTING-PLAN §6 evidence pack. Five `tier3-*` jobs (needs
validate-versions, tag push + dispatch): tier3-correctness +
tier3-correctness-windows (the full workspace suite — the CI invocation
verbatim incl. the gated-cell skip list, so correctness and the
concurrency/recovery cells run on both OSes exactly as CI runs them),
tier3-coverage (the codec/replay floor), tier3-scale (the full-scale
harness: 100k + 1M + MCP column + multi-op txn, PG service, release
aikoql-mcp + the repo's own release SDK — benchmark.yml's
competitor-scale invocation verbatim, result uploaded), and tier3-matrix
(the competitor matrix with the §13/§18 schema check, the report trio
uploaded as the benchmark report artifact). A release measures exactly
what CI measures. Arch-gate: test 2's one-owner rule is amended for CI-09
— the benchmark legs may live in release.yml ONLY inside their tier3 jobs
(ci.yml and the gate-5 1M regime stay benchmark-owned), and workflow test
10 pins the five jobs, the CI suite invocation, the coverage/scale/matrix
legs, the §13 schema check, and the §18 pinned images (mutation-verified).
RED archived as `ci09-release-runs-no-benchmark` (exit 1 at e659d1c:
release.yml carried no tier3-matrix; flips to exit 0 with the job set).

CI-10 shipped 2026-09-27 — the guard gets room under the cap: the 1M
gate-5 job measured 5h41m against the 360-minute GitHub job cap and hit it
on 2 of 3 post-push runs (5h41m completed at 582eacc; cancelled at
6h0m26s/6h0m45s at ea9aa77/fab6dcf). The log breakdown: seed 2h39m, the
RSS loader child re-seeding the same 1M dataset 2h41m, the ten timed cells
21m — RSS is evidence, not a gate row (gate5-check ratios the workload p50
rows only). The loader child moves behind a strict-opt-in `AIKOQL_RSS` arm
(fresh_arm idiom; unset = no child): the guard arms it on schedule/dispatch
only (per-PR guards land ~3h under the cap), the 100K self-regression-main
job stays armed (minutes within its 240 budget), and the weekly run keeps
full RSS evidence. Arch-gate: workflow test 11 pins the guard slice's
schedule/dispatch-gated arm and the main job's arm (mutation-verified both
ways). RED archived as `ci10-guard-rss-weekly-only` (exit 1 at fab6dcf:
benchmark.yml carried no AIKOQL_RSS arm).

### Phase L — correctness matrices (review 1), launch sequence

(L-00 is done this session — the skip-drift gate is comment-aware, and a
real inline `--skip` still trips it, proven locally.)

| id | milestone | review ids | RED (what fails now) | GREEN (after) |
|---|---|---|---|---|
| L-00 | CI base green: comment-aware skip-drift gate | TDD-032 leg | main's dag job RED on the round-4 comment (live, reproduced) | gate green; mutation leg still catches inline `--skip` |
| L-01 | sorted-publish runtime precondition | TDD-001 | unsorted input: debug panics, release publishes silently | `Err(Invalid)` in both profiles; sorted callers unchanged ✅ 2026-09-28: the precondition moved into the shared `publish_sorted_entries` body as a real check (the two entry-point debug_asserts deleted — one definition covers both sorted entry points, and any memtable-order violation is mangled by the run-reversal into a detectable shape); pins `sorted_publish_rejects_out_of_order_input` + `staged_sorted_publish_rejects_out_of_order_input` (key + same-key-seq inversions, Invalid, no file written), GREEN in debug AND release profiles |
| L-02 | duplicate `(key,seq)` matrix | TDD-002 | matrix unpinned (the guard exists, segment.rs:305) | inside-block / across-boundary / run-edge duplicates fail publish; no segment visible ✅ 2026-09-28: pin-only (the shared guard held) — `dup_fixtures()` covers the three placement classes through BOTH entries (sorting writer fed unsorted, sorted writer in memtable order), each asserting Invalid + an empty publish dir; `across_boundary_fixture_really_straddles` control-publishes with the seq bumped and proves the pair's rids land in different blocks (16 KiB target, dry-pass estimates 16107/20135 vs 16384); the staged entry's existing dup pin + the shared body make the matrix complete; GREEN debug AND release |
| L-03 | memtable replacement accounting | TDD-004 | `put(k,10,1KiB)`×3 → bytes ≈ 3 entries | bytes ≈ 1 entry (replacement adjusts by the value-length delta) ✅ 2026-09-28: accounting moved into `insert` (the one replacement path — apply/apply_object used to charge the full entry per write); a replace adjusts bytes by value_len(new)−value_len(old) (saturating), fresh entries charge key+value+overhead; pin `replacement_accounting_charges_one_entry` (3× same-(key,seq), grow/shrink/tombstone deltas, object path), RED verified live pre-fix; full storage suite green after |
| L-04 | byte/object interleave matrix | TDD-006 | unpinned | the review's 5-row sequence answers byte→seq3 / rid7→seq5 / rid8→seq4 / rid0→None through flush+compact+checkpoint+reopen ✅ 2026-09-28: the matrix EXPOSED a real bug — the segment-layer byte probes (`SegmentReader::get`/`get_many`) answered with the newest row of ANY kind, so after flush the byte read returned the newest OBJECT row (o7b, not v3); RED captured live (memtable stage green, flush stage failed). Root-cause fix: both probes now pass the rid-0 filter to `block_get_v2` (byte rows are rid-0 by construction; v2 blocks decode every entry as rid-0, identity rows skip and the run continues) — the memtable byte path was already filtered, which is why the divergence only showed past flush. Pin `interleave_matrix_answers_through_every_lifecycle_stage` (5-row block on one key + a byte-only key): byte=v3 / rid7=o7b / rid8=o8 / never-written oid=None / byte-only key never answers an object read, asserted at memtable → flush → compact → checkpoint → reopen; GREEN debug AND release |
| L-05 | flush equivalence | TDD-007 | unpinned | randomized generic vs sorted: same logical entries, ordering, read/scan answers, duplicate behavior ✅ 2026-09-28: pin-only (the shared body held — the sorted publish IS the generic publish fed the memtable-order input). `flush_equivalence.rs`: 64 seeded-xorshift64 corpora (key asc/seq-asc memtable order, 2–7 keys × 1–5 versions, empty/small/4 KiB values across the 16 KiB boundary, rids 0/7/8/9, tombstones) — the sorted writer takes the corpus as-is, the generic writer takes a seeded shuffle (permutation self-checked); both must return equal (size, checksum, anchor-set), byte-identical files, and identical `scan` answers. 24 duplicate corpora (an extra run version carrying an earlier seq, order kept so the DUPLICATE guard is what fires) rejected identically: Invalid, no file, both entry points. GREEN debug AND release |
| L-06 | restart-index corruption matrix + self-consistency | TDD-009/010 | corpus has no restart legs | each corruption class → Corrupt/Unsupported, never a wrong answer; `debug_restart_metadata` matches independent decode ✅ 2026-09-28: pin-only (every read path already validated — the checksum is re-stamped after each mutation so the STRUCTURAL validation is what's exercised). `restart_corruption.rs` mutates a published segment's bytes, re-stamps the block checksum, and probes open+get+scan: 10 classes — restart_count overrun, offset past the payload, offset inside the table, offset mid-entry (nonzero shared at a restart → Corrupt), swapped offsets (keys descending), duplicate offsets (equal keys), an entry's shared prefix overrun, an entry's value-length overrun, the v4 dense-cadence count overrun, and an unknown block version with a valid checksum (Unsupported — a future format, not damage). Every class fails closed; pristine originals still answer. Self-consistency: an independent file-bytes decoder (the test replicates the format spec — table bytes + restart-key bytes + restart count summed over data blocks) matches `Db::debug_restart_metadata` on multi-block v2 AND v4 segments; GREEN debug AND release |
| L-07 | cache churn + clock wrap | TDD-011/012 | survivor sets unpinned; wrap semantics undefined | exact survivor sets at caps 1/2/3; 100k churn keeps bytes ≤ cap, no stale-heap eviction, bounded heap garbage; clock at u64::MAX−2 proven ✅ 2026-09-28: pin-only (the gen-stamp LRU held every leg). `cache.rs` unit pins (in-module — the probes read the map directly, a `get` probe would bump a gen and perturb the very order asserted): `exact_survivor_sets_at_caps_one_two_three` asserts the COMPLETE set + byte accounting after every op of scripted hit/overwrite/evict sequences at caps 1/2/3 (overwrite strands and stale-node re-pushes included); `hundred_k_churn_keeps_the_invariants` runs 100k seeded ops asserting bytes ≤ cap after EVERY op + heap ≤ 2× entries at the end; `clock_wrap_degrades_lru_but_never_correctness` sets the clock to u64::MAX−2 (test-only) — stamps wrap to 0, the wrapped entry sorts oldest and is the victim (LRU quality degrades, the documented tradeoff), eviction terminates, bytes stay in cap, garbage bounded. GREEN debug AND release |
| L-08 | WAL overflow safety + streaming replay semantics | TDD-015/016 | encode arithmetic unchecked; streaming legs missing | checked arithmetic everywhere; boundary payloads encode or fail safely; torn-tail vs corrupt preserved on the streaming path ✅ 2026-09-28: TDD-016's streaming legs were already pinned green (damage_corpus.rs — the flip corpus classifies every byte torn-tail vs Corrupt, truncation corpus, reader-vs-in-memory verdict equality over the damage corpus, torn-tail truncation, damage-then-valid fail-closed, seq monotonicity — built with M28/PERF-1); the real gap was TDD-015: every length the frame writes went through unchecked `+` and `as u32`, so a >4 GiB batch encoded an unreadable frame (truncated payload_len → decode reads checksum-Corrupt — the acked batch silently lost). Fix: per-op parts summed with checked_add, the payload sum checked, one u32::try_from guard on the sum bounds every per-op `as u32` below (a length can never exceed the payload it sits inside), and the guard fires BEFORE the frame buffer is sized. Pin `wal_boundary.rs`: `oversized_value_fails_safely` (4 GiB + 1 value → Invalid; RED captured live — the pre-fix encode took 60+ s building the truncated frame, 0.09 s after) + `boundary_payload_still_round_trips` (the guard is not a hair-trigger — a fitting frame still encodes and decodes byte-exact). wal_golden/encode_alloc/streaming/reader/damage/group binaries + the full suite GREEN in debug AND release |
| L-09 | per-replica winner matrix | TDD-019 | unpinned | 5-row matrix + tombstone/Drop/Archive/Retired legs: exact winners ✅ 2026-09-28: pin-only (the merge's per-(key,rid) winner semantics held every leg — SE2-M38: each rid's newest entry wins, older same-rid versions lose, rid-0 byte rows form one last-writer-wins group). `winner_matrix.rs`: `winner_matrix_in_one_segment` (the L-04 5-row interleave on one key — rid0=v3 / rid7=o7b / rid8=o8 / b — flushed to ONE segment and merged via `compact_with(&KeepAll)` since `compact()` skips a single segment by design, the PR#2 SE-05 pre-check); `winner_matrix_across_segments` (the advance-first trap, compaction.rs:215 — a rid's winner in a LATER segment than its loser must not pop as a fresh winner); `tombstone_winner_retires_only_its_replica_and_can_resurrect` (a rid-8 tombstone wins only its own group — v3/o7 untouched, rid8 Retired (§16), then a newer put_object resurrects: the value wins and re-anchors to Segment); `drop_policy_drops_every_replica_of_the_key` (every replica's rows of k gone, both rids Retired, b untouched — entries_in−entries_out = 5); `archive_policy_archives_every_version_with_rids` (all 5 versions archived byte-exact with rids intact in a v4 archive — the pinned multiset carries seq+rid+value — the live space loses k, both rids Retired, the archive never consulted on reopen). GREEN debug AND release |
| L-10 | placement direct-read equivalence | TDD-025 | unpinned | object == placement == direct read through flush/compact/checkpoint/reopen ✅ 2026-09-28: pin-only (the placement machinery held every stage). `placement_equivalence.rs`: three objects with rows, checked by `assert_three_way` at each stage — for every object the placement (rid → PhysicalLocation) is read, the anchor entry is read DIRECT (segment file → block → entry_offset, no directories), and must equal the object read of the anchor key byte-exact (key, value, rid). Stages: pre-flush (placements are Memtable — no direct surface yet, the anchor materializes at the first flush), flush (anchors land on the flushed segment), a second flush that MOVES the anchors (a newer max-seq row relocates the placement), compaction (relocated anchors in the merged L1), checkpoint_now (the placement rides the directory checkpoint), and reopen (the recovered state agrees). Every row of every object verified through the object path at every stage; GREEN debug AND release |
| L-11 | checkpoint+WAL ordering + burned reservations | TDD-023/024 | partial (SE2-M33-41, ckp009) | exact interleave windows across all five layers; recycle vs must-not-collide pinned ✅ 2026-09-28: pin-only (the existing partial estate held — ckp004's five publication-window kills, pr6_r2_003's recycle/never-reuse legs, pr6_003's allocator-floor resume; the review's two exact windows and the checkpointed-reservation sequence were the gap). TDD-023: `ckp010_window_a_checkpoint_starts_then_tail_write` (the review's window A — write → checkpoint starts → write → flush → crash: the first flush's checkpoint parks at its temp write, the parent releases it through the marker handshake so the tail rides the WAL, the second flush's checkpoint is killed at its own temp write; the reopen carries the pre rows, the tail rows, and the tail object's directory) and `ckp010_window_b_checkpoint_then_wal_tail_replays` (window B — write → checkpoint → WAL tail remains → reopen: the checkpoint's prune has run, `log_file_count == 0` proves the tail object's identity/replica/placement rows must come from WAL replay alone). Both verify ALL five layers: byte data, object data, identity, replica, placement. TDD-024 (reserve A → WAL failure → reserve B → checkpoint → restart → reserve C): `checkpointed_reservations_survive_prune_alone_and_never_collide` (LogicalId/ReplicaId — B recovers from the checkpoint alone after the prune, C collides with nothing), `placement_generations_never_collide_across_checkpointed_restart` (the acked placement survives the prune with its generation intact, the fresh write publishes above the floor), `segment_ids_are_manifest_derived_and_never_reused` (SegmentId is not WAL-reserved — derived from the manifest generation at open, never re-handed). GREEN debug AND release |
| L-12 | restart/block-boundary + length matrices | TDD-027/028 | unpinned | fixture list through point/object/scan/prefix/direct read; 1 B..near-limit round-trips ✅ 2026-09-28: `boundary_matrix.rs` — five fixtures, each through point get, object get, scan, prefix scan, placement-direct read, pre/post-flush and after reopen: (1) `restart_boundary_entries_answer_every_path` (entries 15/16 and 16/17 of a 40-key set — the interval boundary around RESTART_INTERVAL=16, restarts at k00/k16/k32, plus a never-created-oid object read); (2) `version_run_crossing_a_restart_keeps_its_head` (k99's five-version run occupies entries 13..17 — the cadence point lands inside the run and defers/repositions; every path sees exactly one k99 row: the newest); (3) `v4_dense_cadence_inside_a_run_and_the_byte_surface_split` (20 object versions + a byte head + a newer object row — the raw file pins v4 data blocks, both anchors at entries ≥16 decode standalone via the dense base, and the byte surface yields the byte head with object-only keys invisible); (4) `key_crossing_a_block_boundary_answers_every_path` (block_target=128 splits a 4-version 1 KiB run across four blocks — the index lookup lands the head's block, the scan collapses the run to one row); (5) `length_boundaries_round_trip_or_fail_deterministically` (1-byte/4 KiB/65535-byte keys and 1-byte/64 KiB values round-trip; a 65536-byte key is Invalid at put, at put_object, and at a direct SegmentWriter publish — the value-side >u32::MAX ceiling stays pinned by wal_boundary.rs). TWO product REDs fixed in the feat commit: the segment scan leaked object rows where the memtable path filters (stor006) — SegmentScan::next now skips rid≠0 rows without recording `last`, so the byte head stays the newest BYTE row even under a newer object row; and the `as u16` key truncation — Db::write rejects >65535-byte keys at the trust boundary (pre-WAL-ack) and publish_sorted_entries at the format boundary. One test-estate correction: snp003 walked OBJECT rows through the byte surface (it depended on the leak) — its row checks now ride get_object with a byte-surface-stays-empty pin. GREEN debug (448/448) AND release |
| L-13 | `OnceLock` concurrent readers | TDD-008 | unpinned | N concurrent readers, identical answers, no panic, parse count == 1 ✅ 2026-09-28: `restart_once_lock.rs` — one segment, one 16 KiB data block (100 keys, restarts every 16th entry), three legs: `sequential_cold_read_parses_once_then_reuses` (cold lookup parses exactly once; 200 reuses parse zero times), `concurrent_warm_readers_answer_identically_without_reparsing` (8 barrier-gated threads × 200 gets — every answer identical, zero re-parses, every get counted), `concurrent_cold_readers_stay_within_the_documented_parse_bound` (fresh handle, simultaneous cold storm on one key — answers identical, parse count lands inside [1, N], a reopen proves zero corruption). The parse-count instrument (`restart_parses`, the read-path stats) is the feat: incremented only when a block's RestartIndex OnceLock is found empty. The row's `== 1` is a deterministic property ONLY for the sequential cold leg; under a simultaneous cold gate the design accepts the SE2-M4 benign race (up to N speculative parses, the losers' copies drop — parse is pure). That tolerance is the challenge's own escape hatch ("explicitly documented and benchmarked"): the parse site cites the P5-M44 representation benchmark (compact blob ~17 B/restart vs ~41 B boxed — the race is that representation's price). GREEN debug (451/451) AND release |
| L-14 | stale publication race + lock-scope proof | TDD-020/021 | unpinned (park hooks exist) | park-hook interleave: CURRENT not advanced, T2 survives, staging cleaned; put completes during compaction I/O ✅ 2026-09-28: pin-only — the P5-M39 estate (`compaction_lock_scope.rs`) held every clause, plus one new pin at the challenge's literal window. TDD-020's four asserts ride csc003: a merge parked at in_io while a flush of 20 newer rows lands → the phase-C generation check discards (stats.stale), CURRENT stays at the flush's generation (not advanced), all 60 rows survive the reopen (T2 survives), no `.compact-staging-*` residue (staging cleaned), three segment files on disk (inputs remain). NEW csc004 closes the one fixture-strength gap — csc003's in_io park has an EMPTY staging, so "staging is cleaned" never swept built files: csc004 parks at after_segment (the challenge's "T1 pauses before publish"), asserts the staged SEGMENT-* output EXISTS in the parked window, then the same interleave — the discard sweeps the built staging, files and dir. TDD-021 rides csc001 (a put COMPLETES while the merge is parked in segment I/O — before compaction I/O resumes, the behavioral proof a counter-only claim can hide) + csc002 (hold/io/publish/total populated by a real merge, hold+io ≤ total — the structural invariant). The synchronization is the marker-file handshake, no sleeps. GREEN debug AND release (the csc binary passed in both L-13 suite runs, 4/4 re-verified) |
| L-15 | shutdown matrix + close races | TDD-022 + B 17/18 | ✅ 2026-09-28: pin-only — the estate held; tests/shutdown_matrix.rs adds the item's six legs: sd001 `shutdown_idle_loses_nothing` (plain drop, reopen intact); sd002 `readers_racing_a_parked_merge_shutdown_stay_correct` (three reader threads storm correct-answer gets while the compactor merge parks at after_segment — the last Arc's drop runs after the readers and joins the released compactor); sd003/sd004 the drop JOINS the merge parked at after_segment/after_manifest (the drop thread blocks on the compactor join until the marker release; the publish then lands atomically — reopen: all 32 rows, one SEGMENT file, CURRENT names an on-disk MANIFEST, staging swept — no orphan authoritative); sd005 checkpointing = the estate-cite, no new test: ckp004's five kill windows (temp_write/temp_fsync/after_checkpoint/after_first_prune/after_prune) + ckp010 pin the process-death close, and the graceful close cannot race a checkpoint — checkpoint_now borrows the caller's &Db, so a drop cannot even run until it returns (the orphan can never publish); sd006a `parked_group_batch_acks_and_survives_shutdown` (B18: a batch parked at GROUP_PARK after_apply — applied, un-acked — when the drop starts; the committer join blocks, the ack fires on release, the row survives reopen — no acknowledged write lost); sd006b `group_drain_while_shutting_down_loses_no_acked_write` (B18: the drop starts while three writers stream 150 batches — every submitted batch acks and all 150 rows survive the reopen). Marker handshakes only, one binary, PARK_LOCK-serialized. GREEN debug (458/458) AND release | idle/reading/staged/publishing/checkpointing/group-commit shutdown: no deadlock/panic, no acknowledged write lost, no orphan authoritative |
| L-16 | WAL allocation scaling + VersionChain stress | TDD-014/005 | ✅ 2026-09-28: `allocation_scaling.rs` — a #[global_allocator] counting allocator (alloc + realloc events, process-global: every test in the binary serializes on TEST_LOCK). TDD-014: the 1/10/100/1,000/10,000 op ladder across six mixes (tiny key/value, 4 KiB value, 64 KiB value, 60 KiB key, all-object ops, mixed put/put_object/delete); the property is structural — per-op allocations must stay effectively constant with respect to the op count; the asserts ride the 1k→10k pair (per-op@10k ≤ 1.5 × per-op@1k — a quadratic term gives ~10×) plus a generous absolute cap; the small-N points are recorded, not asserted (bootstrap noise). MEASURED: ~8–10 allocs/op flat across the whole ladder for every mix (the one-batch claim is now the six-mix ladder). TDD-005: 1 key × 10/100/1,000/10,000 versions in reverse sequence order — the newest-labeled value lands FIRST, the final write must win over all 9,999 predecessors, the scan must collapse the chain to one row; the plan's "O(v) insert" is pinned on the memtable directly — DESCENDING seqs out-order every arrival (partition_point lands at 0, each insert memmoves the chain, ~50M element moves at 10k) and the head must stay the highest seq with the chain seq-ascending and every version intact. Throughput / allocations / p99 RECORDED per the plan (no absolute thresholds). GREEN debug (461/461) AND release | allocs constant vs op count at 1..10k ops × size mix; 1 key × 10k reverse-seq: throughput/allocs/p99 recorded (no absolute threshold) |
| L-17 | replay memory + compaction allocation adversarial | TDD-017/018 | ✅ 2026-09-28: `resource_adversarial.rs` — one binary, a LIVE+PEAK+EVENTS counting allocator (live bytes net out, PEAK = max live with per-window reset, alloc + realloc events), every test serialized on TEST_LOCK. TDD-017: a crash-child writes a 10 MB / 100 MB WAL (4 KiB values, Async) and exits WITHOUT Db::drop — the graceful drop would flush the memtable, so the WAL stays the rows' only copy, a real crash; the parent reopens (recovery) inside the counted window. MEASURED: retained/WAL 1.023 → 1.022 (flat — no overhead term linear in WAL size), transient_extra (PEAK − LIVE_before − retained — what replay held above what it kept) 3,733 → 3,737 bytes (a constant ~3.7 KB — the streaming replay holds no slurp buffer). Asserts: retained ≤ 2× WAL, transient_extra ≤ WAL/4 + 4 MB, ratio@100MB ≤ 1.25 × ratio@10MB, spot rows survive. TDD-018: four fixtures — 1k keys × 1/4/16 versions and 100 keys × 1k versions — with mixed replica IDs (every 8th (key+version) is an object put rotated across three oids, the rest byte puts); flush → second segment → compact measured. MEASURED per-entry compaction allocs 2.26 → 2.08 → 2.03 → 2.01 (flat across 1k → 100k entries). Asserts: the 16k→100k slope ≤ 1.5×, an absolute 2,000/entry cap, compact transient ≤ 2× data + 8 MB, and after reopen every byte winner and every (oid, key) object winner survives with object-only keys still invisible to the byte surface. GREEN debug (463/463) AND release | replay overhead not linear in WAL size (10/100 MB CI, 1 GB nightly); allocation scaling + winners at 1k keys × 1/4/16, 100 keys × 1k versions |
| L-18 | prefix-scan oracle + cache contention | TDD-026/013 | ✅ 2026-09-28: `prefix_scan_oracle.rs` (counting allocator, TEST_LOCK-serialized) closes the TDD-026 gap — the SE2-M12 oracle is byte-only, the L-04 class (object rows) was un-oracled: `tdd026_scan_matches_the_byte_oracle_across_layers_with_object_rows` runs an 800-step churn (byte puts 12/20, object puts 3/20 across three rotating oids, byte deletes 3/20, rewrites 2/20) over 3 prefixes + the full range, checking the scan against the byte-only oracle at every stage (memtable, flushed layers, post-compact, final, reopen) AND the sharper half — every object winner re-answered through get_object after the reopen (present-but-filtered, not dropped). `tdd026_scan_allocs_stay_flat_per_key_across_versions`: 256 keys × 1/4/16 versions + interleaved object rows, flush, warm, measured scan. MEASURED: per_key_allocs = 3.05 EXACTLY at V=1/4/16 (skipped versions and object rows allocate zero — the L-12 borrowed-slice skip); asserts: V=16 ≤ 1.5 × V=1 slope, absolute 128/key cap, every key's byte head yielded. TDD-013: the P5-M45 estate (cache_concurrency.rs: 4 regimes × thread ladder, throughput/batch p50-p99/wait_fraction cells, real-Db segment cell) held — the gap was the challenge's 1..64 rung: the ladder extends to 64 threads (cells green armed, 131s debug), and the contention leg is nightly-wired: benchmark.yml's weekly tier arms `AIKOQL_V2_CACHE_CELLS=1` in release (report cells only, answers pinned in-test — the "shard only on data" gate rides those numbers, and M45's measured decision shipped no sharding). Arch + shuffle wiring gates green. GREEN debug (465/465) AND release | zero avoidable allocs, ordering/newest-byte vs oracle; 1..64 readers QPS/hit/p50/p99/mutex-wait — shard only on data |
| L-19 | proptest model oracles | TDD-029 | unpinned | ✅ 2026-09-28: `proptest_oracles.rs` (dev-dep proptest = "1" — the kernel fuzz_codec in-tree pattern) closes the random-feed gap: every prior oracle (SE2-M12, L-12, L-18) fed fixed or seeded-LCG op sequences. A model (byte-surface BTreeMap + three object-surface maps) rides the engine under RANDOM churn with equality checked at every layer transition: `prop_memtable_matches_model_across_random_churn` (no flush — memtable-resident, checked every 10 ops + final), `prop_flushed_segments_match_model_across_random_churn` (flush every 12 ops, checked after each flush + after reopen), `prop_compaction_matches_model_across_layers` (three flushed chunks → compact; `segments_in >= 2` asserted so the merge path cannot be skipped; object rows present-but-filtered; checked before/after reopen), `prop_wal_crash_replay_matches_model` (the L-17 crash-child pattern, one spawn per case — pinned 96 — the child applies the case and exits WITHOUT Db::drop so the WAL is the rows' only copy; the parent's reopen replays it, object rows included, and must equal the model exactly; the child records its oids + the case as hex files in the shared dir — no env-block ceiling). Keys: fixed 24-key alphabet + arbitrary bytes (1..16); values 1..96 + 97..512. Oracle teeth mutation-verified: dropping apply_model's tombstone arm is caught and SHRUNK to a minimal case — not vacuous. First run vs the estate: GREEN (256/256/256/96 cases, ~50s debug) — pin-only, no product RED. Nightly: benchmark.yml weekly tier runs `PROPTEST_CASES=4096` in release; a caught failure persists under proptest-regressions/ (commit that file). GREEN debug (470/470) AND release (470/470) | memtable/segment/compaction/WAL property tests (dev-dep, in-tree); nightly regression file |
| L-20 | CI mutation harness | TDD-031 | 7 mutations uncaught | ✅ 2026-09-29: `mutation-harness.sh` (ARCHITECT-REVIEW P2-10, promoted) — the gates guarding the CI estate are grep checkers, and a gate that rots is worse than the wiring it watches. Seven mutation classes, one surgical damage each, applied to a detached-worktree copy of the tree, each run through the gate that must catch it (the gate's exit propagates — non-zero is the RED the archive captures): m1 remove-gated-test, m2 rename-gated-test, m3 remove-skip-wiring, m4 remove-nextest-install, m5/m6 remove-pre/post-residue-sweep, m7 remove-protected-path (pull_request block only). RED (TDD-031) at the first run — THREE uncaught, all real teeth gaps: m2 — the dead-name grep is a bare substring match, so a rename keeping the old name as a prefix (x → x_renamed) stays green → leg 3 anchors the signature boundary (`fn $t(`); m3 — ci.yml's registry comment self-matched leg 1's raw grep after the derivation dropped → leg 1 matches live invocation lines only (the comment-aware idiom leg 2 already used); m7 — a path dropped from the pull_request block alone kept the whole-file grep green via the push block while the per-PR 1M guard silently died for it → workflow test 7 pins each CI-06 path in BOTH trigger blocks (count ≥ 2). After the fixes all seven are caught; the live tree stays green. Each mutation captured as a RED archive — `mut-*`, 7 manifest/log pairs (manifests pin the re-runnable harness command, logs carry the gate's non-zero exit, mutation-class note in each). The dependency-dag job runs `mutation-harness.sh all` on every CI pass, so a gate that loses teeth goes RED in the same job that runs the gates | each mutation → captured RED archive (post-consolidation estate); harness wired into the dag job |
| L-21 | gated registry integrity | TDD-032 | duplicate names unchecked | ✅ 2026-09-29: `check-skip-drift.sh` leg 4 — the registry's fourth integrity leg: no duplicate test names. RED (TDD-032), observed at the L-20 tip: a registry copy carrying ONE duplicate name (sfm009_large_wal_snapshot_has_bounded_rss, a second row with its own reason) passes the gate untouched — legs 1-3 stay green (the shape counts match and the name exists in the tree), so one row's reason can rot behind the other while the accounting disagrees. The new leg fails on any duplicate; the real registry (six unique names) stays green. The L-20 mutation harness re-run holds all seven caught with leg 4 in place — the shape/dead-entry/inline-skip legs stay green. RED archived `skip-drift-vs-duplicate-entry` (the P0-2 dead-entry mechanism: GATED_TOML override, exit 1) | duplicates fail; shape/dead-entry/inline-skip legs stay green |
| L-22 | shuffle behavioral proof | TDD-033 | wiring grep only | ✅ 2026-09-29: `shuffle_probe.rs` + `shuffle-proof.sh` — and the proof's RED found the wiring itself was dead: the nightly command was `cargo nextest run --shuffle`, but nextest has NO shuffle flag (verified 0.9.146) and stable libtest gates `--shuffle` behind `-Z unstable-options` (rustc 1.97.1) — the randomized-order run could never shuffle, and the schedule job had never fired on this unpushed branch, so nobody saw it. The nightly shuffle moved to pinned-nightly libtest (run-shuffle.sh: `cargo +nightly test --workspace --all-features --no-fail-fast -- -Z unstable-options --shuffle` + the same gated-registry skip args as ci.yml; benchmark.yml pins the toolchain BY DATE — nightly-2026-09-27 — so rust-cache's toolchain key stays stable; the dead `--nextest` skip-list mode deleted; the nextest install step deleted). The proof: an env-armed probe pair (writer leaks a marker file, reader fails when it runs first — no-ops unless AIKOQL_SHUFFLE_PROBE_DIR is set, so the ordinary suites run them unaffected), serialized (`--test-threads=1`, so the shuffled order IS the execution order) at two pinned seeds — SEED_CONTROL=1 (writer first → PASS, the pair is not merely broken) and SEED_CAUGHT=2 (reader first → FAIL with the probe panic, grep-verified); each probe records its name in the shared order log, so the proof asserts the seed + observed order, not an assumption. Leg 0 pins run-shuffle.sh's live command line — a drift back to a non-shuffling harness makes every seed vacuous and fails the proof. The proof step runs in the shuffle job before the 90-minute run; check-shuffle-wiring.sh pins the live shuffle flag, the pinned-nightly install, and the proof step; the mutation harness grew to nine — m4 re-aimed at the shuffle flag (old m4 mutated the deleted nextest install step), m8 removes the nightly install, m9 removes the proof step — all nine caught, three new/re-aimed `mut-*` archives (stale mut-m4 pair removed). RED archived `shuffle-proof-vs-nonshuffling-harness` (exit 1) |
| L-23 | strong-claims evidence audit | TDD-034 | unpinned | ✅ 2026-09-29: `check-strong-claims.sh` + `tests/strong-claims.toml` — every strong claim in the launch's storage estate (crates/storage/aikoql-v2 + crates/kernel, swept via git ls-files; comment lines matching exactly-once/at-most-once/zero-alloc/O(/bounded — never/must/always excluded as prose noise, 858/857/63 raw hits) must pin to an evidence test that EXISTS (anchored at the signature boundary — the L-20 dead-name lesson), or the claim loses its claim. RED (TDD-034) at the L-22 tip: the sweep vs the EMPTY registry printed 92 uncovered claims before crashing mid-sweep — the Windows console is cp1252 and a claim text containing → raised UnicodeEncodeError, so the archived 92 is a TRUNCATED prefix; the full sweep finds 178 uncovered claims, none mapped to evidence (exit 1 either way). Root cause fixed in the gate (stdout reconfigured utf-8/replace — the sweep can never truncate again); the registry filled: all 178 hits disposed — 126 pinned to evidence tests, 52 lost their claim (prose softened, row removed). The gate's other legs: every row's pinned line must still carry its claim and every evidence must still be a live `fn` in the tree — dead rows fail. Wired into the dependency-dag job (ci.yml); the mutation harness grew to TEN — m10 drops one registry row (the sed pins file+line — three rows share line = 8, so the bare line anchor would over-delete) and the uncovered-hit leg names the orphaned claim (lib.rs:8 — SE2-M3 bounded recovery) and exits 1, archive `mut-m10-drop-strong-claim-row`. RED archived `strong-claims-vs-unpinned` (exit 1) | every strong claim maps to executable evidence or loses the claim; the swept estate stays fully pinned |
| L-24 | republish landing | — | `*_us` baseline stale (by design) | ✅ CI-14 2026-09-27: PR #7's fresh twin committed as the 1M baseline; guard green; arch test 16 pins the schema |
| L-25 | security disposition | 28 alerts + R2-008 + P1-8 | alerts/threads open; kernel loses writes on abrupt MCP close | ✅ 2026-09-29: `docs/SECURITY-DISPOSITION.md` — all 28 open code-scanning alerts read site-by-site and disposed: ZERO real findings. The 20 `hard-coded-cryptographic-value` sites are the zero-initializer of a buffer a CSPRNG fills before use (kms.rs/crypto.rs nonce+key generation — the literal is never the material), the HKDF counter octet (hkdf.rs:38, RFC 5869 §2.3), fixed salt vectors in the durability suite (durability.rs, 10 alerts), the `import neo4j` dev-default password flag (cli.rs) and the connectors' dev-only test credentials — false positive (production) / by-design (tests, dev defaults). The 8 `cleartext-logging` sites are the keygen's deliberate one-time passphrase print (admin.rs:165, commented by-design), the shell printing KOIDs (public identifiers, shell.rs:283), and test diagnostics formatting the secret filter's REDACTED findings or metrics (secret_filter.rs/multimodal_golden.rs/generate_multimodal_fixtures.rs) plus a test variable named `secret` (conformance.rs) — no production secret reaches a log. R2-008: all 8 CodeQL bot threads on PR #6 verified `isResolved: true` on GitHub (GraphQL, 2026-09-29) — nothing left to resolve. P1-8 kernel leg — the durable-by-default flush, pinned by a RED at the MCP boundary: the guarantee holds (engine default `Sync` = fsynced WAL per ack; d04/d05 cover the kernel boundary), the missing pin was the MCP boundary — `mcp_stdio.rs::m_abrupt_kill_preserves_acked_writes` (commit 0b3c126): acked remember writes survive a hard-killed server (TerminateProcess/SIGKILL — no EOF, no Db::drop, no maintainer checkpoint) across a reopen; GREEN against the estate (pin-only). Teeth archived `abrupt-close-vs-async-durability` (exit 101): with the WAL append removed from the write path — an ack riding the memtable alone, the exact loss class the pin guards — the same test REDs; mutation uncommitted, reverted, pin re-run green. Honest limit recorded: the fsync-less `Async` mode survives a process kill via page cache (it only loses on power loss) — the pin guards ack-durability, not power-loss semantics. The 28 alerts stay open on GitHub — the doc is the disposition; dismissal is a separate outward action | per-site disposition recorded; threads resolved; durable-by-default flush pinned by a RED |
| L-26 | release hygiene | — | no LICENSE/CHANGELOG; Dependabot red | ✅ 2026-09-29: `LICENSE` (Apache-2.0, matching the workspace `license` metadata) at the repo root; `CHANGELOG.md` started at v0.1.19 (v0.1.19 = SE2-M41 v2-default ratification; v0.2.0 = the launch release with the honest boundary listed); version decision **v0.2.0** (the plan's recommendation) — workspace bumped 0.1.19 → 0.2.0, both SDKs' `MIN_SERVER_VERSION` follow (the Python version-contract test pins that), go-sdk mock fixtures + release-compose example updated; Dependabot RED **fixed, not just investigated**: the failing `lru` update (run 35959979070) was the StarRocks harness dev-dep `mysql` 26 pinning lru 0.12.5 (advisory GHSA-rhfx-m35p-ff5j, `< 0.16.3`) — mysql's only lru calls were `LruCache::pop_lru` (the advisory's `IterMut` never called), and mysql 28 requires the patched lru 0.18.5 — bumped `mysql` 26 → 28, Cargo.lock now carries only lru 0.16.4/0.18.5 (both ≥ 0.16.3), ingestion check + suite green, alert auto-resolves on push; disposition recorded in SECURITY-DISPOSITION.md §4 | LICENSE+CHANGELOG present; version 0.2.0 ratified; lru update green |
| L-27 | launch cut | — | | ✅ 2026-09-29: tag **v0.2.0** cut at the L-27 tip (lightweight, the v0.1.x convention) — the tag push (user) triggers `release.yml`: the version gate (cargo==npm==plugin==python==tag, website download pins — all dry-run-verified green locally BEFORE the tag), the Tier-3 certification slice (correctness Linux+Windows, coverage floor, 100k+1M scale, competitor matrix with the §13/§18 schema-checked benchmark report artifact), 5 platform builds, docker multi-arch, GitHub Release with checksums, npm + PyPI, and post-publish identity verification. Local artifact smoke on the release binary: `--version` = 0.2.0, MCP initialize OK (server aikoql-mcp 0.2.0), tools/list 102 tools, tools/call OK, SHA-256 checksum verified per VERIFY.md's flow (CHECKSUM-OK) — the release tree ships the same smoke the CI build jobs run. Launch = push; CI round 1 (first tag push, run 36552894167): GH Release + npm 0.2.0 + docker multi-arch + tier3 matrix/coverage/correctness green — three first-execution workflow REDs root-caused and fixed: tier3-scale's venv lacked `pymongo` (ModuleNotFoundError in the scale harness), verify-identity's `gh` step lacked `GH_TOKEN` (exit 4), PyPI `invalid-publisher` (the `aikoql` project never existed on PyPI — the job post-dates v0.1.19; the pending trusted publisher must be configured on PyPI, a user-side action). Also added rescue guards so a re-dispatch over the existing cut skips npm/github-release instead of 403ing. CI round-1 REDs (stall pin, perf-smoke W6 8.54× vs 8×) were pool-contention flakes — both passed at the previous head with zero product-code changes between; rerun on a quieter pool | tag v0.2.0 cut; artifact smoke green; VERIFY.md checksum pass; release pipeline armed |

## 3. Definition of done (launch boundary)

Everything from review 1's §36, plus review 2's §23:

- [ ] Storage V2 is the only active storage engine; v1 gone from CI; no V2 adoption concept remains.
- [ ] The three-workflow estate exists with one benchmark owner; PR CI materially faster (measured).
- [ ] Architecture hygiene protects the CURRENT architecture (storage + workflows), not history.
- [ ] Self-regression (current vs committed baseline) is separate from competitive benchmarking; competitors are nightly/manual and version-pinned.
- [x] The hybrid knowledge workload is benchmarked with the §13 schema + report trio; results reproducible.
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
