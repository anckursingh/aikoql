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
| P0-2 | Env-gate registry + drift sweep | P0 | PLANNED | centralize the ci.yml `--skip` lists (sfm009, kse19) |
| P0-3 | Deterministic damage corpus | P0 | PLANNED | shared corpus for the FormatError classifiers |
| P1-4 | Seed-determinism gate | P1 | PLANNED | CI grep: no unseeded RNG / bare set_var in new tests |
| P1-5 | Shuffle runs | P1 | PLANNED | nightly randomized-order run + residue sweepers |
| P1-6 | Per-commit perf smoke budget | P1 | PLANNED | tiny fixed cell set, generous 3× budget, storage paths only |
| P1-7 | Coverage floor on codec/replay | P1 | PLANNED | grcov delta pinned on checkpoint/snapshot/replay |
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

### P0-2 — Env-gate registry + drift sweep (PLANNED)

The env gates live in the ci.yml `--skip` lists (sfm009, kse19, dominance
cells). Centralize them in one `tests/gated.toml` (test, gate, why,
verified-at-head) that the workflow reads; CI asserts **both directions** —
every registered test is skipped, every skipped test is registered — plus a
nightly run with all gates armed. Kills the silent-regression hole that
gating creates.

### P0-3 — Deterministic damage corpus (PLANNED)

Recovery tests hand-roll corruption (truncate, bit-flip, zero checksum
region). One helper applies a shared corpus of deterministic mutations to
golden checkpoint/WAL fixtures, and a matrix test runs the full corpus
through the FormatError classifiers (Io vs Corrupt vs Stale). Every new
codec path inherits the classifier coverage — the R3-004 discovery (codec
round-tripped all six fields, nothing asserted it) is exactly this class.

### P1-4 — Seed-determinism gate (PLANNED)

The two known flake classes (env leakage into parallel children, exact-draw
reproduction) both trace to uncontrolled test-environment state. A CI grep
fails new tests using unseeded RNG or bare `set_var`, requiring the seeded /
`start_with` helpers.

### P1-5 — Shuffle runs (PLANNED)

Nightly randomized-order run (nextest — not currently in the tree) with the
residue-sweeper asserts armed, to catch cross-test interference at night
instead of in the PR that adopted the dependency.

### P1-6 — Per-commit perf smoke budget (PLANNED)

The 1M Baseline Guard is nightly/manual. A tiny fixed cell set (point_read,
hot-head, one recall cell) with a generous 3× budget on storage-crate
changes only catches O(n²) regressions at commit time — the cheap version of
gate-5.

### P1-7 — Coverage floor on codec/replay (PLANNED)

A grcov delta on checkpoint/snapshot/replay, failing on decrease vs the
pinned baseline, makes the next "it round-trips but nobody asserts it" show
up red instead of in a review.

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
