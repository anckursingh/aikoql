---
name: rust-coding
description: Rust coding rules for this repo — error classification, correctness-first optimization, scalability patterns, and the trap ledger of things that have actually bitten this codebase. Use when writing or reviewing any Rust under crates/ or benchmarks/.
---

# Rust coding — Mnemosyne/AikoQL

What makes agent-written Rust effective here: match the patterns this
codebase has already proven, and never repeat a trap on the ledger below.
Repo-level rules (TDD RED-first, one milestone = one commit set, the
re-stamp ritual, gate chain) live in `AGENTS.md` — this skill is the
Rust-specific layer.

## Error handling (the repo idiom)

- Hand-rolled classified error enums, not anyhow/thiserror — one enum per
  crate with failure classes, e.g. aikoql-v2's `FormatError`:
  `Corrupt` / `Unsupported` / `Io` / `Invalid` / `Locked` / `Stale`.
  Classify by *what happened*, not where: missing file = `Io`, bad checksum
  = `Corrupt`, newer format = `Unsupported`, caller misuse = `Invalid`.
- Never discard an error (`let _ = ...` is a review blocker); wrap with
  context at every layer boundary so the chain survives.
- Check errors immediately; no downstream logic before the check.
- RAII over manual cleanup: a `Drop` impl beats an unchecked `close()`
  path. If a handle must outlive its scope, prove it with a test.
- Fail closed at trust boundaries: an unknown-but-integrity-clean version
  is `Unsupported`, anything else `Corrupt` — never a best-effort parse.

## Correctness first, then optimization

- Every optimization claim needs a **structural metric**: allocations,
  bytes, decodes, blocks touched, RSS — wall time only as a baseline ratio
  with multiple samples and the machine string (artifact_schema.py
  enforces the §13 schema; an unlabeled number is uncommittable).
- No absolute timing pins on shared runners; no sleep-based
  synchronization — the park hooks are the interleave mechanism.
- `debug_assert!` never guards a *correctness* precondition (TDD-001: the
  sorted-publish precondition must be a runtime `Err(Invalid)` in both
  profiles — a release build publishes silently otherwise).
- Checked arithmetic on anything derived from payload/capacity sizes
  (TDD-015); byte accounting must adjust by deltas on replacement, never
  count the stale entry (TDD-004).
- Stats go through atomics (WritePathStats pattern, ~0.02% overhead);
  zero-cost when disabled.

## Scalability patterns proven in this codebase

- **Bounded memory everywhere**: streamed publish, streaming checkpoint,
  chunked WAL copy (64 KiB), bounded recovery — nothing reads a WAL or a
  segment wholesale. Memory is O(largest frame), never O(input).
- **Backpressure**: the compactor's 256 MiB hard bound — writers block
  rather than grow unbounded.
- **Merged iteration**: k-way heap over segment iterators; never collect
  then sort.
- **O(1) hot paths**: gen-stamp LRU hit path, L0/L1 index lookup,
  one-step `predecessor()` seek — a scan is the fallback, not the default.
- **Bounded hash maps everywhere**: caps with survivor-set semantics
  (cache, dedup with the crossover constant) — and `HashMap` iteration is
  random, so anything published in hash order diverges (pgen/cp009).
- One publication funnel: write-temp → fsync → rename; crash injection
  hooks belong in that funnel, nowhere else.

## Concurrency

- One `Arc<Kernel>` per store; never an Arc cycle (coordinator→host edges
  are `Weak` — the M18 leak). `Drop` must join owned threads.
- `MutexGuard` blocks disjoint field borrows — destructure `&mut *state`
  to borrow fields separately.
- Park hooks (env-armed, marker-file consumed per checkpoint), not sleeps,
  for interleave tests; park env is **process-wide** — an armed park parks
  a sibling test's own operation, so park-taking tests take the serial
  guard.
- Tests mutate process state through per-child spawns with clean envs —
  a `set_var` leaks into parallel siblings and children (the
  AIKOQL_BACKEND leak). Seeded RNG only.

## Storage-v2 invariants

Segments are sorted by (key, seq); duplicates fail closed; tombstones are
flags, not absences; reader headers must agree with the manifest; every
checkpoint advances floors (lid/rid/pgen) monotonically; a publish is
atomic or absent.

## Trap ledger (verified — do not re-derive)

| trap | fix |
|---|---|
| pipe exit codes mask cargo's | check `PIPESTATUS[0]` |
| `BTreeMap` panics on inverted ranges | guard the range direction |
| `HashMap` iteration order random | sort by rid before publishing |
| statics don't drop on MSVC | TLS sweepers for temp state |
| Windows `remove_dir_all` on missing dir / on a file (os 2 / 267) | tolerate both |
| append-mode handles can't `SetEndOfFile` on Windows | reopen for truncate |
| child processes inherit crate-dir CWD | spawn with explicit CWD |
| reader ids can be reused by the allocator | never reuse them |
| HNSW search scores are SIMILARITY, not distance | mind the comparison direction |
| name-matched perf samplers alias concurrent processes | unique marker names |
| llvm-cov filter takes the bare test name; profraw lands in CWD | sweep after runs |
| PowerShell Get/Set-Content mangles UTF-8 | file edits via proper tools, or `-Encoding utf8` |
| tests asserting segment state race the compactor | `wait_compactor_idle` first |

## Before declaring done

1. `cargo fmt --check` and `cargo clippy --workspace -- -D warnings`.
2. The targeted test suite (`cargo test -p <crate>`), then the full
   workspace only if the change is cross-crate — a workspace run is
   ~50 min; it rides CI when possible.
3. The gate chain: `bash scripts/check-*.sh` — all of them.
4. If `docs/PR6-TDD-DISPOSITIONS.md` changed at all: the dogfood
   re-emit + re-stamp commit as the tip (the ritual in `AGENTS.md`).
5. Never push — the user pushes.
