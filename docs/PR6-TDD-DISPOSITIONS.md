# PR6 TDD review — dispositions (Round 3)

Response to `AIKOQL_PR6_R3_SENIOR_RUST_TDD_REVIEW.md` (5 findings, P0-P1,
"Request changes before merge"). Current head: the commit that last modified
this file (`git log -1 -- docs/PR6-TDD-DISPOSITIONS.md`) — the CI gate
`scripts/check-disposition-head.sh` fails any change that moves the reviewed
tip without re-stamping this document (R3-005).

| finding | disposition | evidence |
|---|---|---|
| R3-001 (P0) tracked node_modules | NOT A FINDING | diff vs origin/main: 0 added / 278 deleted; 0 tracked at head — `scripts/check-no-tracked-node-modules.sh` proves it on every CI run |
| R3-002 (P1) deleted-estate tree assertions | FIXED | `scripts/check-estate-hygiene.sh` — RED vs origin/main (11 paths), GREEN vs HEAD (8e5dd8e) |
| R3-003 (P1) snapshot WAL read-to-end | FIXED | sfm009 RED 192700 KiB (~2× the 96 MiB WAL) → streamed validation GREEN (a70773f) |
| R3-004 (P1) equivalence floors + chains | COVERED | codec already round-trips all six; equivalence extended + allocation-after-restart cell — GREEN pin, structural RED = PR6-001's ckp009 (f291130) |
| R3-005 (P1) evidence doc staleness | FIXED | this doc re-stamped at the final head + automated head check in CI (this commit) |

Round work beyond the findings: the Windows CI runs during the round exposed
that the PR6-004 concurrency matrix's ack floor measured GitHub's Windows
fsync throughput (~22-33ms per group commit vs ~2ms Linux), not the matrix —
floor hardened to per-writer progress (9a9833d).

## R3-001 — tracked node_modules: NOT A FINDING (evidence)

The review's P0 claims the PR adds 278 tracked node_modules paths. The diff
against the PR base (origin/main) shows the opposite:

- added: 0
- deleted: 278
- tracked at head: 0

The 278 deletions are the R2-012 fix (bf79d41): vendored node_modules
removed from the tree. The PR's own additions tracked zero node_modules
paths. The deletion gate `scripts/check-no-tracked-node-modules.sh` (also
from R2-012) fails CI the moment any path under a node_modules directory is
tracked again, so the disposition is a mechanism, not a promise.

## R3-002 — deleted-estate tree assertions: FIXED (8e5dd8e)

The review's P1: the R2-012 estate deletions had only a textual-reference
grep (P3-M9/P3-M0, ci.yml) — nothing asserted the paths are ABSENT from the
tracked tree, so a re-added file nothing references yet would pass CI.

Fix: `scripts/check-estate-hygiene.sh` — a ref-parameterized gate listing
the deleted-estate paths (`crates/cluster/proxy/`, `crates/sdk/go/`,
`crates/sdk/java/`, `tests/universal_test_harness.py`,
`benchmarks/tests/load_test.rs`, `tests/e2e/`) and exiting 1 on any tracked
match. RED proven against origin/main (11 paths tracked there); GREEN
against HEAD. Wired into ci.yml after the reference greps; the SDK/proxy
grep excludes the script itself (it names the paths by design).

## R3-003 — bounded-memory snapshot WAL copy: FIXED (a70773f)

The review's P1: `snapshot_to` read the whole WAL into memory
(read_to_end), so snapshot RSS scaled with the WAL size. RED:
`sfm009_large_wal_snapshot_has_bounded_rss` grew RSS 192700 KiB (≈ 2× the
96 MiB WAL — the full materialization, twice: validate + copy) against the
48 MiB limit.

Fix: `valid_prefix_len` walks the WAL one frame at a time
(`validate_frame_at`: one frame's bytes in memory, per-offset probe with
exactly `replay_frames`' torn-tail / damage / strict-sequence semantics —
the decode tail extracted into `decode_payload` so the two paths stay
byte-identical), and `copy_wal_prefix` copies the prefix in 64 KiB chunks
under the still-held wal mutex while hashing for the marker. Memory is
O(largest frame), not O(WAL). sfm009 (bounded RSS) and sfm007 (torn-tail
semantics preserved) green; sfm009 is env-gated on CI like kse19
(whole-process RSS cell — sibling tests in the same binary perturb the peak
on shared runners).

## R3-004 — checkpoint equivalence floors + chains: COVERED (f291130)

The review's P1: the PR6-003 equivalence helper compared the three maps
only. It now also asserts the PR6-001 allocator floors (next_logical_id,
next_replica_id, next_placement_generation) and the PR6-R2-002 publish
chains (identity/replica/placement chain) on every round-trip cell.

This is a GREEN pin, not a fake RED: the codec already writes and reads all
six inside the checksum (checkpoint.rs, from PR6-001 / PR6-R2-002), and
reopen consumes them — the floors max into the allocators over any map
recompute (db.rs open path), and the coverage validator compares the
chains. The structural RED is PR6-001's own ckp009 (burned generations
re-handed after checkpoint + prune + reopen): dropping either codec half
fails every round-trip cell here; dropping the open-path max re-opens
ckp009.

Plus `allocators_resume_from_checkpoint_floors_after_prune`: checkpoint +
prune (delta_log_count == 0) + reopen, then the first create must hand out
the checkpointed next lid/rid exactly and a placement generation ≥ the
checkpointed floor (writes re-publish placements with a fresh generation,
so the record moves past the floor — the resume is a lower bound, INV-05).

## R3-005 — evidence doc staleness: FIXED (this commit)

The review's P1: this document claimed a head the branch had moved past.
Fix: the document is re-stamped at the final head (the commit that last
modified it), and `scripts/check-disposition-head.sh` — wired into ci.yml
after the estate gate — fails any change that moves the reviewed tip
without re-stamping. On a two-parent head (PR merge ref, or a pushed merge
commit — this repo merges with merge commits) the reviewed tip is the
second parent; on a plain push it is HEAD. RED proven against origin/main
(no stamp for the doc there at all) and against the pre-stamp branch head
(stamp f7696be vs reviewed tip f291130).

---

## Carried-over dispositions (Rounds 1–2)

Each section below states the head it was verified at; the Round-1 items
are structural dispositions that did not change, and the R2-008 closure
rides on the next push re-running the scan (see that section).

## PR6-008 — CI separation: SATISFIED (structure)

The review requires a fast correctness loop on every PR and the
1M/6-hour certification work on schedules/manual only.

The three-workflow structure already separates them:

| workflow | trigger | content |
|---|---|---|
| ci.yml | every PR (master/main), pushes to master/main/cross-db-support | fmt, clippy -D warnings, tests, plugin/docker/npm/e2e smoke, python SDK |
| baseline-guard.yml | PR (protected paths), push to main (protected paths), manual | 1M gate-5 ratio |
| benchmark-nightly.yml | weekly cron, manual | 1M/certification/competitor-scale runs |

- No PR-triggered job runs the 1M or 6-hour workloads; they are cron-,
  manual-, or protected-path-PR-gated.
- "Correctness must not depend only on scheduled execution" — the full
  correctness suite runs on every PR and on every mainline push.
- The dependency-dag job in ci.yml (gd001) pins this separation itself:
  deleting baseline-guard.yml or un-arming its triggers fails CI.
- The review's <15-minute budget for the fast loop is a timing target, not
  a structural gap; if it drifts, split jobs — don't move work into cron.

## PR6-010 — checksum threat model: SATISFIED

See `docs/checksum-threat-model.md` — the explicit guarantee (accidental
corruption, 2^-64 per protected region), the non-guarantees (authenticity,
tamper resistance), the usage sites, and the change rule (a keyed MAC where
tamper is in scope; never a blanket replacement).

## PR6-012 — db.rs decomposition: DEFERRED (by the review's own condition)

The review itself sets the condition: "Do not refactor this before
correctness invariants are locked by tests."

- Those invariants are now locked: PR6-001–011 of this campaign are exactly
  the named suite (checkpoint completeness, delta coverage, placement
  semantics, concurrency matrix, backend detection, streaming publish,
  snapshot failure injection, kernel-boundary tests, release identity).
- The recommended split is recorded, not lost: db.rs → write_path.rs,
  recovery.rs, checkpoint.rs, compaction_controller.rs, directory_state.rs,
  placement_state.rs.
- Disposition: deferred to its own future change, with the now-green suite
  as the safety net. A decomposition is a refactor with its own RED/GREEN
  budget — bundling it into this remediation campaign would violate the
  one-milestone-one-commit discipline.

## R2-008 — CodeQL threads: CLOSED (verified non-reproducing on current head)

Response to the Round-2 review (PR6-R2-008): the PR carries 7 open review
threads from github-advanced-security[bot] — 6 cleartext-logging on
`crates/runtime/tests/{cbo,cbo_default,cpl_execution}.rs`, 1
actions/missing-workflow-permissions on `.github/workflows/baseline-guard.yml`.

Verification (current head = remote 7577e94; the 11 local commits stacked on
top touch none of the flagged files):

- Repo alert state: no alert for those paths exists in ANY state — open,
  dismissed, closed, and fixed are all empty for the thread-linked alert
  numbers 188–204.
- PR CodeQL check: SUCCESS on the current PR head — the scan is green; the
  threads are carry-over comments from earlier scan rounds (two of them are
  already marked outdated by GitHub).
- The three test files are byte-identical between the last bot review and
  HEAD (`git diff 7577e94..HEAD` is empty for all three).

Why false positives: the sinks are `panic!`/`assert!` diagnostics in tests
that format kernel FIXTURE data (the taint source is the kernel's
trusted-ingestion method, which the tests seed); no production secret
reaches a log file on any of these paths. In-source suppression comments are
not supported for Rust (CodeQL documents them for C/C++, C#, Go, Java/Kotlin,
JS/TS, Python, Ruby only), so the disposition uses the review's other
sanctioned path: explicit thread closure with this verification recorded.

- The permissions finding was already fixed in code before this round: all
  four workflows carry an explicit `permissions: contents: read` block
  (baseline-guard.yml landed in c5ae76d).
- Closure: each thread is resolved after the next push re-runs the scan
  green — the reproducible current-head result the review requires — with
  this document cited in the resolution comment.

Out of scope, recorded for the record: 9 open `rust/cleartext-logging` and
19 open `rust/hard-coded-cryptographic-value` repo alerts (created
2026-08-31, pre-dating this round) on mcp/ingestion/kernel paths. They are
not "findings in runtime tests"; they get their own disposition, not a
bundled fix.

---

## Follow-up milestones (post-R3)

The Round-3 review closed at this head. The architect review
(`docs/ARCHITECT-REVIEW-2026-09.md`) files the project-level assessment vs
the agentic-DB field and the TDD enhancement plan; its P0 items land as
follow-up milestones, one commit each, re-stamping this table as they land.

| milestone | status |
|---|---|
| Architect review doc + TDD plan | FILED (8f9416a) |
| P0-1 RED archives as artifacts | DONE (d9cfcfb) — `scripts/red-archive.sh` + `scripts/check-red-archives.sh` + 5 captured archives + CI step |
| P0-2 env-gate registry + drift sweep | DONE (this commit) — `tests/gated.toml` + `scripts/skip-list.sh` + `scripts/check-skip-drift.sh` + ungated nightly job |
| P0-3 deterministic damage corpus | DONE (this commit) — `tests/common/damage.rs` + `tests/damage_corpus.rs` (11 cells, per-byte sweeps vs synthetic WAL / checkpoint / CURRENT / real Db) + RED archive `damage-corpus-no-helper` |
| P1-4 seed-determinism gate | DONE (4ee7bb1) — `scripts/check-test-env-hygiene.sh`: (file\|token) allowlist pinning the existing set_var sites (each with its honest reason) + zero-allowlist unseeded-RNG leg; CI-wired after check-skip-drift; RED archived `env-hygiene-vs-fake-tree` (fake tree via TESTS_ROOT, exit 1) |
| cert002 injection thread-scope fix | DONE (this commit) — the gate's first live catch: cert002's process-global `CERT_INJECT` env leaked into cert003's parallel-thread seeds (coverage 0.333 mid-window vs 0.0 after); `with_inject` thread-local scopes the hook, RED `cert002b-env-leak` (deterministic 2s window), pin removed (10 → 8) |
| auth_surface deadline/port flake | DONE (this commit) — the cert fix's suite gate flaked 2/2: the test's 5s wall-clock deadline, missed by a debug-build child on a busy laptop (guard verified correct — exits 2 with the refusal text); deadline 30s, probe port off the default 9091 (a real local server held 127.0.0.1:9091 during the flaked windows), child stderr carried in the panic; RED `auth-surface-5s-flake` (live suite, exit 101) |
| P1-5 shuffle runs | DONE (this commit) — `scripts/run-shuffle.sh` (nextest `--shuffle`), `scripts/check-residue.sh` (ports/temp-dirs/tree sweeper before+after), `scripts/check-shuffle-wiring.sh` (fails if the nightly job loses its wiring), nightly shuffle job in benchmark-nightly + wiring gate in ci.yml's dag job, `skip-list.sh --nextest` feeds the gated registry as a nextest filter; RED `shuffle-wiring-vs-unwired` (5 pieces missing, exit 1) |
| P1-6 per-commit perf smoke budget | DONE (this commit) — three fixed cells (W1/W2 point reads via the new `V2ADOPT_PERF_SMOKE=1` smoke artifact arm, hot-head, ann004 recall@10) with a 3× budget vs a committed baseline; `scripts/perf-smoke.sh` + `scripts/perf-smoke-check.py` + `perf-smoke.yml` (path-gated on storage/kernel/vector) + dag wiring pin; RED `perf-smoke-no-arm` (the pre-fix smoke runs green and produces no machine-readable artifact, exit 1) |
| P1-7 coverage floor on codec/replay | DONE (this commit) — `scripts/check-coverage-floor.sh` runs the storage-v2 suite under cargo-llvm-cov and fails any of checkpoint/snapshot/wal below its committed baseline (0.05%-point tolerance = report rounding only); committed baseline records the toolchain; `coverage-floor.yml` path-gated on storage + dag wiring pin; RED `coverage-floor-no-baseline` (suite green, no baseline to enforce, exit 1) |
| P1-8 dogfood MRFC-0070 on the review loop | DONE (this commit) — `scripts/dogfood-review-loop.py` (full/verify/trace) runs the repo's own plugin over MCP stdio: the 6 findings are Requirement KOs in `./kb`, the doc is the compiled knowledge document, the 13 re-stamp commits are reconciled via A8, and `trace_requirement` pins the requirement leg (`finding: R3-003` / `re-stamping`) with the tests leg documented as MRFC-0070 follow-up (markdown fact entities are mock tokens; `tested_by` objects are `crate`); the doc carries the compiled section emitted from kernel state (findings→KOID table, trace answers, `compiled-head`), `verify` fails on any drift, and the dag job pins the freshness (a doc change without a re-emit fails CI); RED `dogfood-loop-unwired` (no review-loop script, so no gate could fail, exit 1) |
| M47 CI fix round (R4-P2-06) | DONE (this commit) — the first pushed-head round's classes, root-caused: the Linux 30-min timeout was a csc002 park-leak hang (the park env is process-wide, a sibling's armed park parks an unguarded test's own merge, its marker is deleted by nobody — csc002/fsc002 now take the serial guard; this is also the M41 flush_lock_scope one-off); clippy 1.98 `drain_collect` → `std::mem::take` (whole-workspace 1.98.1 re-lint green); four env-hygiene pins (the gate had masked a second, older failure one check deeper: the shuffle-wiring gate's `[ -x ]` — the repo tracks every script 644, the exec bit is invisible on Windows and failed on every Linux run → `[ -f ]`); the coverage stall pin is enriched with the read-path stats, its verdict rides the re-run. The R4 dispositions themselves (M38–M47) are tracked in the plan docs (`docs/IMPLEMENTATION-PLAN-PHASE5.md` / `TESTING-PLAN-PHASE5.md`) — the 31-commit M47 push never re-stamped this doc, the drift the shallow PR merge-ref checkout masks on CI; this commit restores the stamp at the tip. The same shallow checkout hid a third masked failure one check deeper still: the dag job's P1-8 compiled-head grep died on `rev-parse DOC_COMMIT^` (the merge commit's parent is absent at depth 1) — the job now checks out with `fetch-depth: 0`, which also makes R3-005 itself meaningful on CI (verified in a local merge-ref repro: R3-005 stamp = tip = branch head, F8 compiled-head = tip's parent). Round 3: Test (Linux) surfaced two cell-pin harness races in consecutive rounds — the tiered twin's count-only probe read an undrained state (now waits for its compactor, the suite's own drained-state convention) and the batch sweep's `wall_ms > 0` asserted machine speed, not instrumentation (removed; the cell stays recorded). Neither was a product bug: rounds 2→3 touched only workflow/docs, and the same code flips green/red by runner speed. Round 4: Check + Test (Windows) died at its first test binary — the step lacks `shell: bash`, so pwsh collapsed `$(bash scripts/skip-list.sh)`'s one-line output into one `--skip` argument ("Unrecognized option"); the registry landed after the last green Windows run, and round 1's clippy masked it until now — `shell: bash` restores the word-splitting the command was authored for. Coverage-floor verdicts on the same runs: the stall pin PASSED on its enriched re-run (round-1 RED recorded as a flake), and the floor failed on snapshot.rs by 0.01 point — drift from the 09-20 baseline by two legitimate causes (rustc 1.97.1→1.98.1, plus the post-baseline M28/M33/M34 snapshot.rs commits) — re-based to the gate's own measured trio |
| Launch review (TDD-001..034) + CI/storage consolidation | FILED (this commit) — `docs/IMPLEMENTATION-PLAN-LAUNCH.md` + `TESTING-PLAN-LAUNCH.md`: the 34 TDD items dispositioned as L-01..L-23 (3 code P0s verified real: TDD-001 debug-only sorted precondition, TDD-004 memtable bytes inflation, TDD-015 unchecked WAL arithmetic); the CI redesign (storage-V2-only, three workflows, performance moat) as S-01..S-05 + CI-01..CI-09; L-00 (comment-aware skip-drift gate) fixed — main's dag job RED root-caused to the round-4 comment quoting `--skip` |
| Agent knowledge base | FILED (this commit) — root `AGENTS.md` (auto-discovered by agent harnesses) as the curated index over `docs/`: current-vs-historical classification, the active launch plans, repo conventions incl. this re-stamp ritual, crate/CI/layout maps, no content moved; `CLAUDE.md` is the three-line pointer to it |
| Rust coding skill | FILED (this commit) — `.claude/skills/rust-coding/SKILL.md`: repo-specific Rust guidance for coder agents — the classified-error idiom, correctness-first optimization (structural metrics, no debug_assert! preconditions, checked arithmetic, delta accounting), proven scalability patterns (bounded memory, backpressure, merged iterators, O(1) hot paths), concurrency rules (park hooks, process-wide env), storage-v2 invariants, and the verified trap ledger; `AGENTS.md` links it |

<!-- DOGFOOD-COMPILED-BEGIN -->
## Compiled from kernel state (P1-8 dogfood)

`scripts/dogfood-review-loop.py full` emits this section from the
project knowledge base (`./kb`, served by the repo's own aikoql-mcp
plugin; machine state lives in the `.state.json` sidecar). The
findings are Requirement KOs; the dispositions doc is the compiled
knowledge document; each re-stamp commit below is reconciled via
the A8 `reconcile` tool against that document.

- knowledge document KOID: `01a0d202df900000000000000000a9c9`
- reconciled re-stamp commits: 37 (first ea9aa77, last 7577e94)
- trace answers: `R3-003->finding: R3-003 | R3-005->re-stamping`

The trace pins the requirement leg (the finding is found by query).
The tests leg is empty by construction today — two extractor gaps the
dogfood itself surfaced: the markdown compiler attaches mock tokens,
not component names, as fact entities, and the code extractor's
`tested_by` objects are `crate`, which the tests-leg walk
(components/functions) cannot match. Closing those is MRFC-0070
follow-up, not this milestone.

| finding | KOID | disposition | status |
|---|---|---|---|
| R3-001 | 01a0bfccb7e50000000000000000a9c9 | NOT A FINDING | closed |
| R3-002 | 01a0bfccb8280000000000000000a9c9 | FIXED | closed |
| R3-003 | 01a0bfccb83f0000000000000000a9c9 | FIXED | closed |
| R3-004 | 01a0bfccb8710000000000000000a9c9 | COVERED | closed |
| R3-005 | 01a0bfccb8a40000000000000000a9c9 | FIXED | closed |
| R2-008 | 01a0bfccb8b70000000000000000a9c9 | CLOSED | closed |

compiled-head: 075443e7c824ad015b9a593f878fced5f3074b3d
<!-- DOGFOOD-COMPILED-END -->
