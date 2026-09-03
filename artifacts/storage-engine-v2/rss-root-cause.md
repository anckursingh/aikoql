# Seed RSS amplification — root cause

Evidence-led follow-up to the SE2-M14 finding (DS-PERF-L seed peak
**16.21 GiB** vs a **2.12 GiB** dataset; adoption loader 496.93 → 1.35 GiB
vs the 09-01 nightly). No code changed in this pass — this is the root
cause with citations, reconciliation, and fix options.

## Verdict

The peak is not the memtable and not the WAL. It is
`SegmentWriter::publish()` (`segment.rs:126-323`), which assembles an
entire segment in RAM while holding **five simultaneous copies of the
data**, multiplied by SE2-M10's L0 trigger (`db.rs:373-389`), which runs a
KeepAll merge of **all** segments — and therefore publish of the whole
accumulated dataset — every time four flushes accumulate. At L the final
such merge publishes the full 2.12 GiB through the five-copy pipeline.

## The five-copy stack inside publish()

All alive at once (nothing is dropped until `publish_atomic` returns):

| # | buffer | site | size at L |
|---|---|---|---|
| 1 | `writer.entries: Vec<SegmentEntry>` — owned keys+values (merge pushes decoded entries, compaction.rs:116; flush clones from the memtable, db.rs:634-635) | segment.rs:167 | 32.0M × ~125 B ≈ **4.0 GiB** |
| 2 | `let mut entries = self.entries.clone();` — deep clone for the sort (publish takes `&self`, so the writer also *keeps* its copy afterwards) | segment.rs:135 | **4.0 GiB** |
| 3 | `blocks: Vec<Pending>` — every block's encoded payload | segment.rs:166 | **2.1 GiB** |
| 4 | `data_blocks` — each block re-wrapped in a fresh Vec (`p`, segment.rs:251) + block header | segment.rs:240 | **2.1 GiB** |
| 5 | `out` — the assembled whole-file buffer | segment.rs:224, 313-317 | **2.1 GiB** |

Plus small terms (bloom 40 MB, skeleton headers, readers' index+bloom
≈ 50 MB, active memtable+immutables). Predicted peak ≈ **14.4 GiB**,
measured **16.21 GiB** — the remainder is allocator overhead on 32M
entries and RSS sampling granularity. Per-entry real RAM is estimated
(Vec headers + Windows allocator slack); the estimate is ~125 B/entry
against ~71 B/entry accounted by the memtable.

After publish returns, `compact()` reads the just-published segment back
whole for the manifest checksum (`std::fs::read`, db.rs:740) — another
transient 2.1 GiB, and another reason the write path stays elevated.

## Reconciliation across all three measurements

| measurement | dataset | compactions | predicted peak | measured |
|---|---|---|---|---|
| DS-PERF-M (2.7M rows) | ~192 MB | none (3 flushes < trigger 4) | ~0.5-0.7 GiB | **469.93 MiB** ✓ |
| adoption 100K, 09-01 (pre-M10) | ~348 MB | none — compaction did not exist | ~0.5 GiB | **496.93 MiB** ✓ |
| adoption 100K, 09-03 (post-M10) | ~348 MB | yes — crosses 4 flushes, merges ≈ 256-348 MB | ~1.5-1.6 GiB | **1.35 GiB** ✓ |
| DS-PERF-L (32.0M rows) | 2.12 GiB | 33 flushes; merge sizes 256 MB → 2.12 GiB, quadratic | ~14.4 GiB | **16.21 GiB** ✓ |

The 496.93 → 1.35 GiB change between the 09-01 adoption nightly and the
M14 re-run is the same mechanism, not a separate regression: M10 added
the trigger; the 100K seed (7 segments pre-M10) now merges, and each
merge pays the five-copy pipeline. The M row confirms the flush-only
baseline (no trigger crossed → ~0.5 GiB).

## The 29-minute seed wall (same finding, separate axis)

Sync durability = one fsync per batch = 1,000,002 fsyncs, plus the
quadratic KeepAll policy: each trigger merges **all** L0+L1, so the bulk
seed re-reads and re-writes the entire accumulated dataset at every 4th
flush — Σ merged ≈ 34 GiB written + re-read at L. The M10 policy assumes
incremental arrival with a bounded dataset; a monotonically growing bulk
seed makes it quadratic. I/O, not memory, is the wall here.

## Fix options (not implemented — next milestone)

Ordered by savings vs. diff size:

1. **Publish in place.** `publish(&mut self)` + `std::mem::take(&mut
   self.entries)` instead of the clone; all callers (flush db.rs:640,
   merge compaction.rs:146/151, archive) own local `mut` writers, so
   this is a signature change. Kills copy #2 (−4.0 GiB at L, ≈ −28%) and
   frees the writer's dead entries after publish. Byte-identical output;
   the existing format goldens are the correctness net.
2. **Stream publish to the temp file.** Blocks are already fully encoded
   when they complete; restart offsets and bloom bits (m = 10·n, n known
   from `entries.len()`) are incremental. Write header, then blocks as
   they fill, then index, bloom, footer. Kills copies #3-#5 (−6.3 GiB at
   L) — publish becomes O(largest block), the real fix. Byte-identical
   layout; moderate diff inside publish/publish_atomic.
3. **Drop the read-back checksum in `compact()` (db.rs:740).** The
   segment's own footer already carries the skeleton checksum
   (segment.rs:293-321); compute the manifest record checksum from the
   same skeleton publish has in hand instead of re-reading 2.1 GiB.
4. **Move, don't clone, at flush (db.rs:634-635).** Draining the
   memtable by `into_iter` moves keys/values into the writer instead of
   cloning — removes the flush-path double copy (visible in the M row's
   baseline).
5. **Compaction policy for bulk loads.** KeepAll merge-everything is
   quadratic on a growing seed. Candidates: size-tiered trigger (merge
   L0 only when its size is a material fraction of L1), or expose the
   existing `l0_compact_trigger = 0` knob to the seed harness. Policy
   decision, not a one-liner — belongs with the next milestone's scope.

Options 1-4 together take the L peak from ~16 GiB to roughly
memtable + one encoded block + index (< 1 GiB), and the write path is
then bounded by the largest single segment merge — which for KeepAll is
still the dataset, but held once, streamed.

## Caveats

- Per-entry real RAM is estimated; the 14.4 vs 16.21 GiB gap is
  allocator overhead, not an unaccounted buffer.
- No re-run was done for this report: the mechanism is read from the
  code, and the three existing measurements bracket it (no-compaction
  baseline at M and pre-M10 100K, trigger-active at post-M10 100K and L).
