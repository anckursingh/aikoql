# Architect review — AikoQL vs the agentic-DB field (2026-09-20)

Senior-architect review of the whole project as it stands against the
databases used for agentic AI tasks today, plus enhancements to the TDD
approach. Follow-up milestone plan to the PR6 Round-3 review (head 8c60d74).

Related: [`docs/certification/competitors/REPORT.md`](certification/competitors/REPORT.md),
[`docs/first-class-db-roadmap.md`](first-class-db-roadmap.md),
[`docs/PR6-TDD-DISPOSITIONS.md`](PR6-TDD-DISPOSITIONS.md).

## The position

AikoQL is not racing the incumbents on their axis — and shouldn't. The
measured facts: point_read **185× faster** than Postgres (0.010 vs 1.85 ms),
vector recall **at parity** with Qdrant at N=1 000 (5.0 vs 5.3 ms),
structured_filter **~4.3× slower** than PG (9.11 vs 2.10 ms, honestly
decomposed in the benchmark report). On raw throughput it is a niche engine.
What no competitor in the set has — Postgres/pgvector, Neo4j, Qdrant, mem0,
Zep, Letta, XTDB, Fluree, TerminusDB — is the **epistemic machinery**:
authority-graded assertions, KOID version lineage, contradiction → Conflict
KO → explicit resolution, hash-chained audit, GDPR/HIPAA evidence packs, and
`AS_OF`/`HISTORICAL`/`EPISTEMIC` in the query language itself.

That is the axis agentic AI actually needs and currently fakes. Every agent
memory stack today stores embeddings + text and bolts provenance on in
application code; when two "memories" contradict, the app decides. AikoQL
makes *verifiable knowledge* the storage invariant. The nearest philosophical
cousins are XTDB (bitemporal, Datalog) and Fluree (ledger) — XTDB has time
travel but no epistemic statuses, no conflict-resolution workflow, no
evidence-pack compliance story. That stack is the moat, and it is already
built and oracle-certified.

## Where it stands

| axis | incumbents | AikoQL today |
|---|---|---|
| point reads | PG 1.85 ms | **0.010 ms** |
| vector recall (1k) | Qdrant 5.3 ms | **5.0 ms** (M16 slim ranking) |
| filtered scan | PG 2.10 ms | 9.11 ms — residual gap decomposed (per-row correctness filters + PyO3 marshaling), not mystery |
| temporal queries | XTDB bitemporal | `AS_OF`/`HISTORICAL` — plus epistemic filtering nobody else has |
| provenance / lineage | app-level everywhere | first-class: DERIVED_FROM, audit chains, `trace`/`explain` |
| contradiction handling | app-level everywhere | Conflict KOs + resolution decisions, authority-ranked |
| compliance evidence | bolt-on | exportable GDPR/HIPAA packs with audit-chain hash |
| one engine: KV + graph + vector | none (Qdrant has no graph; Neo4j vectors are add-on) | hybrid recall (RRF/weighted fusion) in the kernel |
| replication / HA | table stakes everywhere | **none** — roadmap Phase 2 |
| multi-node | table stakes | **none** — Phase 3 |
| ecosystem | ORMs/pools/drivers/hosted | Python SDK + MCP only — Phase 4 is demand-gated |

The roadmap's own "gap, honestly" section is the correct self-assessment.
Three points a senior architect adds to it:

1. **The existential gap is Phase 2, not Phase 1.** A system-of-record
   without replication/HA cannot be adopted by any serious agentic product,
   regardless of benchmark ratios. Perf parity buys credibility; HA buys
   existence. The evidence-gated triggers are the right discipline, but
   Phase 2 should carry a *time* trigger too, not only a workload trigger —
   demand will not manifest until it exists, which is a chicken-and-egg the
   roadmap should say out loud.
2. **Ingestion automation is where mem0/Zep win mindshare.** They
   auto-extract memories from conversations. AikoQL has MRFC-0050 (document
   compile) but no conversation→knowledge extraction loop. That is an
   ecosystem/positioning gap, not a storage gap — named because the fastest
   adoption path (drop-in agent memory with provenance) is blocked by it.
3. **The benchmark candor is a competitive asset.** The report's
   decomposition ("the ~2.5 ms aspiration was not met; here is where the
   time goes") is rare and credibility-building. Keep publishing it that
   way; it converts "slower" into "correct, and honestly so" — the brand the
   epistemic engine needs.

**Verdict:** the architecture is sound and differentiated. Do not chase
Qdrant on recall or PG on OLTP beyond the roadmap's ≤2× gate; spend the
headroom on provenance-adjacent surfaces (ingestion loop, conflict-resolution
UX over MCP, HA) where no competitor is even in the race.

## TDD enhancements — plan

The repo's TDD culture is already senior-grade: real REDs with measured
evidence, GREEN pins for env-gated cells citing structural REDs,
oracle-checked certification, disposition docs pinned by a CI head-check
gate. The items below extend that idiom, not replace it. One milestone = one
commit; each DONE row names its commit.

| id | item | priority | status | evidence |
|---|---|---|---|---|
| P0-1 | RED archives as artifacts | P0 | DONE | `scripts/red-archive.sh` + `scripts/check-red-archives.sh` + 4 captured archives + CI step |
| P0-2 | Env-gate registry + drift sweep | P0 | DONE | `tests/gated.toml` + `scripts/skip-list.sh` + `scripts/check-skip-drift.sh` + ungated nightly job |
| P0-3 | Deterministic damage corpus | P0 | DONE | `tests/common/damage.rs` + `tests/damage_corpus.rs` (11 cells, per-byte sweeps) + RED archive |
| P1-4 | Seed-determinism gate | P1 | DONE | `scripts/check-test-env-hygiene.sh` — no unseeded RNG / bare set_var in new tests, 10 pinned sites + fake-tree RED archive, CI-wired |
| P1-5 | Shuffle runs | P1 | DONE | `scripts/run-shuffle.sh` + `scripts/check-residue.sh` + `scripts/check-shuffle-wiring.sh` + nightly shuffle job + RED archive |
| P1-6 | Per-commit perf smoke budget | P1 | DONE | 3 fixed cells + 3× budget vs committed baseline + path-gated workflow + dag pin |
| P1-7 | Coverage floor on codec/replay | P1 | DONE | cargo-llvm-cov gate on checkpoint/snapshot/wal vs committed baseline + path-gated workflow + dag pin |
| P1-8 | Dogfood MRFC-0070 on the review loop | P1 | PLANNED | findings as KOs, fixes reconciled via A8 |
| P2-9 | Wire-contract golden tests (SDK) | P2 | PLANNED | on demand |
| P2-10 | Gate-script mutation harness | P2 | PLANNED | on demand (half-covered by P0-1) |

### P0-1 — RED archives as artifacts (DONE)

Every disposition currently cites a RED as prose ("sfm009 RED 192700 KiB")
that nobody can re-run. `scripts/red-archive.sh` captures a RED as a
reproducible artifact: run the command against the pinned pre-fix state,
assert non-zero exit (that is the RED), and write
`docs/red-archive/<id>.red.log` + a `<id>.json` manifest (id, pre-fix
commit, command, captured-at, captured-head, exit code).
`scripts/check-red-archives.sh` validates the archive directory in CI —
every manifest well-formed, every log carrying its non-zero exit marker —
and the SDK/proxy reference grep excludes the archive directory (the logs
name the deleted estate by design, same treatment as the dispositions doc).

Captured archives:

| id | pre-fix state | exit |
|---|---|---|
| estate-hygiene-vs-origin-main | origin/main (the pre-deletion tree) | 1 — 11 deleted-estate paths tracked |
| disposition-head-vs-origin-main | origin/main (no dispositions doc) | 1 |
| disposition-head-vs-f291130 | f291130 (stamp f7696be vs tip f291130) | 1 |
| sfm009 | worktree at a70773f with snapshot.rs+wal.rs reverted to 8e5dd8e | 101 — RSS 192692 KiB, re-measured vs the round's cited 192700 KiB |

Reproducibility note, recorded honestly: the first capture attempt reverted
to 9a9833d — a *descendant* of the fix commit, which already carries the
fix — and the test passed (GREEN), correctly exposing the wrong base. The
retry with the true parent (8e5dd8e) reproduced the cited RED on the first
attempt. RSS-cell REDs are timing-dependent by nature (the sampler can miss
the transient peak); the capture script's non-zero-exit assertion makes a
silent false capture impossible.

### P0-2 — Env-gate registry + drift sweep (DONE)

The env gates lived in the ci.yml `--skip` lists (sfm009, kse19, dominance
cells). They are now centralized in `tests/gated.toml` — 14 entries, each
with its reason and its `ungated_by` home — and both test jobs derive their
skip list from it via `scripts/skip-list.sh`. `scripts/check-skip-drift.sh`
asserts the wiring (no inline `--skip` may exist in ci.yml), the registry's
shape (one test/reason/ungated_by/verified_at per entry), and — the
dangerous direction — that every registered name still exists in the tree:
a renamed or deleted test silently un-skips itself on CI. RED archived via
the P0-1 mechanism (mutation with one dead entry, exit 1 —
`docs/red-archive/skip-drift-vs-dead-entry.red.log`).

The drift check earned its keep immediately: the ci.yml classification
table claimed the amplification trio runs "weekly benchmark-nightly", but
the nightly workflow has no such step — five cells (amplification trio +
report, sfm009) ran ungated nowhere. The new benchmark-nightly "Gated cells
ungated" job re-runs every `ungated_by = "none"` entry weekly so their
limits can't silently regress.

### P0-3 — Deterministic damage corpus (DONE)

Recovery tests hand-rolled corruption inline. One shared corpus —
`tests/common/damage.rs`, four mutation classes (bit-flip, truncation,
trailing bytes, zeroed region) — and a matrix test (`damage_corpus.rs`,
11 cells) that runs the full corpus through the FormatError classifiers on
synthetic WAL frames, a checkpoint golden, CURRENT, and a real Db. Every
new codec path inherits the classifier coverage by adding its fixture
there — the R3-004 discovery (codec round-tripped all six fields, nothing
asserted it) is exactly this class.

What the sweeps pin, per byte:

- **WAL**: damage with a valid frame after it is Corrupt (KSE-082B);
  damage in the final frame with nothing valid after it is a torn tail —
  replayed as the exact prefix, never as silent data. Truncation at every
  cut replays exactly the complete frames; trailing garbage drops nothing.
- **Checkpoint / CURRENT**: every flip, truncation and trailing byte fails
  closed. Version bytes (and checkpoint placement-variant bytes, whose
  position moves with the record layout — pinned by count, not offset)
  classify Unsupported; everything else Corrupt; a missing file is Io.
- **Stale** is a lifecycle class (generation moved), not byte damage — the
  corpus cannot produce it; phy001–005 pin it.

Two honest first-run findings, both encoded into the matrix: the corpus
RED is archived (`damage-corpus-no-helper`, exit 101 — the test written
before the helper), and the fixture work surfaced that `Db::put` publishes
no WAL frame (memtable until flush) — the Db-level legs seed via `write()`
batches, one frame each.

### P1-4 — Seed-determinism gate (DONE)

`scripts/check-test-env-hygiene.sh` fails any test-code `set_var`/
`remove_var` or unseeded RNG use — the two flake classes named in the plan.
Two legs:

- **Env hygiene.** A bare `set_var` in one test mutates the process-global
  environment for every parallel sibling in the same binary and every
  spawned child — the AIKOQL_BACKEND redb leak from the 2026-09-18 Windows
  flake. The 8 remaining sites are pinned as (file, raw-var) pairs, each
  with its honest one-line reason in the script (`BackendEnvGuard`-scoped
  `AIKOQL_BACKEND`, the park-poll idiom's arm vars). A NEW var in a pinned
  file, or any use in a new file, fails — the allowlist is the review
  point, updated deliberately, never silently.
- **Seed determinism.** `thread_rng()` / `rand::random(` / `from_entropy(`
  carry no allowlist at all: the tree is clean today, and any unseeded RNG
  in new test code fails.

RED captured via the P0-1 mechanism without touching the repo:
`TESTS_ROOT` pointed at a one-file fake tree (`set_var` of a new var +
`thread_rng()`), exit 1 naming both —
`docs/red-archive/env-hygiene-vs-fake-tree.red.log`. Wired into ci.yml's dag
job after the registry checks.

Honest first-run note: the gate's own GREEN run caught two `*_AT`
companion vars the survey had missed — the park-poll idiom sets an
armed-at timestamp next to the park var itself (now pinned with the same
reason). The gate earned its keep before it ever ran in CI.

And the class it names proved live immediately: cert002's `CERT_INJECT`
hook was a process-global env var held open for the whole db-oltp suite —
cert003's determinism assertion (a parallel thread, same process) flaked
mid-window vs after (coverage 0.333 vs 0.0). The follow-up commit
thread-scopes the hook (`with_inject`, thread-local), deterministically
RED-pinned by `cert002b_injection_is_thread_scoped`, and the allowlist
pin is deleted — 10 pins became 8, the review point working as designed.

A second live catch, this time from the suite itself: the cert fix's
full-suite gate flaked 2/2 in `auth_surface` — "server still serving".
Not the guard: the fail-closed child exits 2 with the exact refusal text
(verified in isolation and under a simulated 127.0.0.1:9091 listener).
The RED is the test's own 5s wall-clock deadline, which a debug-build
child on a busy Windows laptop can miss (16.54s binary = both deadlines +
teardown), and its choice of the DEFAULT metrics port — a real local
server held 127.0.0.1:9091 during two of the three flaked windows
(the 2026-09-20 corpse). The follow-up commit raises the deadline to 30s
(the assert is about the guard, not machine speed), moves the probe to a
non-default port (the guard checks the address, not the port), and carries
the child's stderr into the panic so a recurrence is evidence, not a
mystery. RED archived from the live suite (`auth-surface-5s-flake`,
exit 101 — a timing RED, which the P0-1 re-capture assertion cannot
reproduce deterministically; the archive says so).

### P1-5 — Shuffle runs (DONE)

Nightly randomized-order run with the residue-sweeper asserts armed, so
cross-test interference shows up at night instead of in the PR that adopted
the dependency. nextest (new to the tree) runs the workspace with
`--no-fail-fast --shuffle` — process-per-test, the right shape for the
P1-4 interference classes (leaked listeners, temp dirs, tree mutation are
process-level, not thread-level) — and the gated registry feeds it:
`skip-list.sh --nextest` emits the 14 gated cells as a nextest
`-E 'not test(a) and not test(b)'` filter (nextest has no --skip), so the
env-gated cells stay gated in the shuffled world too.

`scripts/check-residue.sh` snapshots listening ports, temp dirs and the
git tree before the run and fails on anything new after it — both flake
classes observed live this campaign (the 2026-09-18 AIKOQL_BACKEND env
leak, the 2026-09-20 port-9091 corpse) would have tripped it. And the
wiring itself is pinned: `check-shuffle-wiring.sh` fails if the scripts
are missing or benchmark-nightly.yml loses its nextest step or either
sweeper arm. RED captured via the P0-1 mechanism against the unwired
tree — all five pieces missing, exit 1
(`docs/red-archive/shuffle-wiring-vs-unwired.red.log`) — and the check
runs in ci.yml's dag job on every PR, since a workflow step can rot
silently in a bad merge.

### P1-6 — Per-commit perf smoke budget (DONE)

The 1M Baseline Guard is nightly/manual. The cheap version of gate-5: three
fixed cells on every change touching their code, with a generous 3× budget
vs a committed baseline — sized to catch O(n²)-class regressions at commit
time, not machine noise. No new measurement code: all three cells are
pre-existing env-armed tests.

- **W1/W2 point reads** — the 2K smoke matrix. `V2ADOPT_PERF_SMOKE=1`
  (strict opt-in) writes `result-smoke.json` / `workloads-smoke.md` with a
  `-smoke` suffix, so the canonical artifacts are still never clobbered
  (SE2-M19 holds); the budget diffs the v2 W1/W2 P50s against the
  baseline's.
- **hot-head** — `SE2M11_NIGHTLY=1` writes `hot-head.md` (100K cached
  lookups, answers pinned per lookup); the check parses its P50 line.
- **recall** — ann004 self-asserts recall@10 ≥ 9 vs the brute-force
  oracle at N=10001, so it needs no budget row.

RED via the P0-1 mechanism (`perf-smoke-no-arm`, exit 1): at the pre-fix
tree the smoke matrix runs green and produces no machine-readable artifact,
so the budget gate has nothing to read. The baseline is the first genuine
measurement (laptop, recorded with its machine string); the 3× budget
absorbs the laptop-vs-runner variance until enough CI runs pin it.
`perf-smoke.yml` path-gates on storage/kernel/vector + the scripts + the
baseline, and ci.yml's dag job pins the wiring inline (a bad merge can't
silently un-arm it).

### P1-7 — Coverage floor on codec/replay (DONE)

The three PR6-003 files must hold their committed line-coverage baseline on
every storage-touching change: `scripts/check-coverage-floor.sh` runs the
storage-v2 suite under cargo-llvm-cov and fails any of the trio below its
committed floor. The floor is a floor, not a delta: coverage *can* only move
when the suite or the code changes, so a decrease below baseline is always a
real signal — the 0.05%-point tolerance absorbs report rounding only.

Baseline (laptop, first genuine measurement — `artifacts/coverage/
coverage-baseline.json`, recorded with rustc 1.97.1 + cargo-llvm-cov 0.9.1
and the machine string, so a toolchain change explains drift and re-baselines
deliberately):

| file | line coverage |
|---|---|
| checkpoint.rs | 96.34% |
| snapshot.rs | 81.11% |
| wal.rs | 89.73% |

RED via the P0-1 mechanism (`coverage-floor-no-baseline`, exit 1): at the
pre-fix tree the storage-v2 suite runs green and there is no baseline to
enforce, so the gate has nothing to read — the structural RED. `coverage-
floor.yml` path-gates on storage + the script + the baseline + itself, and
ci.yml's dag job pins the wiring inline (a bad merge can't silently un-arm
it). The first live run's honest trap, recorded: cargo-llvm-cov 0.9.1's
`report --json` is the raw LLVM export format (per-file segments), not
per-file summaries — the gate parses the `--summary-only` text table
instead, whose last `Cover` column is the line coverage.

### P1-8 — Dogfood MRFC-0070 on the review loop (PLANNED)

Each review finding becomes a Claim/Requirement KO through the repo's own
plugin; each fix commit is reconciled via the A8 `reconcile` tool; the
dispositions doc becomes the *compiled output* of that state, and
`trace_requirement` answers "which tests pin R3-003" by query instead of
prose. The project's own thesis applied to its own TDD workflow — and it
makes the head-check gate (R3-005) check machine state instead of a doc
stamp.

### P2-9 / P2-10 — on demand only

Wire-contract golden tests for the Python SDK (the P5-M21 serde trap class,
versioned golden frames regenerable + diffed in CI); a mutation harness for
the gate scripts themselves (inject an estate path / move the tip, assert
the gates go RED — half-covered by P0-1's archives).
