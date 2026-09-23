//! P5-M45 (R4-P1-06) — cache concurrency matrix.
//! Every cache hit takes the one global State mutex (cache.rs — hash +
//! gen stamp + Arc clone inside the critical section); the hot-head
//! evidence (P50 1.2 µs, SE2-M11) is single-threaded, so multi-reader
//! scaling is unmeasured. The review's deliverable: measure FIRST,
//! shard into 8–16 locked shards ONLY if the cells show contention
//! (the review's own gate — no sharding before measurement).
//!
//! The RED: the harness + the wait-tracking API it drives (compile
//! error today — BlockCache has no set_wait_tracking/wait_ns). The
//! feat is the instrumentation; sharding ships only on evidence.
//!
//! Cells (AIKOQL_V2_CACHE_CELLS=1): threads 1/2/4/8/16/32 × regimes
//!   - hot: every thread hammers the same block (the worst case for
//!     the lock — all hits, one entry);
//!   - random: 256 blocks, each thread walks an LCG sequence — hits
//!     spread over the map;
//!   - mixed: per-thread reader ids (4 ids × 64 blocks — the "mixed
//!     segment readers" arm: one cache, many readers).
//! Per cell: throughput (ops/s), per-8-op batch-wall p50/p95/p99 (the
//! op itself is ~tens of ns — single-op walls are clock noise; a batch
//! of 8 amortizes the sample clock), and the contention signal: cache
//! mutex wait as a fraction of thread wall (Σ wait_ns / Σ wall_ns —
//! both pay the same per-op clock when tracking is on, so the fraction
//! is the clean metric). The sharding decision gates on the fraction.

mod common;

use aikoql_storage_v2::cache::BlockCache;
use common::dir;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

const BLOCK: usize = 16 * 1024;
const OPS: usize = 200_000;

#[test]
fn cache_concurrency_matrix() {
    if std::env::var_os("AIKOQL_V2_CACHE_CELLS").is_none() {
        return;
    }
    let mut cells: Vec<String> = Vec::new();
    for &threads in &[1usize, 2, 4, 8, 16, 32] {
        // hot: one block, one reader id, every thread hits it.
        let hot = BlockCache::new(4 * BLOCK);
        hot.insert(1, 0, Arc::new(vec![7u8; BLOCK]));
        cells.push(cell(&hot, threads, "hot", hot_regime));

        // random: 256 blocks under one reader id, LCG-walked per thread.
        let rnd = BlockCache::new(256 * BLOCK);
        for b in 0..256u32 {
            rnd.insert(1, b, Arc::new(vec![b as u8; BLOCK]));
        }
        cells.push(cell(&rnd, threads, "random", random_regime));

        // mixed: 4 reader ids × 64 blocks (the mixed-readers arm).
        let mix = BlockCache::new(64 * BLOCK);
        for id in 0..4u64 {
            for b in 0..64u32 {
                mix.insert(id, b, Arc::new(vec![id as u8; BLOCK]));
            }
        }
        cells.push(cell(&mix, threads, "mixed", mixed_regime));
    }
    let cells_path = dir("m45-cache-cells").join("cells.json");
    std::fs::write(&cells_path, cells.join("\n")).unwrap();
    eprintln!("[m45 cache] {}", cells.join("\n"));
}

fn hot_regime(cache: &BlockCache, _t: u32, ops: usize) {
    for _ in 0..ops {
        assert!(cache.get(1, 0).is_some(), "hot block must hit");
    }
}

fn random_regime(cache: &BlockCache, t: u32, ops: usize) {
    let mut s = 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(t as u64 + 1);
    for _ in 0..ops {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let b = ((s >> 33) % 256) as u32;
        assert!(cache.get(1, b).is_some(), "block {b} must hit");
    }
}

fn mixed_regime(cache: &BlockCache, t: u32, ops: usize) {
    let id = (t % 4) as u64;
    let mut s = 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(t as u64 + 1);
    for _ in 0..ops {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let b = ((s >> 33) % 64) as u32;
        assert!(cache.get(id, b).is_some(), "block {b} must hit");
    }
}

/// One matrix cell: spawn `threads` workers behind a barrier, run the
/// regime, return the JSON cell line.
fn cell(cache: &BlockCache, threads: usize, name: &str, regime: fn(&BlockCache, u32, usize)) -> String {
    cache.set_wait_tracking(true);
    let wait0 = cache.wait_ns();
    let barrier = Arc::new(Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let barrier = barrier.clone();
            let cache = Arc::new(cache.clone());
            thread::spawn(move || {
                barrier.wait();
                let t0 = Instant::now();
                let mut batch0 = Instant::now();
                let mut samples: Vec<u64> = Vec::with_capacity(OPS / 8);
                for i in 0..OPS {
                    regime(&cache, t as u32, 1);
                    if i % 8 == 7 {
                        samples.push(batch0.elapsed().as_nanos() as u64);
                        batch0 = Instant::now();
                    }
                }
                let wall = t0.elapsed().as_nanos() as u64;
                let wait = cache.wait_ns() - wait0;
                (wall, wait, samples)
            })
        })
        .collect();
    let mut walls = Vec::with_capacity(threads);
    let mut waits = Vec::with_capacity(threads);
    let mut all: Vec<u64> = Vec::new();
    for h in handles {
        let (wall, wait, samples) = h.join().expect("worker panicked");
        walls.push(wall);
        waits.push(wait);
        all.extend_from_slice(&samples);
    }
    all.sort_unstable();
    let pct = |p: usize| all[all.len() * p / 100];
    let wall_sum: u64 = walls.iter().sum();
    let wait_sum: u64 = waits.iter().sum();
    let max_wall = *walls.iter().max().expect("at least one thread");
    let throughput = (threads * OPS) as f64 / (max_wall as f64 / 1e9);
    let fraction = wait_sum as f64 / wall_sum as f64;
    format!(
        "{{\"threads\":{threads},\"regime\":\"{name}\",\"ops_s\":{throughput:.0},\
         \"batch_p50_ns\":{},\"batch_p95_ns\":{},\"batch_p99_ns\":{},\
         \"wait_fraction\":{fraction:.4}}}",
        pct(50),
        pct(95),
        pct(99),
    )
}
