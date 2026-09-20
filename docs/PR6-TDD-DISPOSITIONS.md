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
| Architect review doc + TDD plan | FILED (this commit) |
| P0-1 RED archives as artifacts | next |
| P0-2 env-gate registry + drift sweep | planned |
| P0-3 deterministic damage corpus | planned |
