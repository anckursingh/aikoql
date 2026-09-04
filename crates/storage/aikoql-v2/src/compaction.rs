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

use crate::format::FormatError;
use crate::segment::{
    SegmentAttach, SegmentEntry, SegmentIter, SegmentReader, SegmentWriter, FLAG_DELETE,
};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompactStats {
    pub segments_in: u64,
    pub segments_out: u64,
    pub entries_in: u64,
    pub entries_out: u64,
    pub entries_archived: u64,
}

/// A just-published segment, open for validation: the reader plus the
/// manifest record fields publish computes (SE2-M15 — file size and
/// whole-file checksum8, so callers never read the segment back).
pub type PublishedSegment = (SegmentReader, u64, u64);

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

/// The default: compaction is purely mechanical — newest-per-key wins,
/// nothing leaves the live key space but tombstones at the bottom.
pub struct KeepAll;

impl RetentionPolicy for KeepAll {
    fn classify(&self, _key: &[u8]) -> Retention {
        Retention::Keep
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
) -> Result<(CompactStats, Vec<(u64, PublishedSegment)>), FormatError> {
    let mut stats = CompactStats {
        segments_in: inputs.len() as u64,
        ..CompactStats::default()
    };
    let mut iters: Vec<SegmentIter> = inputs.iter().map(|r| r.iter()).collect();
    // One front entry per segment, already pulled from its iterator.
    let mut fronts: Vec<Option<SegmentEntry>> = iters
        .iter_mut()
        .map(|it| it.next().transpose())
        .collect::<Result<_, _>>()?;
    // Heap: min key first, then max seq — every version of one key drains
    // contiguously, so the first pop of a key IS its winner. The key is
    // Reversed (max-heap pops min key); the seq is NOT — max seq pops
    // first within a key.
    let mut heap: BinaryHeap<(Reverse<Vec<u8>>, u64, usize)> = BinaryHeap::new();
    for (i, f) in fronts.iter().enumerate() {
        if let Some(e) = f {
            heap.push((Reverse(e.key.clone()), e.seq, i));
        }
    }

    let mut live = LiveSink {
        writer: SegmentWriter::new_v2(block_target),
        len: 0,
        chunks: Vec::new(),
    };
    let mut archive: Option<ArchiveSink> = None;
    while let Some((key, _, i)) = heap.pop() {
        let winner = fronts[i].take().expect("front present");
        stats.entries_in += 1;
        // Advance the winner's iterator FIRST: one segment can hold
        // several versions of the key, and a version not yet in the heap
        // would otherwise pop later as a fresh winner (duplicate).
        advance(&mut iters, &mut fronts, &mut heap, i)?;
        // Losers of the same key drain contiguously (heap order, plus
        // whatever further versions advance() keeps pulling): only the
        // newest entry of a key survives the merge — the losers are kept
        // around only long enough to archive them if the policy asks.
        let mut losers: Vec<SegmentEntry> = Vec::new();
        while heap
            .peek()
            .is_some_and(|(k, _, _)| k.0.as_slice() == key.0.as_slice())
        {
            let (_, _, j) = heap.pop().expect("peeked");
            losers.push(fronts[j].take().expect("front present"));
            stats.entries_in += 1;
            advance(&mut iters, &mut fronts, &mut heap, j)?;
        }
        match policy.classify(&winner.key) {
            Retention::Keep => {
                if winner.flags & FLAG_DELETE == 0 {
                    push_live(&mut live, dir, next_id, chunk_bytes, winner, attach)?;
                    stats.entries_out += 1;
                }
            }
            Retention::Drop => {}
            Retention::Archive => {
                let aw = archive.get_or_insert_with(|| ArchiveSink::new(block_target));
                push_archive(aw, dir, next_id, chunk_bytes, winner)?;
                stats.entries_archived += 1;
                for loser in losers {
                    push_archive(aw, dir, next_id, chunk_bytes, loser)?;
                    stats.entries_archived += 1;
                }
            }
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
    Ok((stats, live.chunks))
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
    sink.len += est;
    sink.writer.push(e);
    Ok(())
}

/// Publish one buffered live chunk at SEGMENT-{id:06}.seg, pulling its id
/// from `next_id`, and reopen it for validation — with `attach`, so reads
/// the merged segment serves are cached and attributed (SE2-M21).
fn publish_chunk(
    writer: &mut SegmentWriter,
    len: &mut usize,
    dir: &Path,
    next_id: &mut u64,
    attach: &SegmentAttach,
) -> Result<(u64, PublishedSegment), FormatError> {
    let id = *next_id;
    *next_id += 1;
    let path = crate::segment::segment_path(dir, id);
    let (file_size, checksum) = writer.publish(&path)?;
    let reader = SegmentReader::open_with(&path, attach.cache.clone(), attach.stats.clone())?;
    *len = 0;
    Ok((id, (reader, file_size, checksum)))
}

/// The live merge output: the buffering writer plus its chunk accounting
/// (the ArchiveSink shape, SE2-M20).
struct LiveSink {
    writer: SegmentWriter,
    len: usize,
    chunks: Vec<(u64, PublishedSegment)>,
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
    writer.publish(&archive_dir.join(name))?;
    *len = 0;
    Ok(())
}

/// Pull the next entry of iterator `i` into the heap.
fn advance(
    iters: &mut [SegmentIter],
    fronts: &mut [Option<SegmentEntry>],
    heap: &mut BinaryHeap<(Reverse<Vec<u8>>, u64, usize)>,
    i: usize,
) -> Result<(), FormatError> {
    match iters[i].next() {
        Some(Ok(e)) => {
            heap.push((Reverse(e.key.clone()), e.seq, i));
            fronts[i] = Some(e);
        }
        Some(Err(e)) => return Err(e),
        None => {}
    }
    Ok(())
}
