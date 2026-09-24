//! PERF-1 — recovery must replay the WAL without materializing the frames.
//! `replay_frames` decodes every frame into a `Vec<WalFrame>` whose ops own
//! the keys and values — a full second copy of the WAL, live on top of the
//! read buffer and the accumulating memtable. The streaming replay applies
//! each frame as it is decoded, so peak live memory is the read buffer +
//! the memtable only. One test in its own binary: the global-allocator
//! live-byte tracker is process-wide, and a lone test means no sibling test
//! thread skews the delta.

mod common;

use aikoql_storage_v2::db::{Config, Db, WAL_FILE};
use aikoql_storage_v2::wal::{encode_frame, Op};
use common::dir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
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

#[test]
fn recovery_replays_without_materializing_frames() {
    const FRAMES: usize = 64;
    const VALUE_LEN: usize = 8 << 10; // ~525 KiB WAL total
    let d = dir("wal-replay-stream");
    let wal_path = d.join(WAL_FILE);
    {
        // One Put per frame, strictly increasing seq — the shape recovery
        // accepts; the torn-tail probe never fires.
        let mut bytes = Vec::new();
        for i in 0..FRAMES {
            let ops = [Op::Put(
                format!("key-{i:04}").into_bytes(),
                vec![b'v'; VALUE_LEN],
            )];
            bytes.extend_from_slice(&encode_frame(i as u64 + 1, &ops).unwrap());
        }
        fs::write(&wal_path, &bytes).unwrap();
    }
    let wal_len = fs::metadata(&wal_path).unwrap().len() as usize;

    let cfg = Config::new(d.clone());
    ARMED.store(1, Ordering::Relaxed);
    LIVE.store(0, Ordering::Relaxed);
    PEAK_DELTA.store(0, Ordering::Relaxed);
    let db = Db::open(cfg).unwrap();
    ARMED.store(0, Ordering::Relaxed);
    let peak = PEAK_DELTA.load(Ordering::Relaxed);

    // Budget: the read buffer + the accumulating memtable (+ BTreeMap node
    // overhead). A materializing replay adds a third copy — the decoded
    // frames — and blows through it.
    let budget = wal_len * 5 / 2;
    assert!(
        peak < budget,
        "recovery peaked at {peak} live bytes over a {wal_len}-byte WAL \
         (budget {budget}) — the frames are being materialized on top of \
         the read buffer and the memtable"
    );

    // Correctness leg: every frame replayed into the recovered state.
    for i in 0..FRAMES {
        assert_eq!(
            db.get(format!("key-{i:04}").as_bytes()).unwrap().as_deref(),
            Some(&[b'v'; VALUE_LEN][..])
        );
    }
}
