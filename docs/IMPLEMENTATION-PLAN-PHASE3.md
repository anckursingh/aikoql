# AIKOQL Phase 3 — Implementation Plan

Source: architect review 2026-09-09 (storage / kernel / integration / testing surveys; see `docs/TESTING-PLAN-PHASE3.md` for the evidence ledger). Branch `feature/phase3-enhancements` cut fresh from main (post-PR#5 dbd8db4). Commit per milestone, NO push (user pushes). TDD loop per milestone: PoV → RED (fail for the stated reason) → root-cause GREEN → regression → gates (`cargo fmt --all` + `cargo clippy --all-targets --all-features -- -D warnings`).

## Coder point of view (before implementation)

**Verdict: the phase is P0-security + P1-observability, in that order; everything else sequences off them.** The storage engine is crash-matrix-certified and operationally sound; the serving surface is a shipped product with a default-insecure HTTP listener (`http.rs:413` hardcoded `admin/admin`, time+pid session tokens, `http.rs:426`; 11 of 79 REST arms skip `need_auth()`; `validate_listen` guards only MCP TCP, not HTTP/metrics — `main.rs:429`). That is a defect in a released artifact, not a feature request. M1 ships second, after estate hygiene, and nothing else blocks it.

**Senior challenges (each verified against code on 2026-09-09):**

1. **Observability precedes background compaction — evidence the unmeasured is the gate-4 mistake again.** V2's design deferred background compaction "until measurements justify"; the measurements do not exist (`db.rs` exposes `ReadPathStats`, `CacheStats`, `CompactStats`, `fsync_count()` — no write-path counters, no fsync-latency histogram, no backlog gauge, despite design §21 listing them). M2 lands first and is small; M8 then has a trigger signal and a before/after cell instead of a vibes decision.
2. **Security is never lazy.** Session tokens become 256-bit random server-side secrets (no time+pid derivation); password hashing = `argon2` (new dependency, justified — this is a security path, the ladder's exception); loopback enforcement mirrors the MCP TCP fail-closed at bind time, with an explicit `allow_remote_http` opt-out that refuses to start without configured credentials. No secrets in fixtures, no test binds public interfaces.
3. **The snapshot protocol reuses the publication pattern, not a new one.** M3's manifest-pinning snapshot is stage → verify → publish with a marker file, the same shape as M40's checkpoint protocol — which means the same child-kill park harness covers its crash windows for free. New format surface (the snapshot marker) gets a python-generated golden **before** Rust, per the M0 discipline.
4. **Constraint engine: certify-with-limitations at every sub-boundary, KSE-style.** MRFC-0060 is a large HLD; building it whole is how half-built machinery ships. M5a (modes + severity wiring) → evidence → M5b (cross-object/temporal classes) → evidence → M5c (incremental evaluation) or an accepted-limitations closure. The closure pattern already exists (KSE, M41) and is this codebase's strongest move.
5. **The compiler gaps are one small independent milestone.** INGEST-rejected-in-lowering, TRAVERSE depth fixed at 1, projection skipped after TRAVERSE (`parser/mod.rs:166,282,290`) — three tight fixes, no kernel changes, ships standalone. INGEST lowers to an `IngestOp` dispatched to the existing ingestion pipeline (no new ingestion features — scope guard: the MCP `document_ingest` path stays the blessed one, the language op delegates to it).
6. **The 1M re-run is a half-day, and the harness is dirty.** M28 left `kse_m7_v2_workloads.rs` modified + 3 untracked artifacts and the matrix itself stale (pre-M38). M6 starts by reconciling that state, then runs the fresh 4-backend chain v1→v2→redb→memory at 1M with the post-M38 writer. It also needs P3-M0's artifact gating first (the run rewrites committed reports).
7. **Background compaction is the only default-flip on a shipped engine — protect it.** Background by default, synchronous on config (`compact_background = false` for deterministic tests), backpressure when the backlog gauge crosses a hard bound. All existing compaction suites (CP-001..010, relocation, crash windows) must run green in background mode — the park points are mode-agnostic, so the crash matrix is reusable as-is.
8. **SDK/proxy: delete beats maintain.** Unversioned half-clients outside the workspace (TS, Go), a 0.1.0-vs-0.1.19 Python wheel, and a 306-line untested, unpackaged shard proxy are liability, not feature. M9 is decision-first: a one-page decision doc with the delete-first recommendation; only kept surfaces get TDD. Federation stays NOT_IMPLEMENTED — the identity/placement directory is the seam, and no multi-node demand has appeared.

**Build order & dependencies:** M0 → M1 → M2 → {M3, M4, M5a, M6, M7a} → M8 (needs M2) → M9. M6 needs M0 (artifact gating). M5b/M5c and M7b are gated continuations, not separate milestones.

**Non-goals (honest ledger — each with a reopen gate):**

| Non-goal | Reason | Reopen when |
| --- | --- | --- |
| Federation / replication (§13/§14) | Wave-5 build-vs-buy; no multi-node demand | >1 deployment asks for multi-node; seam = identity directory |
| Bytecode/VM + cost-based optimizer | IR interpreter is adequate; CBO post-1.0 per planner.rs:8 | query perf gates fail |
| L2+ compaction / WAL segmentation | single-node embedded; design deferred pending measurements | scale cells justify |
| Compression | header byte reserved; dataset is text; no evidence | disk-footprint gate fails |
| Paged placement directory (§47) | 419 B/object = fine at 1M; unbounded at 10M | >5M-object deployment exists |
| Read replicas / Raft | federation non-goal | federation reopens |

## Milestones

### P3-M0 — Estate hygiene (test infrastructure)

Deliver: delete the dead harnesses, gate artifact writes. `tests/universal_test_harness.py` (referenced by nothing) deleted; `tests/common/` (empty) deleted; `benchmarks/tests/load_test.rs` (references non-existent `aikoql-scheduler` crate — compile-fail if re-enabled) deleted; `tests/e2e/` Playwright spec deleted with a ledger row (no CI job ever ran it; re-add with CI when studio UI work resumes). Report-writing suites gate their `artifacts/` writes behind `AIKOQL_REPORT_WRITE=1` (same pattern as the NIGHTLY envs); correctness asserts stay unconditional. `kse_m7_v2_workloads.rs` dirty state (M28) is NOT touched here — that is M6's first step.

Acceptance: `cargo test --workspace` compiles and runs with zero dangling references; a full local suite run produces no diff in committed artifacts; dependency-DAG CI job still green; deletions recorded in the testing-plan ledger.

TDD REDs (M0): clb001 artifact-gating — `AIKOQL_REPORT_WRITE` unset → report file unchanged after suite run (pre-existing content hash equal), set → rewritten; clb002 a deleted-harness sweep — repo contains no reference to the four deleted paths (grep pin in the DAG job).

### P3-M1 — Serving-surface security hardening (§53–55)

Deliver: `[auth]` section in `aikoql.toml` (users, roles, argon2 password hashes — no hardcoded creds anywhere); login issues a 256-bit random session token stored server-side with expiry (replaces time+pid, `http.rs:426-433`); `validate_listen` extended to HTTP/metrics listeners — loopback-only default, `allow_remote_http = true` refused without configured auth (`main.rs:429`); the 11 unauth REST arms fixed — everything except the pinned allowlist (health, metrics, openapi.json, abi-version, login) requires auth; `/api/graph` runs with the session's real subject, never a hardcoded admin (`http.rs:78-82`); docker-compose + plugin docs updated (9091 stays loopback-published locally).

Acceptance: 90-tool MCP stdio smoke unaffected; docker health + volume-restart (DEP-003) green; connector cert + dogfood green; auth matrix suites green.

TDD REDs (M1): auth001 login with configured creds 200, wrong creds 401; auth002 session token format pin (256-bit, not derivable from pid+time — token regeneration across restarts with same pid impossible to collide); auth003 token expiry rejects after TTL; auth004 `allow_remote_http` without auth → startup fail-closed; auth005 route matrix — every non-allowlist route 401s unauthenticated; auth006 `/api/graph` subject = session user (audit trail pin); auth007 stdio MCP trust-the-process unchanged (existing mcp_stdio suite as regression); auth008 rate limiter still per-principal post-auth (regression).

### P3-M2 — Observability + StorageAdmin surface (§56–57)

Deliver: v2 write-path instrumentation implementing design §21's list — `WritePathStats` (wal_bytes, segment_bytes, flush_count, flush_latency, fsync_latency histogram buckets, compaction_backlog_bytes, compaction_pending_segments, checkpoint_count/latency, group-commit write_queue_depth + batch stats), atomics only, zero-alloc on the hot path; `Db::stats()` extended; kernel adapter exposes `StorageAdminApi { stats(), compact(), checkpoint_now() }` (design §22, never shipped); MCP tools `storage_stats`, `storage_compact`, `storage_checkpoint` behind the operator role (`authz.rs` capability table); `/metrics` Prometheus output gains the new gauges.

Acceptance: met001–006 green; nightly cell met007 shows ≤1% overhead on the M7 matrix.

TDD REDs (M2): met001 N writes move wal_bytes and flush_count exactly (deterministic pin); met002 Sync mode records an fsync_latency bucket entry; met003 backlog gauge = Σ uncompacted L0 bytes with the trigger unsatisfied; met004 tool list contains the 3 new tools, role gate matrix (operator ok, developer/auditor denied); met005 `/metrics` contains the new series names; met006 `storage_compact` from MCP returns CompactStats and the oracle re-verifies the db afterwards; met007 (env `P3M2_ATTRIB=1`) overhead cell.

### P3-M3 — Engine-native snapshot/restore (§58–60)

Deliver: design §18 manifest-pinning snapshot — `Db::snapshot_to(dir)`: pin manifest generation → copy CURRENT + MANIFEST-{gen} + the segments it references + identity/replica/placement logs ≤ gen + WAL (torn-safe) → verify (all sha256-8) → publish marker `SNAPSHOT-{gen}` (new format surface → python golden first); `restore_from(dir)`: open pinned copy, verify, fail closed on any damage; crash windows via the existing park harness (stage → verify → publish has the same three windows as the checkpoint protocol); MCP `backup`/`restore` route v2 backends to the engine-native path, redb/v1 keep the trait-default scan (REC-002 untouched).

Acceptance: bkp001–006 green; REC-002 conformance green; snapshot of 100K takes O(disk) file copies, no full decode (cell).

TDD REDs (M3): bkp001 snapshot → restore into a fresh dir → full key-value walk byte-equals the live db (oracle); bkp002 concurrent-writer stress — snapshots during writes restore to a consistent pinned generation (never a mix); bkp003 child-kill at each snapshot window → no partial snapshot visible, incomplete dirs ignored on restore; bkp004 byte-flip in a copied segment → restore fails closed; bkp005 MCP backup on v2 produces the engine-native layout (marker present, no redb file), v1/redb path unchanged; bkp006 golden byte-pin of the marker file.

### P3-M4 — Compiler completion: INGEST, TRAVERSE depth, projection (§61–63)

Deliver: INGEST lowers to `IngestOp { artifact_ref }` in KIR (replaces the compile-time rejection, `parser/mod.rs:166`), runtime dispatches it to the existing ingestion pipeline; `TRAVERSE <rel> [DEPTH n]` (default 1, `parser/mod.rs:282`) lowering to repeated set-traverse; projection applied after TRAVERSE (`parser/mod.rs:290` — remove the skip, keep the ponytail note as a record of what was missing).

Acceptance: cpl001–007 green; golden_snapshots + grammar_coverage + fuzz_parser extended; kernel suites untouched (no kernel change).

TDD REDs (M4): cpl001 INGEST statement → IrPlan with IngestOp, no rejection; cpl002 `TRAVERSE rel DEPTH 3` returns the 3-hop closure on a diamond fixture; cpl003 absent DEPTH = depth 1 (existing behavior pinned); cpl004 `TRAVERSE rel RETURN x.y` projects the fields; cpl005 DEPTH 0 / negative → semantic error; cpl006 JSON frontend parity (depth in scan/traverse); cpl007 golden snapshot + grammar-coverage rows updated.

### P3-M5 — Constraint engine: modes, severity, cross-object/temporal (§64–66)

Deliver (sub-boundaries, closure at each): **M5a** — enforcement modes ENFORCED/VALIDATED/ADVISORY/DISABLED per constraint (MRFC-0060 §30), severity machinery wired to ViolationEvent records + a diagnostics surface; **M5b** — cross-object/cross-type constraint classes + relationship cardinality constraints; **M5c** — temporal constraints + incremental evaluation with a dependency graph. M5c builds only if M5b ships clean; otherwise a KSE-style "PASS WITH ACCEPTED LIMITATIONS" closure cites the evidence.

Acceptance: cst001–007 green per sub-boundary; MRFC-0060 coverage table in the milestone doc (implemented vs descoped, evidence per §); all existing constraint/transact suites green.

TDD REDs (M5): cst001 mode matrix — each mode's violation behavior (block commit / record violation / log only / skip) pinned; cst002 severity → ViolationEvent catalog; cst003 cardinality constraint (KO exceeding N rels violates); cst004 cross-type constraint; cst005 temporal window constraint; cst006 incremental re-eval counter pin (only affected objects touched); cst007 DISABLED = zero-overhead path (no evaluator invoked).

### P3-M6 — Scale-1M re-run (M28 un-park) (§67)

Deliver: reconcile the M28 dirty state (modified `kse_m7_v2_workloads.rs` + 3 untracked artifacts) — keep the harness edits if sound, drop if superseded; run the fresh 4-backend chain v1→v2→redb→memory at 1M with the post-M38 writer (`V2ADOPT_NIGHTLY=1m`, release build, sequential, TMP on C:); commit `result-1m-aikoql-v2.json` + `workloads-1m-aikoql-v2.md` + the gate-5 verdict + identity-divergence zero + RSS/amplification cells.

Acceptance: gate 5 ≤8× asserted in the matrix; identity divergence 0 across the run; report artifact committed via the M0 report gating.

TDD REDs (M6): m28-1m matrix invariants (zero-loss, per-backend parity, gate-5 bound assert) — the harness exists; the milestone's RED work is the reconciliation diff, evidenced before the run.

### P3-M7 — Class-B async (MRFC-0011 §68–69)

Deliver (sub-boundaries): **M7a** — `reason` becomes an async JobHandle: persisted minimal job table (status, input-hash, result ref), admission control (max concurrent), `JOB_REJECTED`, poll/status tool, claim-commit wiring on approval (Class-B claim → Class-A commit); **M7b** — `infer`/`predict` on the same machinery (no-op AiProvider still legal).

Acceptance: cb001–005 green; MRFC-0011 §11 conformance suite green; kernel `reason` callers migrated without behavior change (or explicit breaking note).

TDD REDs (M7): cb001 reason returns a job handle, status progresses → completed, result retrievable; cb002 over the admission limit → `JOB_REJECTED`; cb003 approval commits the Class-B claim with the correct epistemic transition; cb004 child-kill between accept and complete → job re-runs or is marked failed on reopen, never silently dropped; cb005 idempotent submit — same input hash returns the same job.

### P3-M8 — Background compaction + backpressure (§70–71)

Deliver: `maybe_compact` moves off the write path onto a compactor thread (the committer-thread pattern, `db.rs:584`); trigger = the existing l0 gates evaluated by the compactor against the M2 backlog gauge; hard-bound backpressure — when backlog crosses the bound, writes block until it drains; `compact_background = true` default, `false` forces the synchronous path (deterministic tests); all existing compaction suites (CP-001..010, relocation, crash windows) run green in background mode.

Acceptance: bgc001–005 green; W8 write-tail cell improved vs the synchronous baseline (nightly, reported not asserted); zero-diff end state vs synchronous compaction.

TDD REDs (M8): bgc001 a write crossing the trigger returns without waiting on the merge (timing pin, env-gated); bgc002 background end state byte-equals synchronous compaction (oracle); bgc003 backlog > hard bound → writes block (pin); bgc004 child-kill during a background merge (reuse FAIL_AFTER windows) → reopen consistent; bgc005 randomized interleave of flush+compact vs oracle (zero lost keys).

### P3-M9 — SDK & proxy decision (§72–73)

Deliver: decision doc first (one page): TS/Go SDKs → delete (outside workspace, unversioned, MCP is the blessed surface) or adopt (workspace, CI, versioned releases); Python → version bump to 0.1.x parity + PyPI publish job (recommended) or pin-and-document; cluster proxy → delete (no tests, no CI, no packaging; no federation demand) or adopt. Only kept surfaces get TDD. Deletions land as one commit with zero dangling references (DAG job as the regression).

Acceptance: decision doc committed with evidence; kept-surface REDs green; deleted surfaces leave the ledger rows.

TDD REDs (M9): sdk001 (python kept) contract tests vs the real MCP binary asserting workspace version parity; sdk002 (proxy kept) hash-routing correctness + a CI job + packaging — these REDs exist only if adoption wins over the delete-first recommendation.

## Gates (every milestone)

`cargo fmt --all` clean; `cargo clippy --all-targets --all-features -- -D warnings` clean; workspace test suite green with the CI skip list; suite counts recorded in the testing-plan ledger; no committed artifact rewritten without `AIKOQL_REPORT_WRITE=1`; gate 5 ≤8× and workload regression bounds (10/10/15% vs the 09-05 baseline) hold wherever the milestone touches the engine.
