# AIKOQL Phase 5 — Testing Plan (Database 1.0)

Mirror discipline of `docs/TESTING-PLAN-PHASE3.md`: one row per milestone; status flips to ✅ only with real evidence (test names + green counts + artifacts). Requirement numbering: phase 3 ended at §73 — phase 5 sections get numbered §74+ when the architect spec is written; the roadmap's ND-xx ids are the working ids until then. Companion: `docs/IMPLEMENTATION-PLAN-PHASE5.md`.

## Rules carried from phase 3 (binding) + phase-5 amendments

1. **RED first.** Write the failing test, run it, verify it fails for the stated reason, then implement. No implementation before the RED is on the branch. Never weaken an assertion to make one green — weakening is a RED on the review.
2. **Correctness and performance separated.** Correctness pins run on every PR. Measurement cells are env-gated and reported, not asserted — except the standing asserts (gate 5 ≤8×, zero-loss, identity divergence 0, **and now gate 7 memory bounds**). Env-set-but-dead = FAIL.
3. **One milestone = one commit** (`Co-Authored-By: Claude Code <noreply@anthropic.com>` trailer), NO push — the user pushes.
4. **Golden byte-pins python-first** for any new format surface (M3's plan serialization): compute the fixture in python before the Rust writer exists; a format change is a visible diff.
5. **Crash windows reuse the park harness.** New publication/commit protocols (M10's transaction commit, M11's server lifecycle) get their windows covered with the existing child-kill pattern. New windows only where the protocol has new stages.
6. **Artifact discipline.** Report writes gate behind `AIKOQL_REPORT_WRITE=1`; a full local suite run must never diff committed artifacts.
7. **Security test hygiene.** No real secrets in fixtures; loopback-only test binds. Phase-5 extension: the encryption-parity REDs (st009 + one per M5/M6/M13 milestone) run against an encrypted database with a test passphrase.
8. **Regressions.** Gate 5 ≤8× and the workload bounds hold wherever a milestone touches the engine; **gates 6–9 apply as defined below**. Suite counts recorded per milestone in the ledger.
9. **Honest ledger.** Anything descoped (M7's non-transactional catalog entities, M8's temporal/provenance index types, M10's READ COMMITTED) gets a "PASS WITH ACCEPTED LIMITATIONS" closure citing evidence — never a silent drop.
10. **CI carry-over.** The existing skip list stays authoritative; new suites that are measurement-first join it or gate themselves. The dependency-DAG grep ban extends to the logical-layer boundary pin (qm004: no `aikoql-v2` imports in the logical plan layer).
11. **Evidence-gated work.** No batch/index optimization ships without a cell showing a real gain over the scalar path at current scale; every CBO strategy change (M9) ships with an oracle run (gate 6) or a closure row — SE2-M25 falsified naive `get_many` at 100K warm, the same discipline applies to index batch APIs (M8).

**Phase-5 addition to rule 8 — the plan-equivalence oracle (M0):** every milestone M3+ whose RED list touches the planner must include an oracle row — optimized plan vs unoptimized plan over the seeded differential corpus, divergence = 0. The oracle is delivered by M0 so later milestones consume it.

## Evidence ledger

| # | Milestone | ND | Status | Evidence (to fill) |
| --- | --- | --- | --- | --- |
| P5-M0 | Baseline certification guard | ND-00 | ✅ | gd001 pin RED→GREEN (dependency-dag requires baseline-guard.yml + the 4 protected paths + `V2ADOPT_NIGHTLY=1m`); baseline-guard.yml ships (windows-latest, path-triggered, 6h, 1m v2 leg); gd002 RED = E0432 on missing `plan_oracle` → module shipped; gd002/gd002b/gd003/gd003b 4/4; gate5-check.py on committed artifacts: W1 7.96× REDLINE, W2 5.81×, PASS; runtime 21+2+4 green; clippy -D warnings green. Pending push: the actual CI 1M run (not executable in-session) — first-PR evidence lands with it. |
| P5-M1 | Planner semantics remainder | ND-01 | ✅ | alg001 doc pin RED→GREEN (operator-algebra.md ships: 3 rewrites × preconditions/proof/pinned-by); alg002 compiler proptest 3/3 seeded — RED from injected tenant-drop regression (2 properties fail, restore green); alg002 runtime oracle proptest 256 cases divergence 0 — RED from injected merge-drop regression (caught in 9 cases); compiler 151/0 + runtime 28/0; clippy -D warnings green |
| P5-M2 | KOQL v1 remainder | ND-02 | ✅ | kq001–012 40/40 RED→GREEN (RED = 26 compile errors on the missing surface); compiler 201/0 (84 lib + golden 16 + grammar 49 + kq 40 + fuzz 4 + doc pin + proptest 3 + 4 other); full workspace green exit 0 (kernel, v2, runtime, mcp untouched-green); fmt + clippy -D warnings green |
| P5-M3 | Logical/physical model | ND-03 | ⬜ | qm001–004; EXPLAIN shows logical + physical; plan serialization byte-pinned; compiler baseline 146/0 + additions green |
| P5-M4 | Streaming/batch execution | ND-04 | ⬜ | st001–009; gate-7 RSS cell; W1/W2 cells re-run (gate 5); encrypted-DB parity |
| P5-M5 | Aggregation + sorting | ND-05 | ⬜ | ag001–008; spill cell recorded; AS OF + authorization pins |
| P5-M6 | Join engine | ND-06 | ⬜ | jn001–009; tenant fail-closed pin; oracle vs nested-loop |
| P5-M7 | Database catalog | ND-07→(ND-09) | ⬜ | ct001–006; restart/corruption/migration pins; honest-ledger row for non-transactional entities |
| P5-M8 | Unified index subsystem | ND-08→(ND-07) | ⬜ | idx2-001..010; vec001/002 untouched-green; async-maintainer pin; batch-API evidence row |
| P5-M9 | Statistics + CBO | ND-08 | ⬜ | cbo001–008; gate-6 oracle green over every CBO change |
| P5-M10 | Transaction contract | ND-10 | ⬜ | tx001–007; child-kill commit window; SNAPSHOT-only honest-ledger row |
| P5-M11 | Standalone server | ND-11 | ⬜ | sv001–010; KOQL protocol adapter; graceful shutdown + crash windows |
| P5-M12 | CLI + SDK | ND-12 | ⬜ | cl01–003; error-code table pin; SDK version-mismatch fail-fast |
| P5-M13 | Hybrid certification | ND-13 | ⬜ | h1–h6; hand-computed oracle per modality; EXPLAIN shows every modality |
| P5-M14 | Database certification | ND-14 | ⬜ | cert001–003; DB-* artifacts in `docs/certification/` with run-date + commit hash |

## Milestone gates (what flips a row to ✅)

- **P5-M0:** gd001–003 green; the guard job runs on the four protected paths; corpus reproducible from clean checkout.
- **P5-M1:** alg001–002 green; proptest suite in CI; algebra doc committed and grep-pinned.
- **P5-M2:** kq001–012 green (incl. kq009 fail-closed on both compile paths, kq010 Int literals, kq011 serde round-trip); existing compiler suites extended green (golden 16/16, grammar_coverage 49/49, fuzz 4/4); MCP `aikoql` tool untouched-green.
- **P5-M3:** qm001–004 green; EXPLAIN shows both plans; serialization byte-pinned python-first.
- **P5-M4:** st001–009 green; gate-7 cell recorded; gate-5 1M re-run green; encrypted-DB parity.
- **P5-M5:** ag001–008 green; spill strategy documented + cell recorded; AS OF pins green.
- **P5-M6:** jn001–009 green; cross-tenant JOIN fails closed; oracle green.
- **P5-M7:** ct001–006 green; migration deterministic; non-transactional entities on the honest ledger.
- **P5-M8:** idx2-001..010 green; vec001/002 + text suites untouched-green; index commits off the critical path (pinned); batch APIs closed with evidence or a closure row.
- **P5-M9:** cbo001–008 green; gate-6 oracle green over every CBO change; statistics persisted + stale detection pinned.
- **P5-M10:** tx001–007 green; commit-crash window covered; SNAPSHOT-only documented; no async suspension inside critical locks (grep pin).
- **P5-M11:** sv001–010 green; KOQL adapter round-trip through the repo-built binary (dogfood); clean-shutdown zero-loss pin.
- **P5-M12:** cl001–003 green; error-code table committed; each new CLI verb dogfooded.
- **P5-M13:** h1–h6 correctness pins green; env-gated latency cells recorded; ACL sub-queries pinned.
- **P5-M14:** cert001–003 green; all five DB-* suites reproducible from clean checkout; artifacts committed.

## Honest-ledger template (pre-declared rows)

| Item | Milestone | Closure |
| --- | --- | --- |
| READ COMMITTED isolation | P5-M10 | SNAPSHOT only — kernel implements snapshot semantics; READ COMMITTED reopens on workload evidence |
| Non-transactional catalog entities (constraint/policy/model/user/role/tenant) | P5-M7 | Schema rows, not transactional writes — transactional when a milestone consumes them |
| Temporal/provenance index types | P5-M8 | Catalog schema-only until a workload profile shows the need |
| Index batch APIs | P5-M8 | Evidence-gated (rule 11) — SE2-M25 discipline; ship with a gain cell or a closure row |
| `aikoqld` / `aikoql` rename | P5-M11/12 | Ship 1.0 as `aikoql-mcp`; rename is a 1.0-cutover product decision with aliases (user-owned) |
| PG-wire protocol adapter | P5-M11 | Non-goal — reopens when a SQL-ecosystem deployment demands it |
