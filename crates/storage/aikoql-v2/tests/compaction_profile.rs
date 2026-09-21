//! P5-M36 (P1-08 + P1-09) — compaction scale profile. cp001 records
//! the merge's own cost across a (keys × replica-count) sweep: wall,
//! peak RSS growth (base = the pre-compact process — the memtable is
//! already resident, so the delta is the merge's own), allocation
//! count (the PERF-3 harness, re-armed around compact()), and the
//! engine's rids_seen — the seen-set denominator the decision needs
//! (generation-mark array/bitset vs the HashSet; rule 11: no custom
//! structure without a cell showing the gain).
//!
//! Env-gated: AIKOQL_V2_CP_CELLS_PROFILE=1 runs the 1M points (2M
//! puts); AIKOQL_V2_CP_CELLS_FULL=1 adds the 10M points (22M puts
//! total — the laptop runs the base gate, CI the full one). One test
//! in its own binary: the global-allocator counter is process-wide.
//!
//!   AIKOQL_V2_CP_CELLS_PROFILE=1 cargo test -p aikoql-storage-v2 \
//!     --test compaction_profile -- --nocapture

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use aikoql_storage_v2::identity::ReplicaId;
use aikoql_storage_v2::wal::Op;
use common::dir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// One grid point: seed n keys (half, flush, half, flush — two L0
/// segments, compact() no-ops at ≤ 1), each carrying rid `i % r + 1`,
/// then merge under the alloc arm + RSS sampler. Returns (wall ms,
/// peak RSS growth KiB, allocation count, rids_seen).
fn one_merge(n: usize, r: usize) -> (u64, u64, u64, u64) {
    let mut cfg = Config::new(dir(&format!("cp-{n}-{r}")));
    cfg.durability = DurabilityMode::Async; // fsync per batch would dominate the cell
    cfg.memtable_bytes = usize::MAX; // no auto-flush: two explicit flushes
    cfg.l0_compact_trigger = 0; // manual compaction only (the PERF-3 shape)
    let db = Db::open(cfg).unwrap();
    let val = vec![b'v'; 32];
    for half in 0..2 {
        for i in half * n / 2..(half + 1) * n / 2 {
            let rid = ReplicaId((i % r) as u64 + 1);
            let key = format!("k{i:08}").into_bytes();
            db.write(&[Op::PutObject(rid, key, val.clone())]).unwrap();
        }
        db.flush().unwrap();
    }

    let base = common::self_rss_kb();
    let peak = Arc::new(AtomicU64::new(base));
    let stop = Arc::new(AtomicBool::new(false));
    let (p2, s2) = (Arc::clone(&peak), Arc::clone(&stop));
    let sampler = std::thread::spawn(move || {
        while !s2.load(Ordering::Relaxed) {
            p2.fetch_max(common::self_rss_kb(), Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    ARMED.store(1, Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    let t = Instant::now();
    let stats = db.compact().unwrap();
    let wall_ms = t.elapsed().as_millis() as u64;
    ARMED.store(0, Ordering::Relaxed);
    stop.store(true, Ordering::Relaxed);
    sampler.join().expect("sampler thread");
    let growth_kb = peak.load(Ordering::Relaxed).saturating_sub(base);
    (
        wall_ms,
        growth_kb,
        ALLOCS.load(Ordering::Relaxed) as u64,
        stats.rids_seen,
    )
}

#[test]
fn cp001_merge_scale_cells() {
    if std::env::var_os("AIKOQL_V2_CP_CELLS_PROFILE").is_none() {
        return; // env-gated — 2M puts for the base gate
    }
    let full = std::env::var_os("AIKOQL_V2_CP_CELLS_FULL").is_some();
    let mut points: Vec<(&str, usize, usize)> =
        vec![("n1m_r1k", 1_000_000, 1_000), ("n1m_r100k", 1_000_000, 100_000)];
    if full {
        points.push(("n10m_r1k", 10_000_000, 1_000));
        points.push(("n10m_r100k", 10_000_000, 100_000));
    }
    let mut cells = String::from("{");
    for (tag, n, r) in points {
        let (wall_ms, growth_kb, allocs, rids) = one_merge(n, r);
        assert!(wall_ms > 0, "{tag}: the merge wall is recorded");
        assert!(allocs > 0, "{tag}: the allocation count is recorded");
        assert!(
            allocs <= 4 * n as u64,
            "{tag}: {allocs} allocations merging {n} keys (budget {}) — the \
             PERF-3 per-key budget must hold at scale",
            4 * n
        );
        assert_eq!(
            rids as usize, r,
            "{tag}: the sweep premise — every swept rid appears exactly once \
             (CompactStats.rids_seen is the seen-set denominator)"
        );
        cells.push_str(&format!(
            "\"{tag}_wall_ms\":{wall_ms},\"{tag}_rss_growth_kb\":{growth_kb},\
             \"{tag}_allocs\":{allocs},\"{tag}_rids_seen\":{rids},"
        ));
    }
    cells.push('}');
    let cells_path = dir("cp001-cells").join("cells.json");
    std::fs::write(&cells_path, &cells).unwrap();
    eprintln!("[cp001 cells] {cells}");
}
