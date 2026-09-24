//! M29 (P0-02) — the flush's publish must not sort already-sorted input.
//! The memtable iterates key asc, seq asc within key (BTreeMap order) —
//! exactly the publish's key asc + seq desc requirement with each key's
//! version run reversed. The sorted-input publish
//! (`publish_with_anchors_sorted`) reverses each key run in place: no
//! scratch buffer, O(n) total. This pin holds the flush's entry point to
//! that: publish over memtable-ordered input must not allocate a
//! whole-buffer sort scratch (RED: 7.2 MB driftsort buffer over 100k
//! entries).
//!
//! One test in its own binary: the global-allocator live-byte tracker is
//! process-wide, and a lone test means no sibling test thread skews the
//! delta (the wal_replay_reader / wal_encode_alloc pattern).

mod common;

use aikoql_storage_v2::identity::ReplicaId;
use aikoql_storage_v2::segment::{SegmentEntry, SegmentWriter, FLAG_PUT};
use common::dir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

/// Live-byte tracking allocator: while armed, `LIVE` is the net bytes
/// allocated (frees of pre-arm allocations are clamped so they cannot
/// understate it), and `PEAK_DELTA` is the high-water mark. Dealloc of an
/// armed-window allocation nets out, so the peak is live delta, not churn.
struct LiveCounting;
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK_DELTA: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for LiveCounting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if ARMED.load(Ordering::Relaxed) == 1 {
            let live =
                LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed) + layout.size() as i64;
            PEAK_DELTA.fetch_max(live as usize, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.load(Ordering::Relaxed) == 1 {
            LIVE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some((v - layout.size() as i64).max(0))
            })
            .ok();
        }
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = System.realloc(ptr, layout, new_size);
        if ARMED.load(Ordering::Relaxed) == 1 {
            let live = LIVE.fetch_add(new_size as i64, Ordering::Relaxed) + new_size as i64;
            PEAK_DELTA.fetch_max(live as usize, Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static A: LiveCounting = LiveCounting;

/// N entries in MEMTABLE order (key asc, seq asc within key) — the exact
/// shape the flush pushes. 50k keys × 2 versions = 100k entries.
fn memtable_order_entries(n_keys: usize) -> Vec<SegmentEntry> {
    let mut out = Vec::with_capacity(n_keys * 2);
    for j in 0..n_keys {
        let key = format!("k-{j:05}").into_bytes();
        for v in 0..2 {
            out.push(SegmentEntry {
                key: key.clone(),
                value: vec![b'v'; 16],
                seq: (2 * j + v + 1) as u64,
                flags: FLAG_PUT,
                replica_id: ReplicaId(0),
            });
        }
    }
    out
}

#[test]
fn publish_allocation_excludes_a_whole_input_sort() {
    const N_KEYS: usize = 50_000; // 100k entries, ~7 MB of SegmentEntry
    const BLOCK_TARGET: usize = 16 << 10;

    let path = dir("flush-alloc").join("SEGMENT-001.log");
    let mut writer = SegmentWriter::new_v2(BLOCK_TARGET);
    for e in memtable_order_entries(N_KEYS) {
        writer.push(e);
    }
    // The entries themselves were allocated before the pin — the pin is
    // the PUBLISH's temporary memory: bloom + one block's payload +
    // index on the sorted path; plus the sort's scratch buffer (one
    // SegmentEntry per entry — 7.2 MB here) on a sorting path.
    ARMED.store(1, Ordering::Relaxed);
    LIVE.store(0, Ordering::Relaxed);
    PEAK_DELTA.store(0, Ordering::Relaxed);
    writer.publish_with_anchors_sorted(&path).unwrap();
    ARMED.store(0, Ordering::Relaxed);
    let peak = PEAK_DELTA.load(Ordering::Relaxed);

    // A quarter of the entries' bytes: the sorted path's bloom + block
    // buffers fit well inside; the sort's scratch buffer (half the
    // entries) cannot.
    let budget = writer_size_hint(N_KEYS * 2) / 4;
    assert!(
        peak < budget,
        "publish over {N_KEYS}k memtable-ordered entries peaked at {peak} live bytes \
         (budget {budget}) — the whole-buffer sort scratch is being allocated on \
         input that is sorted by construction"
    );
}

/// size_of::<SegmentEntry>() × n — the sort scratch is n/2 of these.
fn writer_size_hint(n: usize) -> usize {
    n * std::mem::size_of::<SegmentEntry>()
}
