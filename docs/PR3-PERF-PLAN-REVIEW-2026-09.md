# PR3 performance-plan review — dispositions (2026-09)

Response to `AIKOQL_PR3_Rust_Performance_Optimization_Plan.md` (27 sections,
external senior review). That plan is pinned at PR3 head `6a0880b`; this
disposition verifies each item against the current head
(`043ac1b`, `feature/storage-enhancements-phase3`). Every claim below was
checked in the code at that head, not assumed.

Verdict up front: the plan is well-structured, but a large share of its P0
budget was shipped by later phases (P4/P5/SE2 work the plan predates), and
two of its remaining items have engineering costs the plan does not notice
(§3 needs a key-shape change, not a Borrow impl; §2's sort is already
allocation-free). The genuinely open work is four small, deterministically
pinnable milestones, sequenced below.

## Disposition table

| plan § | item | disposition | evidence at 043ac1b |
|---|---|---|---|
| §2 P0 redundant flush sort | OPEN (downranked, see below) | the sort still exists — `segment.rs:244` `entries.sort_by(key asc, seq desc)` in `publish_with_anchors_staged`. It is in-place (zero allocations; `slice::sort_by` mutates the taken `Vec`). The flush input (`db.rs:1847`, `into_entries`) is BTreeMap order = key asc, **seq asc**; the segment wants seq **desc**, so the sort is doing real work — but it is O(n log n) work where an O(n) per-key-run reversal suffices. The compaction input (heap merge, `compaction.rs`) is already in final order — there the sort is pure re-validation. |
| §3 P0 memtable point-lookup allocation | OPEN (downranked, see below) | `memtable.rs:129/144/178` — `range((key.to_vec(), 0)..)` on `BTreeMap<(Vec<u8>, u64), _>` allocates the owned range start on every `get`/`get_by_rid`/`prefix_heads`. Verified by compiling a probe: std has **no** `Borrow<([u8], u64)> for (Vec<u8>, u64)` (tuple-composite Borrow does not exist), so no `&[u8]`-range variant compiles against the flat key. |
| §4 P0 O(n) LRU | DONE — plan stale here | the `VecDeque`+`retain` LRU the plan describes is gone. P5-M25 (f87cda1, RED f189184) replaced it with the gen-stamp cache (`cache.rs`): hit path O(1) (hash + stamp), min-generation eviction on miss. |
| §5 P0 full-state copies on read | PARTIAL — mostly shipped | SE2-M10 (0e8519f): `State.segments` is `Arc<Vec<Arc<SegmentReader>>>`; a get clones the arc under the guard and probes lock-free — no disk I/O under the state lock (`db.rs:1447-1449`, `1602-1631`). Residual: (a) one `RwLock` read per get (measured by `lock_wait_ns`, SE2-M21), (b) `flush` publishes segments **while holding the write guard** (`flush_locked_impl` does the streamed disk write under `state: &mut State`), so readers stall during a flush window. |
| §6 P0 segment selection before disk/cache reads | DONE | SE2-M9: the `[key_min, key_max]` range skip fires before the bloom probe and any I/O (`db.rs:1644`, both `get_inner` and `get_many_inner`). |
| §7 P1 compaction heap allocations | OPEN | `compaction.rs:135-139` — `BinaryHeap<(Reverse<Vec<u8>>, u64, usize)>`; every advance clones the entry's key into the heap (`e.key.clone()`), one key-bytes copy per merged entry. |
| §8 P1 HashSet per compaction key run | OPEN | `compaction.rs:180` — `let mut grouped: HashSet<ReplicaId>` created **per key run** (inside the merge loop); plus `run: Vec<SegmentEntry>` at `:162` per key. One `HashSet` allocation + one `Vec` allocation per key, for keys that overwhelmingly have a single version. |
| §9 P1 SmallVec for short runs | MERGED into §8 | `smallvec` is not a dependency; the hoisted-Vec fix (§8) removes the allocation without a new dependency — a short-run `SmallVec` is the optimization the hoist already delivers. |
| §10 P1 WAL encode double-allocation | OPEN | `wal.rs:85-141` — `encode_frame` builds a `payload` `Vec` (grown from 0) and then a second `frame` `Vec` that copies it. Two+ allocations per frame. The `encoded_len`-then-one-buffer idiom already exists for identity/replica/placement records (`directory.rs`), so the fix is pattern reuse, not a new idea. |
| §11 P1 WAL replay streaming | PARTIAL | R3-003 (a70773f) streamed the **snapshot** path (`valid_prefix_len`, one frame in memory). Recovery still materializes: `db.rs:664` reads the WAL to `wal_bytes` then `replay_frames` (`wal.rs:240`) builds `Vec<WalFrame>` — the frames hold the op keys/values, so peak is ~2× the WAL size. |
| §12 P1 parallelize reads | DEFERRED | `get_many` already batches: one guard, one key hash per unique key, one block fetch per block. Adding worker threads to a ~µs hot path costs more than it wins; the plan's own §20 matrix has no cell that would currently fail. Revisit only if a scan-heavy benchmark shows a gap. |
| §13 P1 hot segment metadata | DEFERRED | SE2-M11 covers the memtable hot head (P50 1.2µs vs 20µs gate). Segment index/bloom pinning has no evidence of mattering (index fetch is one cached block); §20 matrix first. |
| §14 P2 adaptive block size | DEFERRED | YAGNI until a benchmark shows block-size sensitivity on this workload. |
| §15 P2 dense placement reads | DEFERRED | §45 evidence (419 B/object, M38) shows the current shape; no failing cell. |
| §16-19 discipline advice | NOTED | (§17 cache-line, §18 premature `Arc<Vec<T>>` cloning, §19 compaction strategy) — advisory; no code change claimed, none needed now. |
| §20-22 benchmark matrix + gates | MAPPED | see "Evidence" below. |
| §23/§27 sequencing | SUPERSEDED | the plan's "first implementation" (streaming flush + O(1) LRU + alloc-free lookup) is 2/3 already shipped; the sequence below replaces it. |

## The two downranks, stated precisely

The plan ranks §2 and §3 P0. At this head they are not:

**§2 (flush sort)** — flush is not the ack path (group commit acks on WAL
append; the sort runs inside the flush window), the sort is allocation-free,
and its cost is O(n log n) key comparisons against a streamed disk write +
fsync — roughly 10-20% of flush CPU for a 64 MiB table. The fix is cheap
and low-risk (below), but it is not P0. Optional milestone PERF-5.

**§3 (memtable lookup allocation)** — the plan's "preferred design"
(presumably a Borrow-style borrowed bound) does not compile: even with a
newtype key, `Borrow` must return a reference to an existing place, and a
`{ Vec<u8>, u64 }` newtype has no `([u8], u64)` place in memory (only the
two-level map gives a zero-allocation std range, via `Vec<u8>: Borrow<[u8]>`
on the outer map). Further: `get` must clone the value to return it
(`db.rs:1610`), so the pin is "exactly one allocation per memtable hit"
(the value clone), not zero — and memtable hits are only the
recently-written subset of reads; segment hits (the common case) are
unaffected. Optional milestone PERF-2, with the per-key inner-map memory
cost called out.

## Implementation plan

Four open milestones + two optional, one commit each, TDD-shaped. REDs are
deterministic allocation pins via a counting allocator — the repo already
has the pattern (`tests/block_v2_alloc.rs` installs a test-only
`#[global_allocator]`; each tests/ file is its own binary, so per-file
allocators are free). No timing-based pins (env-gated RSS cells like sfm009
are the fallback, not the plan).

Each milestone: RED → fix → gates (`cargo fmt --all`,
`cargo clippy -p aikoql-storage-v2 --all-targets --all-features -- -D warnings`,
workspace suite with genuine exit code, coverage floor untouched). None of
these touch `docs/PR6-TDD-DISPOSITIONS.md`, so the dogfood dag pin and the
R3-005 head check are unaffected.

| milestone | change | RED (fails before, passes after) |
|---|---|---|
| **PERF-1** WAL replay streaming on recovery | `wal.rs`: `replay_frames_streaming(bytes, &mut |frame| …)` — apply-during-scan; `db.rs:664` recovery switches to it. `replay_frames` stays (10+ test call sites + damage corpus use it as the oracle). | allocator pin: open a Db over a synthetic ~512 KiB WAL, assert peak live allocations during replay stay below the WAL byte size. Today recovery materializes frames ≈ the WAL again → RED. |
| **PERF-3** compaction per-key allocations | `compaction.rs`: heap items own the entry (the key moves into the heap — kills the per-entry `key.clone()`; `fronts` slot is `None` while popped, refilled by `advance`), and `run` + `grouped` are hoisted `Vec`s with `clear()` per key run (`grouped` linear-scan dedup, O(k²) with k = versions per key — tiny; `ponytail:` comment with the ceiling). | allocator pin: merge N single-version keys, assert steady allocations ≤ C×N. Today: heap key clone per entry + per-key `HashSet` + per-key `run` `Vec` ≈ 3×N → RED. |
| **PERF-4** WAL encode single buffer | `wal.rs`: `op_encoded_len` + one `Vec::with_capacity` built directly (the identity/replica/placement `encoded_len` idiom reused). | allocator pin: `encode_frame` on a mixed-op frame allocates exactly 1 block. Today ≥ 2 (payload + frame, plus payload growth) → RED. |
| **PERF-5** (optional) flush-order normalize | `segment.rs`: replace the unconditional sort with `normalize_entries` — one pass detects the input shape (already key-asc/seq-desc → no-op; key-asc/seq-asc → reverse each key run in place; anything else → fall back to the full sort). Both call sites keep their invariant checks (the `windows(2)` duplicate scan). | no timing RED (comparison counts aren't pinnable without wrapping the key type); GREEN equivalence pin per the R3-004 precedent: shuffled input produces the same order as `sort_by`, both pre-sorted shapes pass through unchanged. Evidence: flush wall on `scale.py --quick` before/after, reported in the commit message. |
| **PERF-2** (optional) two-level memtable | `memtable.rs`: `BTreeMap<Vec<u8>, BTreeMap<u64, MemEntry>>`; outer range over `&[u8]` (std `Borrow<[u8]>` exists) → zero-allocation lookup; `entries`/`into_entries`/`prefix_heads` become nested iterations (same emission order). Tradeoff stated in the commit: per-key inner-map node overhead for single-version keys, write path gains one inner-map allocation per key. | allocator pin: memtable `get` on a present key allocates exactly 1 block (the value clone) — today 2 (range start + value) → RED. |

Deferred (need the §20 matrix first, explicitly not planned now): §12
parallel reads, §13 hot segment metadata, §14 adaptive block size, §15 dense
placement reads, and the §5 residual (flush publishing outside the write
guard — a correctness-heavy change touching the SE2-M36 crash windows; the
reverse stall it solves — readers blocking during flush — has no measured
victim yet).

## Evidence (§20-22 mapping)

The plan's benchmark matrix maps onto the existing harness, not a new one:

- **Point reads**: perf-smoke cells W1/W2 (`perf-smoke.yml`, 3× budget vs
  committed baseline) — the cells PERF-2/PERF-3 claim to move.
- **Writes**: ack path = group commit, covered by the nightly matrix; the
  PERF-4 encode change is pinned by the allocator cell and the existing
  `wal_golden.rs` byte pins (the encoding is byte-identical by construction
  — the pin asserts it).
- **Recovery**: PERF-1's allocator pin is the memory evidence; a wall-clock
  recovery cell can be added to the smoke later if the pin shows headroom.
- **Compaction/scan**: no dedicated smoke cell exists today; the suite's
  memory gates (§45 directory accounting) plus PERF-3's pin are the
  evidence. Full 1M runs stay on CI (baseline-guard / benchmark-nightly) —
  laptop is `--quick` only, per standing directive.

## Filing

- This document: reviewed against `043ac1b`, re-stamp if the dispositions
  table's cited lines move.
- Implementation starts on go-ahead, PERF-1 → PERF-4 → PERF-3 in that order
  (smallest surface first), then the optionals on request.
