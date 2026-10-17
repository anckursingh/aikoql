//! CI-03 (review W5) — the per-commit perf smoke's small-compaction cell.
//!
//! One merge of two flushed L0 segments (2 × 25K keys, 4 replica ids)
//! under a counting allocator: wall + allocation count — the structural
//! pair the perf-smoke checker budgets at 3× vs the committed baseline
//! (artifacts/storage-engine-v2/perf-smoke-baseline.json). The smoke
//! script arms it with STORAGE_PERF_SMOKE=1 and the result lands in
//! artifacts/storage-engine-v2/compact-smoke.json (stamped with the
//! tested HEAD — the fresh side of a comparison, P5-M47); unarmed it is
//! a zero-cost no-op in the per-push suite. The full merge sweep stays
//! cp001 at 1M/2M puts (P5-M36).
//!
//! Own binary: the global-allocator counter is process-wide (the P5-M36
//! cp001 pattern).
//!
//!   STORAGE_PERF_SMOKE=1 cargo test -p aikoql-storage-v2 \
//!     --test compaction_smoke --release

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use aikoql_storage_v2::identity::ReplicaId;
use aikoql_storage_v2::wal::Op;
use common::dir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
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

/// Two flushed segments of n keys each, then the merge under the alloc
/// arm — the P5-M36 one_merge shape at smoke scale.
fn one_merge(n: usize, r: usize) -> (u64, u64) {
    let mut cfg = Config::new(dir("compact-smoke"));
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

    ARMED.store(1, Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    let t = Instant::now();
    let stats = db.compact().unwrap();
    let wall_ms = t.elapsed().as_millis() as u64;
    ARMED.store(0, Ordering::Relaxed);
    assert!(
        stats.rids_seen == r as u64,
        "the sweep premise — every swept rid appears exactly once"
    );
    (wall_ms, ALLOCS.load(Ordering::Relaxed) as u64)
}

#[test]
fn compact_smoke_cell() {
    if std::env::var_os("STORAGE_PERF_SMOKE").is_none() {
        return; // env-gated — the smoke script arms it
    }
    let (wall_ms, allocs) = one_merge(25_000, 4);
    assert!(wall_ms > 0, "the merge wall is recorded");
    assert!(allocs > 0, "the allocation count is recorded");
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../artifacts/storage-engine-v2/compact-smoke.json");
    let json = format!(
        "{{\n \"environment\": {{\"git_sha\": \"{}\"}},\n \"cells\": {{\"compact_wall_ms\": {wall_ms}, \"compact_allocs\": {allocs}}}\n}}\n",
        git_sha()
    );
    std::fs::write(&out, json).unwrap();
    eprintln!(
        "[compact smoke] wall {wall_ms} ms · allocs {allocs} → {}",
        out.display()
    );
}

fn git_sha() -> String {
    match std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "NOT_REPORTED".into(),
    }
}
