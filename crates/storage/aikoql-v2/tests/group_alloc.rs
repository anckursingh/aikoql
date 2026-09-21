//! P5-M32 (P1-03) — the committer allocates per group: a fresh seqs Vec
//! per group (commit_group) on top of the per-group fold temporaries. The
//! fix hoists the seqs buffer — clear + reuse across groups — so the
//! per-group allocation count drops by the number of groups. This pin
//! counts armed-window allocations over N sequential groups (wait=0: one
//! batch per group, submission included) against a budget that the
//! hoisted-seqs shape meets and today's per-group seqs Vec does not.
//!
//! One test in its own binary: the global-allocator tracker is
//! process-wide (the wal_replay_reader / flush_alloc pattern).

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use aikoql_storage_v2::wal::Op;
use common::dir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

/// Live-byte tracking allocator: while armed, `LIVE` is the net bytes
/// allocated, `PEAK_DELTA` the high-water mark, and `ALLOC_COUNT` the
/// number of allocations (the per-group pin: reallocs do not count).
struct LiveCounting;
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK_DELTA: AtomicUsize = AtomicUsize::new(0);
static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for LiveCounting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if ARMED.load(Ordering::Relaxed) == 1 {
            let live =
                LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed) + layout.size() as i64;
            PEAK_DELTA.fetch_max(live as usize, Ordering::Relaxed);
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
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
fn per_group_allocations_drop_with_the_hoisted_seqs_buffer() {
    const N: usize = 200; // sequential groups (wait=0: one batch each)
    let d = dir("group-alloc");
    let mut cfg = Config::new(d.clone());
    cfg.durability = DurabilityMode::GroupCommit; // explicit opt-in
    cfg.max_wait_duration = Duration::ZERO;
    let db = Db::open(cfg).unwrap();
    let writer = db.writer().unwrap();

    ARMED.store(1, Ordering::Relaxed);
    LIVE.store(0, Ordering::Relaxed);
    PEAK_DELTA.store(0, Ordering::Relaxed);
    ALLOC_COUNT.store(0, Ordering::Relaxed);
    for i in 0..N {
        writer
            .write(&[Op::Put(
                format!("k{i:04}").into_bytes(),
                format!("v{i:04}").into_bytes(),
            )])
            .unwrap();
    }
    ARMED.store(0, Ordering::Relaxed);
    let allocs = ALLOC_COUNT.load(Ordering::Relaxed);

    // Structural floor per write: batch + ops + group vec + frame + the
    // apply-path clones (key + value + the key's first chain vec) + the
    // mpsc send slot. The per-group seqs Vec adds one MORE per write —
    // the pin budget sits between the two shapes.
    let budget = N * 8;
    assert!(
        allocs < budget,
        "{allocs} allocations over {N} groups (budget {budget}) — a per-group \
         seqs buffer is being allocated on every commit"
    );

    drop(writer);
    drop(db);
    let db = Db::open(Config::new(d)).unwrap();
    for i in 0..N {
        assert!(
            db.get(format!("k{i:04}").as_bytes()).unwrap().is_some(),
            "all writes durable"
        );
    }
}
