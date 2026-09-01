//! SE2-M4 — L0 → L1 compaction (design §15): a synchronous k-way merge of
//! all segments into one sorted L1 segment. Per key only the max-seq entry
//! survives — reads are newest-wins, so the merge is the exact logical
//! state; distinct keys (AIKOQL's (koid, ts) version rows are distinct
//! keys) are preserved by construction. A tombstone winner drops the key
//! entirely: the merged L1 is the bottom level, nothing below can
//! resurrect it. No deep levels until measurements justify (the doc's own
//! restraint).

use crate::format::FormatError;
use crate::segment::{SegmentEntry, SegmentIter, SegmentReader, SegmentWriter, FLAG_DELETE};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompactStats {
    pub segments_in: u64,
    pub segments_out: u64,
    pub entries_in: u64,
    pub entries_out: u64,
}

/// Merge `inputs` (key-sorted segments, any levels) into one L1 segment
/// published at `out_path` and validated by reopening it. Returns the open
/// reader, or None when every key resolved to a tombstone — the output
/// would be empty, so nothing is published and the L1 set is empty.
pub fn merge(
    inputs: &[SegmentReader],
    block_target: usize,
    out_path: &Path,
) -> Result<(CompactStats, Option<SegmentReader>), FormatError> {
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

    let mut writer = SegmentWriter::new(block_target);
    while let Some((key, _, i)) = heap.pop() {
        let winner = fronts[i].take().expect("front present");
        stats.entries_in += 1;
        // Advance the winner's iterator FIRST: one segment can hold
        // several versions of the key, and a version not yet in the heap
        // would otherwise pop later as a fresh winner (duplicate).
        advance(&mut iters, &mut fronts, &mut heap, i)?;
        // Losers of the same key drain contiguously (heap order, plus
        // whatever further versions advance() keeps pulling): drop them —
        // only the newest entry of a key survives.
        while heap
            .peek()
            .is_some_and(|(k, _, _)| k.0.as_slice() == key.0.as_slice())
        {
            let (_, _, j) = heap.pop().expect("peeked");
            let _ = fronts[j].take();
            stats.entries_in += 1;
            advance(&mut iters, &mut fronts, &mut heap, j)?;
        }
        if winner.flags & FLAG_DELETE == 0 {
            writer.push(winner);
            stats.entries_out += 1;
        }
    }

    if stats.entries_out == 0 {
        return Ok((stats, None));
    }
    writer.publish(out_path)?;
    // The validate step of the pipeline: structural damage must never
    // reach the manifest.
    let reader = SegmentReader::open(out_path)?;
    stats.segments_out = 1;
    Ok((stats, Some(reader)))
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
