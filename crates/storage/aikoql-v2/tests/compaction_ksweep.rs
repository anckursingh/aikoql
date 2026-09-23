//! P5-M46 (R4-P2-01) — two-tier replica dedup k-sweep.
//! compaction.rs:219-233 dedups each key's version run with a linear
//! `grouped.contains` scan — O(k²) per key with k surviving candidates
//! (a key whose k versions all carry distinct rids pays k(k-1)/2 rid
//! compares). The review's deliverable: benchmark
//! k = 2/8/32/128/512/4096 FIRST; ship the two-tier (Vec below the
//! crossover, HashSet above) ONLY where the cells show a crossover
//! worth the branch (rule 11).
//!
//! The RED: the harness drives CompactStats.dedup_compares — the Σ
//! rid-equality tests the dedup path pays (the O(k²) signature) —
//! which does not exist today, so this does not compile. The feat is
//! the counter (the scan goes inline to count it); the two-tier ships
//! only on evidence.
//!
//! Cells (AIKOQL_V2_K_CELLS=1): k ∈ 2/8/32/128/512/4096 × N keys, each
//! key written once per rid (k distinct rids — the worst case for the
//! scan: every probe misses and pushes). Per cell: merge wall,
//! allocation count (the PERF-3 arm around compact()), dedup compares,
//! rids_seen (the M36 pin re-run). One test in its own binary: the
//! global-allocator counter is process-wide.
//!
//! AIKOQL_V2_K_CELLS_FULL=1 raises N (256 → 2048) for the CI arm.
//!
//!   AIKOQL_V2_K_CELLS=1 cargo test -p aikoql-storage-v2 \
//!     --test compaction_ksweep -- --nocapture

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use aikoql_storage_v2::identity::ReplicaId;
use aikoql_storage_v2::wal::Op;
use common::dir;
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

/// One k-sweep grid point: N keys, each written once by each of k rids
/// (half the writes, flush, half, flush — two L0 segments, so compact()
/// merges), then the merge under the alloc arm. Returns (wall ms,
/// allocs, compares, entries_out, rids_seen).
fn one_merge(n: usize, k: usize) -> (u64, u64, u64, u64, u64) {
    let mut cfg = Config::new(dir(&format!("kcell-{n}-{k}")));
    cfg.durability = DurabilityMode::Async; // fsync per batch would dominate the cell
    cfg.memtable_bytes = usize::MAX; // no auto-flush: two explicit flushes
    cfg.l0_compact_trigger = 0; // manual compaction only (the PERF-3 shape)
    let db = Db::open(cfg).unwrap();
    let val = vec![b'v'; 32];
    for half in 0..2 {
        for i in half * n / 2..(half + 1) * n / 2 {
            let key = format!("k{i:08}").into_bytes();
            for rid in 1..=k as u64 {
                db.write(&[Op::PutObject(
                    ReplicaId(rid),
                    key.clone(),
                    val.clone(),
                )])
                .unwrap();
            }
        }
        db.flush().unwrap();
    }
    ARMED.store(1, Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    let t = Instant::now();
    let stats = db.compact().unwrap();
    let wall_ms = t.elapsed().as_millis() as u64;
    ARMED.store(0, Ordering::Relaxed);
    (
        wall_ms,
        ALLOCS.load(Ordering::Relaxed) as u64,
        stats.dedup_compares,
        stats.entries_out,
        stats.rids_seen,
    )
}

/// The O(k²) signature pin (ungated — 128 puts + one merge): k distinct
/// rids per key means every probe misses, so the dedup pays exactly
/// k(k-1)/2 rid compares per key; every (key, rid) survives.
#[test]
fn dedup_compare_pin() {
    let (wall_ms, allocs, compares, entries_out, rids_seen) = one_merge(16, 8);
    assert!(wall_ms > 0, "the merge wall is recorded");
    assert!(allocs > 0, "the allocation count is recorded");
    assert_eq!(
        compares,
        16 * 8 * 7 / 2,
        "k=8 distinct rids pay 28 compares per key — the exact O(k²) signature"
    );
    assert_eq!(entries_out, 128, "distinct rids all survive");
    assert_eq!(rids_seen, 8, "the M36 pin — every swept rid appears once");
}

/// Tombstone grouping parity (ungated): rid 1 deletes each key after
/// writing it — its tombstone is the (key, 1) winner, so its puts never
/// survive, yet the rid still probes the dedup scan (the memtable
/// appends both versions, so each key's run has k+1 entries: the delete
/// probes 0, the put probes 1 and finds it, the k-1 other rids probe
/// 1..k-1 — per key exactly 1 + k(k-1)/2 compares) and counts as seen.
#[test]
fn tombstone_rid_groups_and_counts() {
    let n = 16;
    let k = 8;
    let mut cfg = Config::new(dir("kcell-tomb"));
    cfg.durability = DurabilityMode::Async;
    cfg.memtable_bytes = usize::MAX;
    cfg.l0_compact_trigger = 0;
    let db = Db::open(cfg).unwrap();
    let val = vec![b'v'; 32];
    for i in 0..n {
        let key = format!("k{i:08}").into_bytes();
        db.write(&[Op::PutObject(ReplicaId(1), key.clone(), val.clone())])
            .unwrap();
        db.write(&[Op::DeleteObject(ReplicaId(1), key.clone())])
            .unwrap();
        for rid in 2..=k as u64 {
            db.write(&[Op::PutObject(
                ReplicaId(rid),
                key.clone(),
                val.clone(),
            )])
            .unwrap();
        }
    }
    db.flush().unwrap();
    let stats = db.compact().unwrap();
    assert_eq!(
        stats.dedup_compares,
        (n * (1 + k * (k - 1) / 2)) as u64,
        "the tombstone's rid still probes (and its own put re-probes it)"
    );
    assert_eq!(
        stats.entries_out,
        (n * (k - 1)) as u64,
        "the delete wins over its rid's put — only rids 2..=k survive"
    );
    assert_eq!(stats.rids_seen, k as u64, "the tombstoned rid is still seen");
}

#[test]
fn ksweep_cells() {
    if std::env::var_os("AIKOQL_V2_K_CELLS").is_none() {
        return; // env-gated — the base gate writes ~1.2M puts
    }
    let full = std::env::var_os("AIKOQL_V2_K_CELLS_FULL").is_some();
    let n = if full { 2048 } else { 256 };
    let mut lines: Vec<String> = Vec::new();
    for &k in &[2usize, 8, 32, 128, 512, 4096] {
        let (wall_ms, allocs, compares, entries_out, rids_seen) = one_merge(n, k);
        assert_eq!(
            compares,
            (n * k * (k - 1) / 2) as u64,
            "k={k}: the exact O(k²) probe count — every probe misses"
        );
        assert_eq!(rids_seen, k as u64, "k={k}: the M36 pin re-run");
        assert_eq!(
            entries_out,
            (n * k) as u64,
            "k={k}: distinct rids all survive"
        );
        lines.push(format!(
            "{{\"k\":{k},\"n\":{n},\"wall_ms\":{wall_ms},\"allocs\":{allocs},\
             \"compares\":{compares},\"rids_seen\":{rids_seen}}}"
        ));
    }
    let cells_path = dir("k-cells").join("cells.json");
    std::fs::write(&cells_path, lines.join("\n")).unwrap();
    eprintln!("[k cells] {}", lines.join("\n"));
}
