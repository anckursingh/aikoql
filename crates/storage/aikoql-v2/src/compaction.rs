//! SE2-M4/M5 — L0 → L1 compaction (design §15): a synchronous k-way merge
//! of all segments into one sorted L1 segment. Per key only the max-seq
//! entry survives — reads are newest-wins, so the merge is the exact
//! logical state; distinct keys (AIKOQL's (koid, ts) version rows are
//! distinct keys) are preserved by construction. A tombstone winner drops
//! the key entirely: the merged L1 is the bottom level, nothing below can
//! resurrect it. No deep levels until measurements justify (the doc's own
//! restraint).
//!
//! SE2-M5 adds the retention policy as an INPUT: the caller classifies
//! each key class KEEP/DROP/ARCHIVE, the engine stays key-space-generic.
//! ARCHIVE rows are appended to an archive segment (all versions — they
//! leave the live key space); the archive is never consulted by the live
//! database, only readable directly (SegmentReader).
//!
//! SE2-M20 — chunked emission: the merge publishes its output as a
//! sequence of bounded segments (chunk cap, 0 = one unbounded segment)
//! instead of buffering the whole merged dataset in one writer — the
//! DS-PERF-L RSS amplification. Chunks split on entry granularity in
//! merge order, so chunks are globally sorted and non-overlapping, and
//! ids come from one counter (archive chunks pull lazily). A crash
//! mid-merge leaves only orphan chunks the next open ignores; the
//! manifest naming all chunks stays the single atomic commit point.
//!
//! SE2-M35 — relocation (design §21–25): identity-carrying entries flip
//! each chunk writer to v3, the published chunks' anchors aggregate into
//! the RelocationSet (per-rid max-seq surviving entry; None = nothing
//! survived the live key space — Retired), and the compaction driver
//! turns it into placement records under the §23 publication order.
//! The merge itself stays placement-agnostic — it returns the raw
//! material, the driver owns the protocol.

use crate::format::FormatError;
use crate::identity::ReplicaId;
use crate::placement::{BlockId, SegmentId};
use crate::segment::{
    SegmentAnchor, SegmentAttach, SegmentEntry, SegmentIter, SegmentReader, SegmentWriter,
    FLAG_DELETE,
};
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompactStats {
    pub segments_in: u64,
    pub segments_out: u64,
    pub entries_in: u64,
    pub entries_out: u64,
    pub entries_archived: u64,
    /// Distinct replicas in the merged output — the seen-set denominator
    /// (P5-M36 cp001: the RSS ∝ (keys, replicas) sweep's direct data,
    /// deciding the generation-mark array/bitset vs this HashSet).
    pub rids_seen: u64,
    /// P5-M46 (R4-P2-01) — Σ rid-membership tests the per-key dedup pays:
    /// the Vec tier (below the crossover) counts each equality test, the
    /// set tier counts one insert probe. The k-sweep harness pins the
    /// exact value per tier.
    pub dedup_compares: u64,
    /// P5-M39 — the merge published nothing: a generation changed between
    /// its capture (phase A) and publication (phase C) — a flush
    /// interleaved — so the staged output was discarded. The counters
    /// above still describe the merge that ran.
    pub stale: bool,
}

/// A just-published segment, open for validation: the reader plus the
/// manifest record fields publish computes (SE2-M15 — file size and
/// whole-file checksum8, so callers never read the segment back).
pub type PublishedSegment = (SegmentReader, u64, u64);

/// One published live chunk: its segment id, the open validated reader
/// with its manifest-record fields, and its anchors (SE2-M35 — the
/// relocation set's raw material).
pub(crate) type LiveChunk = (u64, PublishedSegment, Vec<SegmentAnchor>);

/// SE2-M35 — one replica's relocation: its surviving max-seq entry's new
/// home in the merged output, or `None` when nothing of it survived the
/// live key space (tombstone winner, policy drop, or archive) — the §16
/// Retired case. The compaction driver turns each into a placement record
/// with a fresh §25 generation.
pub(crate) type Relocation = Option<(SegmentId, BlockId, u32)>;
pub(crate) type RelocationSet = HashMap<ReplicaId, Relocation>;

/// Per-key-class verdict for a compaction (SE2-M5). The policy is an
/// input, never an engine feature — the engine stays key-space-generic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    Keep,
    Drop,
    Archive,
}

pub trait RetentionPolicy {
    fn classify(&self, key: &[u8]) -> Retention;
}

/// P5-M46 (R4-P2-01) — the two-tier rid-dedup crossover: a key's run
/// with at most this many entries dedups on the linear-scan Vec (no
/// hashing, no per-key allocation); longer runs use the hoisted
/// HashSet, taking the per-key cost from O(k²) to O(k). The M46
/// k-cells (debug): at k=4096 the wall fell 40.4 → 21.5 s with the set
/// tier (compares 2.15G → 1.05M, allocs +1 total — the 21.5 s is the
/// entry-linear merge base, M36-consistent); the marginal compare cost
/// (≈ 9 ns) makes the per-probe hash+insert more expensive, so the Vec
/// wins below the tie and 64 sits above it with margin for the set's
/// occasional rehash. Any value in [16, 512] captures the k=4096 gain —
/// 64 is an estimate, not a measured optimum.
pub const DEDUP_CROSSOVER: usize = 64;

/// The default: compaction is purely mechanical — newest-per-key wins,
/// nothing leaves the live key space but tombstones at the bottom.
pub struct KeepAll;

impl RetentionPolicy for KeepAll {
    fn classify(&self, _key: &[u8]) -> Retention {
        Retention::Keep
    }
}

/// PERF-3 — one heap node owns its front entry, so a push moves the key
/// in instead of cloning it (and the `fronts` staging vector disappears —
/// the heap IS the staging). Ordering: min key first, then max seq (a
/// key's versions drain seq-descending), then idx as the tie-break.
struct HeapEntry {
    entry: SegmentEntry,
    idx: usize,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for HeapEntry {}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        (Reverse(&self.entry.key), self.entry.seq, self.idx).cmp(&(
            Reverse(&other.entry.key),
            other.entry.seq,
            other.idx,
        ))
    }
}

/// Merge `inputs` (key-sorted segments, any levels) into a sequence of
/// bounded L1 segments under `dir` (SE2-M20): live chunks publish as
/// `SEGMENT-{id:06}.seg` with ids pulled from `next_id` in chunk order,
/// archive chunks as `archive/ARCHIVE-{id:06}.seg` / `ARCHIVE-{id:06}-{c}.seg`
/// (the archive id is pulled lazily on first use). `chunk_bytes` is the
/// cap in estimated entry bytes — 0 keeps the pre-M20 shape, one
/// unbounded segment. Chunks split on entry granularity in merge order,
/// so they are globally sorted and non-overlapping. Every live chunk is
/// reopened for validation (the pipeline's validate step) and returned
/// open with its manifest-record fields — SE2-M21: the reopened reader
/// carries `attach` (the shared block cache and read-path stats), a
/// merged segment serves point reads so its reads are cached and counted
/// like any segment's. An empty live output returns no chunks — nothing
/// is published to the live key space (an archive, if any, is still
/// published). A mid-merge error leaves earlier chunks as
/// orphans the next open ignores — the manifest naming all chunks stays
/// the single atomic commit point.
pub(crate) fn merge(
    inputs: &[Arc<SegmentReader>],
    block_target: usize,
    chunk_bytes: usize,
    dir: &Path,
    next_id: &mut u64,
    policy: &dyn RetentionPolicy,
    attach: &SegmentAttach,
) -> Result<(CompactStats, Vec<LiveChunk>, RelocationSet), FormatError> {
    let mut stats = CompactStats {
        segments_in: inputs.len() as u64,
        ..CompactStats::default()
    };
    let mut iters: Vec<SegmentIter> = inputs.iter().map(|r| r.iter()).collect();
    // PERF-3 — the heap owns each segment's front entry (HeapEntry): min
    // key first, then max seq, so every version of one key drains
    // contiguously in seq-descending order, and advance() only ever pulls
    // further versions of that same key.
    let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::new();
    for (i, it) in iters.iter_mut().enumerate() {
        if let Some(e) = it.next().transpose()? {
            heap.push(HeapEntry { entry: e, idx: i });
        }
    }

    let mut live = LiveSink {
        writer: SegmentWriter::new_v2(block_target),
        len: 0,
        chunks: Vec::new(),
    };
    // SE2-M35 — every rid the merge saw, anchored at its surviving
    // max-seq entry (the newest chunk carrying it wins) or None when
    // nothing of it survived the live output.
    let mut seen: HashSet<ReplicaId> = HashSet::new();
    let mut archive: Option<ArchiveSink> = None;
    // PERF-3 — the per-key run and the rid-dedup tiers are hoisted and
    // drained/cleared per key instead of constructed per key.
    let mut run: Vec<SegmentEntry> = Vec::new();
    let mut grouped: Vec<ReplicaId> = Vec::new();
    let mut grouped_set: HashSet<ReplicaId> = HashSet::new();
    while let Some(HeapEntry { entry, idx: i }) = heap.pop() {
        // SE2-M38 — a key is a shared byte namespace: any number of
        // replicas may write it, so the winner is per (key, rid) — each
        // rid's newest entry survives, its older same-rid versions are
        // losers. Byte-API rows (rid 0) form one group: plain per-key
        // last-writer-wins. The run drains seq-descending, so a rid's
        // FIRST entry in the run IS its newest; the advance-first trick
        // still matters — one segment can hold several versions of the
        // key, and a version not yet in the heap would otherwise pop
        // later as a fresh winner.
        run.push(entry);
        stats.entries_in += 1;
        advance(&mut iters, &mut heap, i)?;
        while heap
            .peek()
            .is_some_and(|e| e.entry.key.as_slice() == run[0].key.as_slice())
        {
            let HeapEntry { entry, idx: j } = heap.pop().expect("peeked");
            run.push(entry);
            stats.entries_in += 1;
            advance(&mut iters, &mut heap, j)?;
        }
        for entry in &run {
            if entry.replica_id != ReplicaId(0) {
                seen.insert(entry.replica_id);
            }
        }
        // P5-M46 — two-tier rid dedup: below DEDUP_CROSSOVER the linear
        // scan wins (no hashing, no per-key allocation); at or above it
        // the per-key cost is O(k) instead of O(k²) — the k-cells put
        // k=4096 at 40.4 → 21.5 s. Both tiers are hoisted; the set
        // reuses its allocation across keys (clear keeps the capacity).
        let use_set = run.len() > DEDUP_CROSSOVER;
        match policy.classify(&run[0].key) {
            Retention::Keep => {
                for entry in run.drain(..) {
                    // dedup_compares counts each membership test — the
                    // Vec tier's equality tests, one insert probe on the
                    // set tier (the O(k²) signature below the crossover).
                    let fresh = if use_set {
                        stats.dedup_compares += 1;
                        grouped_set.insert(entry.replica_id)
                    } else {
                        let mut fresh = true;
                        for &rid in &grouped {
                            stats.dedup_compares += 1;
                            if rid == entry.replica_id {
                                fresh = false;
                                break;
                            }
                        }
                        if fresh {
                            // The rid groups even on a tombstone: its
                            // delete wins over older same-rid versions.
                            grouped.push(entry.replica_id);
                        }
                        fresh
                    };
                    if fresh && entry.flags & FLAG_DELETE == 0 {
                        push_live(&mut live, dir, next_id, chunk_bytes, entry, attach)?;
                        stats.entries_out += 1;
                    }
                }
            }
            Retention::Drop => {
                run.clear();
            }
            Retention::Archive => {
                let aw = archive.get_or_insert_with(|| ArchiveSink::new(block_target));
                for entry in run.drain(..) {
                    push_archive(aw, dir, next_id, chunk_bytes, entry)?;
                    stats.entries_archived += 1;
                }
            }
        }
        grouped.clear();
        if use_set {
            grouped_set.clear();
        }
    }

    if live.len > 0 {
        live.chunks.push(publish_chunk(
            &mut live.writer,
            &mut live.len,
            dir,
            next_id,
            attach,
        )?);
    }
    // An archive is published even when the live output is empty.
    if let Some(mut aw) = archive {
        if aw.len > 0 {
            let id = aw.id.get_or_insert_with(|| {
                let id = *next_id;
                *next_id += 1;
                id
            });
            publish_archive_chunk(&mut aw.writer, &mut aw.len, dir, *id, aw.chunk)?;
        }
    }
    stats.segments_out = live.chunks.len() as u64;
    stats.rids_seen = seen.len() as u64;
    // The relocation set: per-rid anchors aggregate across chunks (a
    // replica's keys may straddle a chunk boundary — the max-seq entry's
    // chunk wins), and every seen rid gets its entry (None = Retired).
    let mut anchored: HashMap<ReplicaId, (u64, SegmentId, BlockId, u32)> = HashMap::new();
    for (segment_id, _published, anchors) in &live.chunks {
        for a in anchors {
            let slot = (a.seq, SegmentId(*segment_id), a.block_id, a.entry_offset);
            match anchored.get(&a.replica_id) {
                Some(&(best, ..)) if best >= a.seq => {}
                _ => {
                    anchored.insert(a.replica_id, slot);
                }
            }
        }
    }
    let mut relocations: RelocationSet = HashMap::new();
    for &rid in &seen {
        relocations.insert(rid, anchored.get(&rid).map(|&(_, sid, b, o)| (sid, b, o)));
    }
    Ok((stats, live.chunks, relocations))
}

/// Wire-size upper bound of one entry (shared-prefix savings ignored — an
/// over-estimate is exactly right for a memory bound).
fn entry_bytes(e: &SegmentEntry) -> usize {
    e.key.len() + e.value.len() + 17
}

/// Buffer one live entry, publishing the current chunk first when the cap
/// would be exceeded (the buffer never holds more than the cap + one
/// entry, and a chunk never publishes empty).
fn push_live(
    sink: &mut LiveSink,
    dir: &Path,
    next_id: &mut u64,
    chunk_bytes: usize,
    e: SegmentEntry,
    attach: &SegmentAttach,
) -> Result<(), FormatError> {
    let est = entry_bytes(&e);
    if chunk_bytes > 0 && sink.len > 0 && sink.len + est > chunk_bytes {
        sink.chunks.push(publish_chunk(
            &mut sink.writer,
            &mut sink.len,
            dir,
            next_id,
            attach,
        )?);
    }
    // SE2-M35/M39 — an identity-carrying entry flips the writer to v4
    // (v3 rids + the dense cadence table): the merged output persists rids
    // AND placement-direct reads, so relocation anchors keep the fast path
    // (rid-0 rows encode identically either way).
    if e.replica_id != ReplicaId(0) {
        sink.writer.enable_v4();
    }
    sink.len += est;
    sink.writer.push(e);
    Ok(())
}

/// Publish one buffered live chunk at SEGMENT-{id:06}.seg, pulling its id
/// from `next_id`, and reopen it for validation — with `attach`, so reads
/// the merged segment serves are cached and attributed (SE2-M21).
/// SE2-M35 — the chunk's anchors come back too (empty for a v2 chunk):
/// they are the relocation set's raw material.
fn publish_chunk(
    writer: &mut SegmentWriter,
    len: &mut usize,
    dir: &Path,
    next_id: &mut u64,
    attach: &SegmentAttach,
) -> Result<LiveChunk, FormatError> {
    let id = *next_id;
    *next_id += 1;
    let path = crate::segment::segment_path(dir, id);
    // SE2-M36 — staged: publish_chunk only ever runs inside compaction.
    // P5-M41 — sorted: the merge heap emitted publish order (key asc,
    // seq desc within key), so the staged publish takes it straight
    // through — no whole-buffer sort of heap-ordered entries.
    let (file_size, checksum, anchors) =
        writer.publish_with_anchors_sorted_staged(&path, Some("SEGMENT"))?;
    let reader = SegmentReader::open_with(&path, attach.cache.clone(), attach.stats.clone())?;
    *len = 0;
    Ok((id, (reader, file_size, checksum), anchors))
}

/// The live merge output: the buffering writer plus its chunk accounting
/// (the ArchiveSink shape, SE2-M20).
struct LiveSink {
    writer: SegmentWriter,
    len: usize,
    chunks: Vec<LiveChunk>,
}

/// The buffered archive output: writer plus its chunk accounting. The
/// archive id is pulled from the shared counter lazily, on the first
/// chunk publish — an archive that never emits a chunk consumes no id.
struct ArchiveSink {
    writer: SegmentWriter,
    len: usize,
    chunk: usize,
    id: Option<u64>,
}

impl ArchiveSink {
    fn new(block_target: usize) -> Self {
        ArchiveSink {
            writer: SegmentWriter::new_v2(block_target),
            len: 0,
            chunk: 0,
            id: None,
        }
    }
}

/// Buffer one archive entry, splitting archive chunks on the same cap. A
/// key's version run may straddle two chunks — the archive is never
/// consulted for answers, each chunk stays a valid standalone segment.
fn push_archive(
    sink: &mut ArchiveSink,
    dir: &Path,
    next_id: &mut u64,
    chunk_bytes: usize,
    e: SegmentEntry,
) -> Result<(), FormatError> {
    let est = entry_bytes(&e);
    if chunk_bytes > 0 && sink.len > 0 && sink.len + est > chunk_bytes {
        let this = sink.id.get_or_insert_with(|| {
            let id = *next_id;
            *next_id += 1;
            id
        });
        publish_archive_chunk(&mut sink.writer, &mut sink.len, dir, *this, sink.chunk)?;
        sink.chunk += 1;
    }
    // SE2-M35/M39 — the archive preserves identity too: its rows left the
    // live key space but remain historically readable with their rids.
    if e.replica_id != ReplicaId(0) {
        sink.writer.enable_v4();
    }
    sink.len += est;
    sink.writer.push(e);
    Ok(())
}

/// Publish one archive chunk at archive/ARCHIVE-{id:06}.seg (first chunk)
/// or archive/ARCHIVE-{id:06}-{c}.seg. Not reopened for validation —
/// archives are never consulted by the live database (SE2-M5 unchanged).
fn publish_archive_chunk(
    writer: &mut SegmentWriter,
    len: &mut usize,
    dir: &Path,
    id: u64,
    chunk: usize,
) -> Result<(), FormatError> {
    let name = if chunk == 0 {
        format!("ARCHIVE-{id:06}.seg")
    } else {
        format!("ARCHIVE-{id:06}-{chunk}.seg")
    };
    let archive_dir = dir.join("archive");
    std::fs::create_dir_all(&archive_dir).map_err(|e| {
        FormatError::Io(format!("create archive dir {}: {e}", archive_dir.display()))
    })?;
    // P5-M41 — heap order straight through (no stage: archives ride no
    // crash window). Anchors are dropped — an archive is never consulted
    // for placement (the publish body computes them identically).
    let _ = writer.publish_with_anchors_sorted_staged(&archive_dir.join(name), None)?;
    *len = 0;
    Ok(())
}

/// Pull the next entry of iterator `i` into the heap — PERF-3: the entry
/// moves in, no key clone.
fn advance(
    iters: &mut [SegmentIter],
    heap: &mut BinaryHeap<HeapEntry>,
    i: usize,
) -> Result<(), FormatError> {
    if let Some(e) = iters[i].next().transpose()? {
        heap.push(HeapEntry { entry: e, idx: i });
    }
    Ok(())
}
