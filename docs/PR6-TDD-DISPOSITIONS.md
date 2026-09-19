# PR6 TDD review — dispositions (008 / 010 / 012)

Response to `AIKOQL_PR6_SENIOR_RUST_TDD_REVIEW.md`. Findings 001–007, 009,
and 011 were fixed or pinned in code (see commit history; each carries its
PR6 number). This file disposes of the three structural/documentation
findings.

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
