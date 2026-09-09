# AIKOQL Phase 3 — Testing Plan

Mirror of `docs/TESTING-PLAN.md` §13.2 for phase 3, same ledger discipline as `docs/TESTING-PLAN-V2.md`: one row per milestone; status flips to ✅ only with real evidence (test names + green counts + artifacts). Requirement numbering continues V2 (S52 was the v2 acceptance matrix — phase 3 starts at §53). TDD rules below are binding for every milestone.

| # | Milestone | §§ | Status | Evidence |
| --- | --- | --- | --- | --- |
| P3-M0 | Estate hygiene | — | ⬜ | clb001–002; `cargo test --workspace` green; zero artifact diffs on a full local run |
| P3-M1 | Security hardening | §53–55 | ✅ | auth001–008 + auth_surface 2/2 spawn (exit-2 fail-closed pins); `cargo test -p aikoql-mcp` 126/126 (mcp_stdio + connector_certification + auth_surface green); fmt + clippy `-D warnings` green; grep pin — admin/admin prefills+hints removed from graph_ui/studio, only auth001's negative assertion remains; hash-password CLI smoke `$argon2id$`; docker health endpoint allowlisted (CI-covered) |
| P3-M2 | Observability + StorageAdmin | §56–57 | ✅ | met001–003 3/3 (0.09s); met004–006 green in `cargo test -p aikoql-mcp` (74/74 main binary, 129 across binaries); `cargo test -p aikoql-storage-v2` 264/0; fmt + clippy `-D warnings` green; met007 (P3M2_ATTRIB=1, release) instrumentation share **0.02%** ≤1% (16 B put 832 µs/op, 1400 B 902 µs/op, marginal 194.6 ns/op; fsync_count 200000 pins one fsync/Sync-put) — artifact `artifacts/storage-engine-v2/write-stats-overhead.md` |
| P3-M3 | Engine-native snapshot/restore | §58–60 | ⬜ | bkp001–006; REC-002 conformance; 100K copy-time cell |
| P3-M4 | Compiler completion | §61–63 | ⬜ | cpl001–007; golden_snapshots + grammar_coverage + fuzz_parser green |
| P3-M5 | Constraint engine (a/b/c) | §64–66 | ⬜ | cst001–007 per sub-boundary; MRFC-0060 coverage table |
| P3-M6 | Scale-1M re-run | §67 | ⬜ | result-1m-aikoql-v2.json + workloads-1m-aikoql-v2.md; gate 5 ≤8×; identity divergence 0 |
| P3-M7 | Class-B async | §68–69 | ⬜ | cb001–005; MRFC-0011 §11 conformance green |
| P3-M8 | Background compaction | §70–71 | ⬜ | bgc001–005; CP-001..010 + relocation + crash windows green in background mode; W8 tail cell |
| P3-M9 | SDK & proxy decision | §72–73 | ⬜ | decision doc; kept-surface REDs (sdk001–002) or deletion commits + DAG green |

## Rules carried from v2 (binding) + phase-3 additions

1. **RED first.** Write the failing test, run it, verify it fails for the stated reason, then implement. No implementation before the RED is on the branch. Never weaken an assertion to make one green — weakening is a RED on the review.
2. **Correctness and performance separated.** Correctness pins run on every PR. Measurement cells are env-gated (`*_NIGHTLY`, `P3M2_ATTRIB`, `AIKOQL_REPORT_WRITE`) and reported, not asserted — except the standing asserts (gate 5 ≤8×, zero-loss, identity divergence 0). Env-set-but-dead = FAIL.
3. **One milestone = one commit** (`Co-Authored-By: Claude Code <noreply@anthropic.com>` trailer), NO push — the user pushes.
4. **Golden byte-pins python-first** for any new format surface (P3-M3's snapshot marker): compute the fixture in python before the Rust writer exists; a format change is a visible diff.
5. **Crash windows reuse the park harness.** Any new publication protocol (P3-M3 snapshot, P3-M7 job table, P3-M8 background merge) gets its windows covered with the existing child-kill pattern (`current_exe --exact` + env gates, KSE-15). New windows only where the protocol has new stages.
6. **Artifact discipline (P3-M0 first).** Report writes gate behind `AIKOQL_REPORT_WRITE=1`; a full local suite run must never diff committed artifacts. Nightly re-runs regenerate subsets without committing them.
7. **Security test hygiene.** No real secrets in fixtures; argon2 test vectors, not live hashes; no test binds a public interface; loopback enforcement tests bind loopback.
8. **Regressions.** Gate 5 ≤8× and the workload bounds (10/10/15% vs 09-05 baseline) hold wherever a milestone touches the engine. Suite counts recorded per milestone in this ledger's Evidence column.
9. **Honest ledger.** Anything descoped (M5c, M7b, SDK adoptions) gets a "PASS WITH ACCEPTED LIMITATIONS" closure citing evidence — the KSE/M41 pattern — never a silent drop.
10. **CI carry-over.** The existing skip list stays authoritative; new suites that are measurement-first join it or gate themselves. The dependency-DAG grep ban extends to the new deletion sweeps (clb002).

## Milestone gates (what flips a row to ✅)

- **P3-M0:** clb001 (report write gated), clb002 (no dangling references to deleted harnesses); full workspace suite green; zero artifact diffs.
- **P3-M1:** auth001–008 green; MCP stdio suite (90 tools) green; docker health + volume-restart green; connectors + dogfood green; no hardcoded credentials remain (grep pin: `admin/admin` absent from src). — **SHIPPED (commit 4583b68-p3m1):** auth001–008 unit green; auth_surface 2/2 spawn (remote-HTTP-without-credentials exit 2, metrics non-loopback exit 2); full crate 126/126 incl. mcp_stdio (auth007) + connector_certification; dogfood is stdio transport (untouched, auth007 pins it); grep pin: UI prefills/hints removed, remaining `admin`+`password` adjacency in src is auth001's rejection assertion (evidence of absence); docker health endpoint on the allowlist, compose change is an env pass-through (CI-covered).
- **P3-M2:** met001–006 green; met007 cell ≤1% overhead; `/metrics` series visible in a live scrape; `storage_compact` + oracle re-verify green.
- **P3-M3:** bkp001–006 green; REC-002 green; snapshot protocol crash windows covered; marker golden byte-pinned.
- **P3-M4:** cpl001–007 green; golden snapshots updated; fuzz corpus re-run clean; kernel suites untouched (regression only).
- **P3-M5:** cst001–007 green per sub-boundary; MRFC-0060 coverage table committed; existing constraint + transact suites green.
- **P3-M6:** the 1M matrix runs to completion; result artifacts committed (via report gating); gate 5 assert green; identity divergence 0.
- **P3-M7:** cb001–005 green; MRFC-0011 §11 conformance green; restart durability covered by a child-kill window.
- **P3-M8:** bgc001–005 green; the full existing compaction battery green in background mode; W8 cell recorded.
- **P3-M9:** decision doc + evidence committed; kept surfaces have green REDs, deleted surfaces leave zero dangling references (DAG green).
