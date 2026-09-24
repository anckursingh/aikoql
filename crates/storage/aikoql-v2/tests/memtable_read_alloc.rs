//! M31 (P0-03) — point reads must not allocate. The flat (key, seq) map
//! builds an owned range start per read (`key.to_vec()`, SE2-M10's
//! accepted one allocation); the chain shape reads through `Borrow<[u8]>`
//! with no key construction. This pin holds get / get_by_rid /
//! prefix_heads to a zero-byte armed peak over a warm mixed table.
//!
//! One test in its own binary: the global-allocator live-byte tracker is
//! process-wide, and a lone test means no sibling test thread skews the
//! delta (the wal_replay_reader / flush_alloc pattern). The timing phase
//! runs unarmed and asserts nothing (mtr002 — the warm p50/p99 cell is
//! reported, not gated: wall time in CI is not a contract).

mod common;

use aikoql_storage_v2::identity::ReplicaId;
use aikoql_storage_v2::memtable::Memtable;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Instant;

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

/// 2k keys x 2 byte versions + an object version on half — a version-
/// heavy, identity-mixed table, built OUTSIDE the pins.
fn fixture() -> Memtable {
    let mut m = Memtable::new();
    for i in 0..2_000u32 {
        let key = format!("k-{i:05}").into_bytes();
        m.apply(key.clone(), 1, Some(vec![b'v'; 16]));
        m.apply(key.clone(), 2, Some(vec![b'v'; 16]));
        if i % 2 == 0 {
            m.apply_object(key, 3, Some(vec![b'o'; 16]), ReplicaId(7));
        }
    }
    m
}

#[test]
fn point_reads_allocate_zero_bytes_and_measure_warm_latency() {
    let m = fixture();
    let keys: Vec<Vec<u8>> = (0..2_000u32)
        .map(|i| format!("k-{i:05}").into_bytes())
        .collect();

    // mtr002 — warm point-read cell (reported, not gated). Durations are
    // collected unarmed into a preallocated vec.
    for i in 0..10_000 {
        let _ = m.get(&keys[i % keys.len()]);
    }
    let n: usize = 100_000;
    let mut durs = vec![0u128; n];
    let t0 = Instant::now();
    for (i, slot) in durs.iter_mut().enumerate() {
        let t = Instant::now();
        let _ = m.get(&keys[i % keys.len()]);
        *slot = t.elapsed().as_nanos();
    }
    let wall = t0.elapsed();
    durs.sort_unstable();
    println!(
        "mtr002 warm memtable point reads: n={n} wall={:.1}ms p50={}ns p99={}ns",
        wall.as_secs_f64() * 1e3,
        durs[n / 2],
        durs[n * 99 / 100]
    );

    // mtr001 — the pin: reads allocate nothing. Today each get clones the
    // key for the range start (peak = key bytes), so this fails.
    ARMED.store(1, Ordering::Relaxed);
    LIVE.store(0, Ordering::Relaxed);
    PEAK_DELTA.store(0, Ordering::Relaxed);
    for i in 0..50_000 {
        let k = &keys[i % keys.len()];
        let _ = m.get(k);
        let _ = m.get_by_rid(k, ReplicaId(7));
        if i % 7 == 0 {
            let _ = m.prefix_heads(b"k-01").count();
        }
    }
    ARMED.store(0, Ordering::Relaxed);
    let peak = PEAK_DELTA.load(Ordering::Relaxed);
    assert_eq!(
        peak, 0,
        "reads allocated — armed peak {peak} bytes over 50k reads"
    );
}
