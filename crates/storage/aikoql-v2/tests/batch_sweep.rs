//! P5-M42 (R4-P1-03 + R4-P2-02) — get_many O(1) resolution tracking:
//! the review's batch sweep. The two findings — the per-resolution O(B)
//! `remaining.retain` (O(B²) worst case per segment pass, db.rs) and
//! the `HashMap<usize, (u64, u64)>` bloom-hash cache over dense input
//! positions — are pinned here: positional `Vec<bool>` resolution +
//! `Vec<Option<(u64, u64)>>` bloom hashes.
//!
//! The sweep: 128/512/1K/4K/16K × {one segment, spread (round-robin
//! over 16 segments)}. Each point measures wall, allocations (the
//! PERF-3 counting allocator, re-armed around get_many only) and the
//! retain-scan counter. Pins:
//!   - retain scans scale O(B): `batch_retain_scans` at the large size
//!     ≤ the 128-key delta × the size ratio × 2. The per-resolution
//!     retain scans B(B+1)/2 elements (4K:128 ≈ 1016×) — the pin
//!     fails on it; the per-pass compaction scans O(B) (≈ 32×).
//!     Structural (an exact counter, never a wall assert).
//!   - allocs not worse than O(B): the same ratio shape with 4× slack —
//!     the regression guard on the new code's allocation profile (one
//!     Vec<bool>, one Vec<Option>, no per-probe allocation).
//!   - correctness parity: answers by construction, duplicates share
//!     their first position's answer.
//!
//! One test in its own binary: the global-allocator counter is
//! process-wide. All ten points are cheap (≤ 16K keys each) — no
//! env gate.

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use common::{dir, stats_delta};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) == 1 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) == 1 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Seed n keys into `segments` segments: puts + a flush every
/// n/segments keys (one segment = the whole corpus in one flush).
/// Returns the db and the (key, value) corpus in write order.
fn build_db(tag: &str, n: usize, segments: usize) -> (Db, Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut cfg = Config::new(dir(&format!("m42-{tag}")));
    cfg.durability = DurabilityMode::Async; // fsync per batch would dominate
    cfg.memtable_bytes = usize::MAX; // no auto-flush: explicit flushes
    let db = Db::open(cfg).unwrap();
    let mut keys = Vec::with_capacity(n);
    let mut vals = Vec::with_capacity(n);
    let per = n.div_ceil(segments);
    for i in 0..n {
        let key = format!("k{i:08}").into_bytes();
        let val = format!("v{i:08}").into_bytes();
        db.put(&key, &val).unwrap();
        keys.push(key);
        vals.push(val);
        if segments > 1 && (i + 1) % per == 0 && i + 1 < n {
            db.flush().unwrap();
        }
    }
    db.flush().unwrap();
    (db, keys, vals)
}

/// One grid point: the full corpus + one duplicate + two absent keys
/// (outside every segment's range), answered by construction. Returns
/// (wall ms, allocations, retain scans).
fn one_batch(db: &Db, keys: &[Vec<u8>], vals: &[Vec<u8>], layout: &str) -> (u64, u64, u64) {
    let n = keys.len();
    let mut q: Vec<Vec<u8>> = keys.to_vec();
    q.push(keys[3].clone()); // duplicate of position 3
    q.push(b"kabsent-0".to_vec());
    q.push(b"kabsent-1".to_vec());
    let refs: Vec<&[u8]> = q.iter().map(|k| k.as_slice()).collect();
    let mut expected: Vec<Option<Vec<u8>>> = vals.iter().map(|v| Some(v.clone())).collect();
    expected.push(Some(vals[3].clone()));
    expected.push(None);
    expected.push(None);

    let before = db.read_path_stats();
    ARMED.store(1, Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    let t = Instant::now();
    let batch = db.get_many(&refs).unwrap();
    let wall_ms = t.elapsed().as_millis() as u64;
    ARMED.store(0, Ordering::Relaxed);
    let delta = stats_delta(db.read_path_stats(), before);

    assert_eq!(batch, expected, "{layout} n={n}: answers by construction");
    assert_eq!(
        batch[3], batch[n],
        "{layout} n={n}: the duplicate position shares position 3's answer"
    );
    // No wall pin here: a sub-millisecond cell is a valid measurement
    // (CI round 2: the 128-key batch on a fast runner), and the cell is
    // written to cells.json unconditionally below.
    (
        wall_ms,
        ALLOCS.load(Ordering::Relaxed) as u64,
        delta.batch_retain_scans,
    )
}

#[test]
fn m42_batch_sweep_cells() {
    let points: [(usize, usize); 10] = [
        (128, 1),
        (512, 1),
        (1024, 1),
        (4096, 1),
        (16384, 1),
        (128, 16),
        (512, 16),
        (1024, 16),
        (4096, 16),
        (16384, 16),
    ];
    let mut results: Vec<(String, u64, u64, u64)> = Vec::new();
    let mut cells = String::from("{");
    for &(n, segments) in &points {
        let layout = if segments == 1 { "one" } else { "spread" };
        let tag = format!("{layout}-{n}");
        let (db, keys, vals) = build_db(&tag, n, segments);
        let (wall_ms, allocs, scans) = one_batch(&db, &keys, &vals, layout);
        cells.push_str(&format!(
            "\"{tag}_wall_ms\":{wall_ms},\"{tag}_allocs\":{allocs},\"{tag}_retain_scans\":{scans},"
        ));
        results.push((tag, wall_ms, allocs, scans));
    }
    cells.push('}');
    let cells_path = dir("m42-cells").join("cells.json");
    std::fs::write(&cells_path, &cells).unwrap();
    eprintln!("[m42 cells] {cells}");

    for layout in ["one", "spread"] {
        let g = |n: usize| {
            let &(_, _, allocs, scans) = results
                .iter()
                .find(|(t, ..)| t == &format!("{layout}-{n}"))
                .expect("point recorded");
            (allocs, scans)
        };
        let (a128, s128) = g(128);
        let (a4k, s4k) = g(4096);
        let (a16k, s16k) = g(16384);
        // R4-P1-03 — the retain scans scale with the batch size, not its
        // square (the per-resolution retain scans B(B+1)/2 elements: the
        // 4K:128 ratio would be ~1016× against this 64× bound).
        assert!(
            s4k <= s128 * 32 * 2,
            "{layout}: {s4k} retain scans at 4096 vs {s128} at 128 — the O(B) \
             bound ({}) is blown by the per-resolution retain",
            s128 * 32 * 2
        );
        assert!(
            s16k <= s128 * 128 * 2,
            "{layout}: {s16k} retain scans at 16384 vs {s128} at 128 — the O(B) \
             bound ({}) is blown by the per-resolution retain",
            s128 * 128 * 2
        );
        // R4-P2-02 — allocations scale no worse than O(B) (the regression
        // guard on the positional bloom-hash cache).
        assert!(
            a4k <= a128 * 32 * 4,
            "{layout}: {a4k} allocs at 4096 vs {a128} at 128 — worse than O(B)"
        );
        assert!(
            a16k <= a128 * 128 * 4,
            "{layout}: {a16k} allocs at 16384 vs {a128} at 128 — worse than O(B)"
        );
    }
}
