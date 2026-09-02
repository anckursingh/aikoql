//! SE2-M8 — read-path instrumentation (QA spec M0, TC-PERF-0001..0003).
//!
//! The counters must move only with real operations — a metric that never
//! moves fails the pin (the QA doc's "not synthetic" rule). The bloom-skip
//! scenario is deterministic by construction: 1-entry segments give m = 10
//! bits (m = 10·n, the M1 spec); the nine filler keys are chosen so all four
//! of their probe positions sit below bit 5, and the target probes at least
//! one bit ≥ 5 — so the fillers' blooms provably reject the target, and the
//! exact 9-skipped / 1-searched split is not a probability.

mod common;

use aikoql_kernel::knowledge::kom::sha256;
use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::stats::ReadPathStats;
use common::dir;

/// The M1 bloom spec (m = 10·n, 4 probes, double hashing over sha256),
/// computed independently — the test's expectation must not be the
/// engine's code.
fn probes(key: &[u8], m: u32) -> [u32; 4] {
    let d = sha256(key);
    let h1 = u64::from_le_bytes(d[..8].try_into().expect("sha256 len"));
    let h2 = u64::from_le_bytes(d[8..16].try_into().expect("sha256 len"));
    let mut p = [0u32; 4];
    for (i, slot) in p.iter_mut().enumerate() {
        *slot = ((h1 as u128 + i as u128 * h2 as u128) % m as u128) as u32;
    }
    p
}

/// A key whose bloom (m = 10) rejects any target that probes bit ≥ 5: all
/// four of its own probe positions sit in the low half. The search is
/// bounded and deterministic (sha256 is).
fn low_half_key(seed: u32) -> String {
    for i in 0..10_000u32 {
        let k = format!("f{seed}-{i}");
        if probes(k.as_bytes(), 10).iter().all(|p| *p < 5) {
            return k;
        }
    }
    panic!("no low-half filler found for seed {seed}");
}

/// A target that provably probes the high half at least once.
fn high_bit_target() -> String {
    for i in 0..10_000u32 {
        let k = format!("t{i}");
        if probes(k.as_bytes(), 10).iter().any(|p| *p >= 5) {
            return k;
        }
    }
    panic!("no high-bit target found");
}

/// Nine one-key segments whose blooms reject `target` by construction, plus
/// the segment that holds it. Flush is explicit (memtable threshold off).
fn ten_segments(tag: &str) -> (Db, String) {
    let target = high_bit_target();
    let mut cfg = Config::new(dir(tag));
    cfg.memtable_bytes = usize::MAX;
    cfg.block_target = 256;
    let mut db = Db::open(cfg).unwrap();
    // Target FIRST so it lives in the oldest segment: get walks newest-first,
    // so the nine filler segments are bloom-rejected before the hit.
    db.put(target.as_bytes(), &[b'v'; 200][..]).unwrap();
    db.flush().unwrap();
    for i in 0..9 {
        let filler = low_half_key(i);
        db.put(filler.as_bytes(), &[b'v'; 200][..]).unwrap();
        db.flush().unwrap();
    }
    (db, target)
}

#[test]
fn instrumented_point_lookup_populates_metrics() {
    let (db, target) = ten_segments("perf-0001");
    let zero = db.read_path_stats();
    assert_eq!(
        zero,
        ReadPathStats::default(),
        "fresh stats must be all-zero — counters move only with real operations"
    );
    assert_eq!(
        db.get(target.as_bytes()).unwrap().as_deref(),
        Some(&[b'v'; 200][..])
    );
    let s = db.read_path_stats();
    assert_eq!(s.lookups, 1);
    assert_eq!(
        s.segments_considered, 10,
        "every segment is iterated (candidate selection is a later milestone)"
    );
    assert_eq!(s.blocks_read, 1, "exactly one physical block read");
    assert!(s.bytes_read > 0, "bytes_read must count the real read");
    assert!(s.entries_decoded >= 1, "the winning entry was decoded");
    assert!(s.block_io_ns > 0, "block_io_ns must measure a real read");
    assert!(
        s.memtable_lookup_ns > 0,
        "memtable_lookup_ns must measure the real memtable probe"
    );
}

#[test]
fn bloom_skip_evidence() {
    let (db, target) = ten_segments("perf-0002");
    db.get(target.as_bytes()).unwrap();
    let s = db.read_path_stats();
    assert_eq!(
        s.segments_bloom_skipped, 9,
        "the nine non-matching segments are bloom-rejected by construction"
    );
    assert_eq!(
        s.segments_index_searched, 1,
        "only the containing segment searches its block index"
    );
}

#[test]
fn cache_hit_skips_physical_io() {
    let (db, target) = ten_segments("perf-0003");
    db.get(target.as_bytes()).unwrap();
    let first = db.read_path_stats();
    assert_eq!(first.blocks_read, 1);
    assert!(
        first.block_cache_misses >= 1,
        "the cold read missed the cache"
    );
    db.get(target.as_bytes()).unwrap();
    let second = db.read_path_stats();
    assert!(second.block_cache_hits >= 1, "the warm read hit the cache");
    assert_eq!(
        second.blocks_read, first.blocks_read,
        "a cached hit performs no second physical block read"
    );
    assert_eq!(
        second.bytes_read, first.bytes_read,
        "a cached hit reads no further bytes"
    );
}
