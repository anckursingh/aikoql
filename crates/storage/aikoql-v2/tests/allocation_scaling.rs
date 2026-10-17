//! L-16 (TDD-014 + TDD-005) — allocation scaling and the VersionChain
//! stress.
//!
//! TDD-014 (P0, "WAL one-allocation test must scale"): the op ladder
//! 1/10/100/1,000/10,000 across six mixes (tiny key/value, 4 KiB value,
//! 64 KiB value, large key, all object ops, mixed ops). The property is
//! structural, not timed: the per-op allocation count must stay
//! effectively constant with respect to the op count — a super-linear
//! term (every op scanning or rebuilding prior state) shows as per-op
//! growth between N=1,000 and N=10,000, where it cannot hide in noise.
//! The assert rides that pair (per-op@10k ≤ 1.5 × per-op@1k — a quadratic
//! term would give ~10×) plus a generous absolute cap; the small-N points
//! are recorded, not asserted (their per-op count is bootstrap noise).
//!
//! TDD-005 (P1, "VersionChain stress"): 1 key × 10/100/1,000/10,000
//! versions in reverse sequence order — the newest-labeled value lands
//! FIRST and the final write must win over all 9,999 older ones; the
//! scan must collapse the chain to one row. The plan's "O(v) insert"
//! state is pinned on the memtable directly: DESCENDING seqs make every
//! arrival out-order the chain (partition_point lands at idx 0, each
//! insert memmoves the whole chain) — 10k inserts ≈ 50M element moves,
//! and the head must still be the highest seq, the chain seq-ascending.
//! Throughput / allocations / p99 are RECORDED per the plan (no absolute
//! thresholds — wall clocks are the flake the plan forbids on CI).
//!
//! One binary: the counting allocator is #[global_allocator] — it counts
//! every allocation in the process, so EVERY test in this binary takes
//! TEST_LOCK (the PARK_LOCK discipline) or a sibling's steady-state
//! allocations land inside the measured window.

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use aikoql_storage_v2::identity::ObjectId;
use aikoql_storage_v2::memtable::Memtable;
use common::dir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

// The counting allocator: alloc + realloc events (a cumulative counter —
// live bytes would net out inside a loop and hide the per-op cost).
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn allocs() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}

/// Serial guard: the alloc counter is process-wide; a sibling running
/// concurrently would add its steady-state allocations to the window.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// No flush (the memtable sits above every workload here), no background
/// merge (l0_compact_trigger 0) — the measured window holds only the op
/// loop, never a compactor's steady-state allocations.
fn open_big(d: &Path) -> Db {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.memtable_bytes = 1 << 30;
    cfg.l0_compact_trigger = 0;
    cfg.durability = DurabilityMode::Async;
    Db::open(cfg).unwrap()
}

fn walk(db: &Db) -> BTreeMap<Vec<u8>, Vec<u8>> {
    db.scan(b"").unwrap().into_iter().collect()
}

// ---------------------------------------------------------------------------
// TDD-014 — the mix ladder
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mix {
    Tiny,
    Val4K,
    Val64K,
    BigKey,
    Object,
    Mixed,
}

impl Mix {
    const ALL: [Mix; 6] = [
        Mix::Tiny,
        Mix::Val4K,
        Mix::Val64K,
        Mix::BigKey,
        Mix::Object,
        Mix::Mixed,
    ];

    fn name(self) -> &'static str {
        match self {
            Mix::Tiny => "tiny",
            Mix::Val4K => "val4k",
            Mix::Val64K => "val64k",
            Mix::BigKey => "bigkey",
            Mix::Object => "object",
            Mix::Mixed => "mixed",
        }
    }

    /// One op of the mix. Key/value construction lives INSIDE the loop
    /// (input construction is linear in N either way — the ratio assert
    /// cannot be fooled by it, and 10k × 60 KiB of pre-built keys would
    /// just hold 600 MB for nothing).
    fn op(self, db: &Db, oid: &Option<ObjectId>, i: u64) {
        match self {
            Mix::Tiny => {
                db.put(format!("k{i:05}").as_bytes(), format!("v{i}").as_bytes())
                    .unwrap();
            }
            Mix::Val4K => {
                db.put(format!("k{i:05}").as_bytes(), &[b'x'; 4096])
                    .unwrap();
            }
            Mix::Val64K => {
                db.put(format!("k{i:05}").as_bytes(), &[b'x'; 65536])
                    .unwrap();
            }
            Mix::BigKey => {
                let mut key = vec![b'k'; 60_000];
                key.extend_from_slice(format!("{i}").as_bytes());
                db.put(&key, b"v").unwrap();
            }
            Mix::Object => {
                db.put_object(oid.unwrap(), format!("k{i:05}").as_bytes(), b"v")
                    .unwrap();
            }
            Mix::Mixed => {
                let k = format!("k{:05}", i % 100).into_bytes();
                match i % 3 {
                    0 => {
                        db.put(&k, b"v").unwrap();
                    }
                    1 => {
                        db.put_object(oid.unwrap(), &k, b"v").unwrap();
                    }
                    _ => {
                        db.delete(&k).unwrap();
                    }
                }
            }
        }
    }
}

/// Per-op allocations for one (mix, n) point: fresh db, 32 warm ops
/// (first-call paths), then the measured window holds ONLY the loop.
fn per_op_allocs(mix: Mix, n: u64) -> f64 {
    let d = dir(&format!("alloc-{}-{}", mix.name(), n));
    let db = open_big(&d);
    let oid = matches!(mix, Mix::Object | Mix::Mixed).then(|| db.create_object().unwrap());
    for i in 0..32 {
        mix.op(&db, &oid, i);
    }
    let before = allocs();
    for i in 0..n {
        mix.op(&db, &oid, i);
    }
    let delta = allocs() - before;
    drop(db);
    delta as f64 / n as f64
}

#[test]
fn tdd014_per_op_allocations_stay_flat_across_the_op_ladder() {
    let _serial = serial();
    const LADDER: [u64; 5] = [1, 10, 100, 1_000, 10_000];
    for mix in Mix::ALL {
        let mut per_op: Vec<f64> = Vec::new();
        for &n in &LADDER {
            let p = per_op_allocs(mix, n);
            eprintln!(
                "RECORDED tdd014 mix={mix} n={n:>5} per_op_allocs={p:.2}",
                mix = mix.name(),
                n = n,
                p = p
            );
            per_op.push(p);
        }
        // The structural property: no super-linear term. per-op@1k and
        // per-op@10k are the pair that can tell them apart (a quadratic
        // term inflates the 10k point ~10×; noise stays under 1.5×).
        assert!(
            per_op[4] <= per_op[3] * 1.5,
            "mix {}: per-op allocations grew with the op count \
             (1k: {:.2}, 10k: {:.2}) — a super-linear term exists",
            mix.name(),
            per_op[3],
            per_op[4]
        );
        // The absolute sanity cap — generous, but a runaway (per-op in
        // the thousands) must never pass silently.
        assert!(
            per_op[4] <= 1000.0,
            "mix {}: {:.2} allocations per op at n=10k",
            mix.name(),
            per_op[4]
        );
    }
}

// ---------------------------------------------------------------------------
// TDD-005 — the VersionChain stress
// ---------------------------------------------------------------------------

/// The trio the plan promises: throughput, per-op allocations, p99 write
/// latency — RECORDED, never thresholded (wall clocks are CI flake).
fn record_trio(
    tag: &str,
    n: u64,
    wall: std::time::Duration,
    delta_allocs: usize,
    mut lat_ns: Vec<f64>,
) {
    lat_ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p99 = lat_ns[(n as usize * 99 / 100).saturating_sub(1)];
    eprintln!(
        "RECORDED {tag} n={n} throughput={:.0}ops/s per_op_allocs={:.2} p99={p99:.0}ns",
        n as f64 / wall.as_secs_f64(),
        delta_allocs as f64 / n as f64,
    );
}

#[test]
fn tdd005_reverse_sequence_versions_newest_wins_and_records() {
    let _serial = serial();
    let mut per_op_at = [0.0f64; 2]; // [1k, 10k] for the slope assert
    for n in [10u64, 100, 1_000, 10_000] {
        let d = dir(&format!("vs001-{n}"));
        let db = open_big(&d);
        let before = allocs();
        let t0 = Instant::now();
        let mut lat_ns = Vec::with_capacity(n as usize);
        // Reverse sequence order: the newest-labeled version lands FIRST
        // (v=N first, v=1 last) — the final write is the newest version
        // and must win over every one of its predecessors.
        for v in (1..=n).rev() {
            let s = Instant::now();
            db.put(b"k", format!("v{v}").as_bytes()).unwrap();
            lat_ns.push(s.elapsed().as_nanos() as f64);
        }
        let wall = t0.elapsed();
        let delta = allocs() - before;
        record_trio("tdd005-vs001", n, wall, delta, lat_ns);
        assert_eq!(
            db.get(b"k").unwrap().as_deref(),
            Some(b"v1".as_slice()),
            "the LAST write is the newest version"
        );
        let rows = walk(&db);
        assert_eq!(
            rows,
            BTreeMap::from([(b"k".to_vec(), b"v1".to_vec())]),
            "the scan collapses {n} versions of one key to one row"
        );
        if n == 1_000 || n == 10_000 {
            per_op_at[if n == 1_000 { 0 } else { 1 }] = delta as f64 / n as f64;
        }
    }
    assert!(
        per_op_at[1] <= per_op_at[0] * 1.5,
        "per-op allocations grew with the version count (1k: {:.2}, 10k: {:.2})",
        per_op_at[0],
        per_op_at[1]
    );
}

#[test]
fn tdd005_descending_seq_memtable_inserts_keep_the_newest_head() {
    let _serial = serial();
    let n = 10_000u64;
    let mut mt = Memtable::new();
    let before = allocs();
    let t0 = Instant::now();
    let mut lat_ns = Vec::with_capacity(n as usize);
    // DESCENDING seqs — every arrival out-orders the chain: the insert's
    // partition_point lands at idx 0 and each insert memmoves the whole
    // chain (the plan row's "O(v) insert", ~50M element moves here).
    for s in (1..=n).rev() {
        let st = Instant::now();
        mt.apply(b"k".to_vec(), s, Some(format!("v{s}").into_bytes()));
        lat_ns.push(st.elapsed().as_nanos() as f64);
    }
    let wall = t0.elapsed();
    let delta = allocs() - before;
    record_trio("tdd005-vs002", n, wall, delta, lat_ns);
    // The O(v) path must not corrupt the head: the highest seq answers.
    assert_eq!(
        mt.get(b"k").unwrap().value.as_deref(),
        Some(format!("v{n}").as_bytes()),
        "the highest seq is the head under descending arrival"
    );
    // And the chain itself stays seq-ascending with every version intact.
    let mut count = 0u64;
    let mut last = 0u64;
    for (k, s, _e) in mt.entries() {
        if k == b"k" {
            count += 1;
            assert!(
                s > last,
                "the chain must stay seq-ascending ({s} after {last})"
            );
            last = s;
        }
    }
    assert_eq!(count, n, "all {n} versions survive the descending inserts");
}
