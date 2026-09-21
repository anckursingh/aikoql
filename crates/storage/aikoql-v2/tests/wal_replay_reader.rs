//! M28 (P0-01) — recovery peak allocation must not scale with the WAL's
//! byte size. `Db::open` reads the WHOLE WAL with `read_to_end` before
//! replay, so the read buffer is a second copy of the WAL live beside the
//! replayed memtable. The reader-based replay holds one frame's bytes at a
//! time, so two WALs with the SAME live data (identical memtable delta)
//! arm the same live-byte delta no matter how the WAL's byte size differs.
//! One test in its own binary: the global-allocator live-byte tracker is
//! process-wide, and a lone test means no sibling test thread skews the
//! delta (the PERF-1 wal_replay_streaming pattern).

mod common;

use aikoql_storage_v2::db::{Config, Db, WAL_FILE};
use aikoql_storage_v2::wal::{encode_frame, Op};
use common::dir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::fs::{self, OpenOptions};
use std::path::Path;
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

/// Open the Db at `d` with the allocator armed; the armed-window live-byte
/// peak is the recovery allocation cost.
fn open_armed(d: &Path) -> usize {
    ARMED.store(1, Ordering::Relaxed);
    LIVE.store(0, Ordering::Relaxed);
    PEAK_DELTA.store(0, Ordering::Relaxed);
    let db = Db::open(Config::new(d.to_path_buf())).unwrap();
    ARMED.store(0, Ordering::Relaxed);
    drop(db);
    PEAK_DELTA.load(Ordering::Relaxed)
}

#[test]
fn recovery_peak_alloc_independent_of_wal_size() {
    const FRAMES: usize = 256;
    const VALUE_LEN: usize = 40 << 10; // ~10.2 MB WAL of live data
    const WAL_BIG: u64 = 100 << 20; // ~100 MB WAL — same live data + torn tail

    let (da, wa) = {
        let d = dir("replay-reader-10m");
        let w = d.join(WAL_FILE);
        let mut bytes = Vec::new();
        for i in 0..FRAMES {
            let ops = [Op::Put(
                format!("key-{i:04}").into_bytes(),
                vec![b'v'; VALUE_LEN],
            )];
            bytes.extend_from_slice(&encode_frame(i as u64 + 1, &ops).unwrap());
        }
        fs::write(&w, &bytes).unwrap();
        (d, w)
    };
    // The 100 MB arm: the SAME valid prefix plus a garbage tail (set_len
    // fills zeros — no AKWF magic, so the tail is pure torn bytes). Same
    // live data, same memtable delta; the byte size differs 10x.
    let (db_b, wb) = {
        let d = dir("replay-reader-100m");
        let w = d.join(WAL_FILE);
        fs::copy(&wa, &w).unwrap();
        let f = OpenOptions::new().write(true).open(&w).unwrap();
        f.set_len(WAL_BIG).unwrap();
        drop(f);
        (d, w)
    };

    let peak_10m = open_armed(&da);
    let peak_100m = open_armed(&db_b);

    // A materializing open adds the WAL's bytes on top of the memtable:
    // the 100 MB arm peaks ~90 MB higher today. The reader holds one frame
    // (plus the probe's fixed 64 KiB chunk) — the same 10 MB memtable
    // arms both, so the peaks agree.
    let budget = 4 << 20;
    assert!(
        peak_100m.saturating_sub(peak_10m) < budget,
        "recovery over a {WAL_BIG}-byte WAL peaked at {peak_100m} live bytes vs \
         {peak_10m} over the 10 MB WAL (budget {budget}) — the WAL is being \
         materialized beside the memtable"
    );

    // Correctness leg: the torn tail was dropped, the prefix data is intact.
    let db = Db::open(Config::new(db_b.clone())).unwrap();
    for i in 0..FRAMES {
        assert_eq!(
            db.get(format!("key-{i:04}").as_bytes()).unwrap().as_deref(),
            Some(&[b'v'; VALUE_LEN][..])
        );
    }
    // And the tail is physically gone — the reopen truncated to the prefix.
    assert_eq!(
        fs::metadata(&wb).unwrap().len(),
        fs::metadata(&wa).unwrap().len()
    );
}
