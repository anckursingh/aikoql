//! PERF-3 — compaction must not allocate per key.
//! Pre-fix, every merged entry paid a `key.clone()` into the heap (the key
//! was `Reverse<Vec<u8>>` beside the entry, which `fronts` still owned),
//! every key run paid a fresh `Vec` plus a fresh `HashSet` for the rid
//! dedup, every decoded entry paid a suffix Vec and a prev-key clone, and
//! both publish passes cloned each key into their prefix scratch — ~10N
//! allocations merging N single-version keys. The fix: the heap owns each
//! front entry (the key moves in, `fronts` dies), `run` + `grouped` are
//! hoisted and cleared per key run, the decode's prefix base is the
//! previous decoded entry (no suffix Vec, no clone), and the publish
//! passes hold references into the entry list. Steady cost is then the
//! decode (key + value Vecs) plus pipeline constants — 2N + c, under a 4N
//! budget. One test in its own binary: the global-allocator counter is
//! process-wide, and a lone test means no sibling test thread skews the
//! count.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use common::dir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

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

#[test]
fn merge_single_version_keys_allocations_bounded() {
    const N: usize = 10_000;
    let mut cfg = Config::new(dir("compact-alloc"));
    cfg.memtable_bytes = usize::MAX; // no auto-flush: two explicit flushes
    cfg.l0_compact_trigger = 0; // SE2-M10: this test pins MANUAL compaction
    let db = Db::open(cfg).unwrap();
    // Two L0 segments — compact() is a documented no-op at <= 1 segment
    // (db.rs SE-05 pre-check), so one flush would skip the merge entirely.
    for i in 0..N / 2 {
        db.put(format!("key-{i:05}").as_bytes(), &[b'v'; 32][..])
            .unwrap();
    }
    db.flush().unwrap();
    for i in N / 2..N {
        db.put(format!("key-{i:05}").as_bytes(), &[b'v'; 32][..])
            .unwrap();
    }
    db.flush().unwrap();

    ARMED.store(1, Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    let stats = db.compact().unwrap();
    ARMED.store(0, Ordering::Relaxed);
    let allocs = ALLOCS.load(Ordering::Relaxed);

    assert_eq!(stats.entries_in, N as u64, "the merge must see every key");
    assert_eq!(stats.entries_out, N as u64, "every key survives");
    let budget = 4 * N;
    assert!(
        allocs <= budget,
        "{allocs} allocations merging {N} single-version keys (budget \
         {budget}) — the per-key costs (heap key clone + run Vec + grouped \
         HashSet) must be gone; steady cost is the decode + one heap node \
         per entry"
    );
}
