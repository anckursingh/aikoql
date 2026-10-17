# AikoQL Launch Test Plan

Sibling of `docs/IMPLEMENTATION-PLAN-LAUNCH.md`; phases S / CI / L match its
milestone ids. Two review inputs bind this plan: the TDD challenge
(TDD-001..034) and the CI optimization plan (§22 TDD, §23 acceptance).

## 1. Standing rules (both reviews, non-negotiable)

1. **Real RED first.** A RED is a failing assert or a measured counter that
   moves, not a vacuous placeholder. Where a RED cannot run deterministically
   (RSS peaks, timing), the env-gate + `ungated_by` home rules apply — and
   the cell says so.
2. **Do not weaken assertions to get GREEN.**
3. **Do not replace deterministic synchronization with sleeps** — the park
   hooks (SE2-M36) are the interleave mechanism for L-14.
4. **Do not turn correctness properties into timing thresholds** — no
   absolute wall-clock pins on shared runners.
5. **No performance claim without a structural metric or a reproducible
   benchmark** — allocs, decodes, bytes, RSS, block counts; timing only as
   baseline ratios with multiple samples.
6. Release-build legs: L-01's precondition must be tested in BOTH profiles
   (the RED is a panic in debug and a silent publish in release).
7. One milestone = one commit set (test RED → feat → docs); honest ledger;
   the re-stamp ritual (R3-005/F8) applies to any dispositions-doc change.

## 2. Phase S / CI — architecture testing (review 2 §22)

The review prescribes RED architecture tests BEFORE consolidation, written
as `scripts/check-architecture-hygiene.sh` with two legs:

**Storage leg (S-01, RED today):**
- v2 is the only active storage backend
- no active production code references Storage V1
- no deprecated storage crate is a workspace member
- no production dependency points to a deprecated backend
- the benchmark harness uses the current storage API

**Workflow leg (CI-01, RED today):**
- the required CI jobs exist (fmt/clippy/check/test + the gates)
- `benchmark.yml` exists (post-consolidation) and owns the 1M alone
- the competitor matrix exists
- the perf smoke remains wired (5 cells under the 3× budget)
- the release workflow remains wired (version gate + identity verification)

Each RED is captured via the P0-1 red-archive mechanism (worktree or
fake-tree, never the live repo); `check-red-archives.sh` validates the
captures. The script joins the dag job — the gate that pins the estate,
replacing the historical legs (CI-04).

**GREEN verification** (after each S/CI milestone):
- `cargo fmt --check` + `cargo clippy --workspace -- -D warnings`
- `cargo test --workspace` (Linux + Windows, skip-list derived from
  gated.toml)
- Storage V2 smoke: WAL/recovery, crash consistency, memtable, segments,
  codec, cache, compaction, identity, placement, snapshot, concurrency —
  the review's §3 list stays the required coverage set
- Regression validation (review §22): Docker, MCP, Python SDK, connectors,
  E2E restart/recovery, release build — each must still be covered by a
  named job after every consolidation step

**Acceptance checklist** = review 2 §23 (17 items), mapped one-to-one onto
S/CI milestone evidenc — no item closed without a named GREEN run.

## 3. Phase L — per-milestone mechanics

| milestone | RED source | GREEN evidence | env-gating |
|---|---|---|---|
| L-01 | unsorted feed: panic (debug) / silent publish (release) | `Err(Invalid)` both profiles; sorted callers green | — |
| L-02 | matrix unpinned | each duplicate placement fails publish; no segment visible | — |
| L-03 | bytes ≈ 3 entries after 3 same-(key,seq) puts | bytes ≈ 1 entry; delta correct on size change | — |
| L-04 | unpinned | 5-row matrix × flush/compact/checkpoint/reopen | — |
| L-05 | unpinned | randomized generic vs sorted equivalence (seeded, no unseeded RNG — P1-4) | — |
| L-06 | corpus restart legs missing | corruption classes → Corrupt/Unsupported; `debug_restart_metadata` vs independent decode | — |
| L-07 | survivor sets unpinned | cap 1/2/3 exact sets after every op; 100k churn invariants; test-only clock at u64::MAX−2 | churn leg nightly |
| L-08 | arithmetic unchecked; streaming legs missing | checked ops; boundary payloads fail safely; torn-tail vs corrupt on the streaming path | — |
| L-09 | unpinned | 5-row + tombstone/Drop/Archive/Retired winners | — |
| L-10 | unpinned | three-way agreement × flush/compact/checkpoint/reopen | — |
| L-11 | partial coverage | review's exact interleave windows × five layers; reserve/WAL-fail/restart semantics | crash legs follow the ci00x child-kill pattern |
| L-12 | unpinned | fixture matrix × five read paths; length round-trips | — |
| L-13 | unpinned | N readers, identical answers, no panic; parse count == 1 (instrumented) | — |
| L-14 | unpinned | park-hook interleave (no sleeps): CURRENT not advanced, T2 survives, staging cleaned, inputs remain; put completes under compaction I/O | — |
| L-15 | unpinned | shutdown × 6 states + checkpoint/close + GroupCommit/close | — |
| L-16 | one-batch claim | allocs constant vs op count (1..10k × size mix); VersionChain 10k-reverse-seq counters | benchmark/property, no thresholds |
| L-17 | unpinned | replay RSS sublinear in WAL size (10/100 MB); compaction alloc scaling + winners | 1 GB leg nightly |
| L-18 | unpinned | prefix-scan oracle + alloc counters; cache contention 1..64 readers | contention leg nightly |
| L-19 | unpinned | proptest oracles for memtable/segment/compaction/WAL (dev-dep, in-tree pattern) | nightly regression file |
| L-20 | 7 mutations uncaught | each mutation → captured RED archive (post-consolidation estate) | — |
| L-21 | duplicate names pass | duplicates fail; existing legs stay green | — |
| L-22 | wiring grep only | deliberate order dependency caught; seed + order recorded | — |
| L-23 | claims unevidenced | claim → evidence table; orphans become cells or lose the claim | audit runs in the dag job |
| L-24 | `*_us` baseline stale | committed v2 baseline refreshed; guard green | 1M rides the guard |
| L-25 | alerts/threads open; kernel write loss | disposition recorded per site; threads resolved; durable-by-default flush pinned by a RED | — |
| L-26/L-27 | LICENSE/CHANGELOG absent; pipeline unexercised | files present; release tag run green end-to-end incl. Tier-3 report | — |
| F-01 | block_target=0 opens + acks, dies at the first flush (product bug the matrix caught) | every knob × {0,1,b−1,b,b+1} opens, runs a fixed 16-op script, equals the byte-surface model, reopens, equals again — clean `Invalid` at open is the accepted fail-safe, a panic/overflow/silent divergence the RED | — |
| F-02 | mid-stream CloseReopen un-oracled (the other four props reopen only terminally) | state-machine proptest: Flush/Compact/Checkpoint/Reopen interleaved with data ops (24..80 cmds), model checked after EVERY command; pin-only — 256 debug + 4096 release, no regression file | PROPTEST_CASES=4096 nightly |
| F-03 | eleven §12 storage mutants unkilled | `storage-mutation-harness.sh`: each mutant surgically applied to a detached-worktree copy of HEAD, killed by its named regression — a survivor is a test-suite defect; all-mode exit 0 = all eleven fired | weekly storage-mutation job |
| F-04 | fuzz estate unpinned (removing coverage is undetected) | arch-gate workflow test 17: nightly proptest arm, the storage-mutation job + all-mode, the eleven ids in the harness, the F-02 state machine in proptest_oracles.rs; the job rides test 6's rust-cache spec | — |

## 4. Performance evidence rules (review 1 §30 + review 2 §5/§13)

- Cells report structural counters first: allocations, bytes, decodes,
  blocks touched, write/read amplification, RSS scaling. Wall time only as
  baseline ratio with multiple samples and the machine string.
- The perf smoke (post-CI-03) is 5 cells — W1 point lookup, W2 write
  throughput, W3 scan, W4 hot-cache lookup, W5 small compaction — under the
  committed 3× catastrophic-regression budget; < 5 min target on PR.
- Every benchmark result carries the §13 schema (commit, engine,
  engine_version, workload, dataset, configuration, throughput, p50/p95/p99,
  cpu_seconds, memory_mb, disk_mb) plus OS/CPU/RAM, dataset-generation
  version, harness SHA, random seed, cache state — enforced by
  `artifact_schema.py`, so an unlabeled number is uncommittable.
- Competitor runs are version-pinned (review §18) and never block PRs;
  semantic fairness (§11): no engine is forced into a workload it is not
  for.
- The coverage floor extends from the codec trio to the review §7 set
  (checkpoint, snapshot, WAL, segment codec, recovery) at one deliberate
  re-baseline, recorded with toolchain + machine.

## 5. Nightly legs (benchmark-nightly → benchmark.yml)

- Shuffle run + residue sweepers (P1-5) — with the L-22 seed/order
  recording so a caught failure reproduces.
- Gated cells whose `ungated_by = "none"` — weekly re-run (P0-2).
- 1 GB WAL replay (L-17), 100k cache churn (L-07), 10k-version chain
  (L-16), proptest regression file (L-19), contention ladder (L-18).
- Tier 2: 100K/1M self-regression + the competitor matrix + the hybrid
  knowledge workload (CI-07) with the report trio.
- Storage mutation harness (F-03): all eleven §12 mutants, weekly
  `storage-mutation` job (240-min budget, dispatched parallel to the
  benchmark job whose steps sit near the 360-min cap).

## 6. Launch certification (Tier 3, on the release tag)

Full correctness suite (Linux + Windows), full concurrency, coverage
floor, 1M+ cold/warm/concurrency/mixed/recovery/resource benchmarks,
competitor matrix, and the benchmark report artifact — the release
evidence pack that ships with the tag. VERIFY.md is the operator-facing
smoke; it runs from the built release artifacts, not the dev tree.

## 7. Phase D — SDK program mechanics

The review's §8 ladder is binding for every D-* capability — no SDK
capability is complete from a happy-path test alone:

```text
RED → minimal GREEN → SDK unit tests → shared conformance →
real-server integration → failure injection → fuzz → benchmark → release test
```

- **Contract gates (D-01..D-05)** are schema-checked like the §13
  result.json leg: `api-v1.json` / `errors.json` / `compatibility.json`
  validated by a checker that fails on drift, and the SDK
  `MIN_SERVER_VERSION` constants held against `compatibility.json` by
  `scripts/check-sdk-compat.sh` (D-03). Its own RED doubles as a drift
  mutation: bump the workspace version, leave the SDK constants — the
  check must fail.
- **Shared conformance (D-11)** lives in `tests/sdk-conformance/` with
  language-neutral vectors from `protocol/test-vectors/`; every SDK
  executes the same vector — expected results byte-identical where the
  contract says so, semantic-identical everywhere. The runner is the
  §23 canonical workload, not a curated subset.
- **Failure injection (D-16)** is one fault proxy in front of a real
  server — drop/delay/duplicate/reorder/truncate/corrupt/inject-notify/
  stale/close/half-close/slow/oversized, the §18 matrix — and every SDK
  must pass it. §19's resource-safety rules are enforced per target:
  max frame, max JSON nesting, max string/array, max chunks, max
  concurrent requests, max retries, max txn duration — a malicious
  server cannot cause unbounded client memory.
- **Fuzz layers (§10)**: L1 codec/parser, L2 protocol state machine
  (§17 — illegal transition sequences must error deterministically,
  never panic/deadlock/leak/bypass/phantom-commit), L3 semantic/property,
  L4 server fault. Go = native fuzz targets (§11 names), Python =
  hypothesis + the §12 stateful model (DISCONNECTED→CONNECTED→
  INITIALIZED→TRANSACTION→STREAMING→CLOSED), TS = fast-check, Java =
  Jazzer (JVM + native), Rust = cargo-fuzz (§15 names). The
  cross-language golden corpus (`sdk-fuzz-corpus/`) pins the §16
  invariant: same malformed input → same classification in all five
  languages, different idiomatic exception types allowed.
- **Mutation testing (D-17)**: the twelve §29 mutations killed per SDK
  under the F-03 pattern — named killer per mutant, survivor = missing
  test, every kill RED-archived.
- **CI (D-19)**: PR = unit + contract + integration + small fuzz +
  package smoke; nightly = long fuzz + state-machine + fault injection +
  cross-language corpus + pool/stream stress + large payloads; release =
  full conformance + full fuzz smoke + real server + all packages +
  platforms + benchmark + security.
- **Benchmarks (D-20)** report p50/p95/p99/throughput/allocs/CPU/RSS/
  wire bytes per tier and per operation; SDK latency is never presented
  as engine latency.
