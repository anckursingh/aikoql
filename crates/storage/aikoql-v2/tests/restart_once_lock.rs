//! L-13 (TDD-008) — RestartIndex concurrency (P5-M44's OnceLock).
//!
//! One segment, many concurrent readers resolving against the same block:
//! every answer identical, no panic, no corruption, no re-parse of a
//! published table. The parse-count instrument (`restart_parses`, the
//! read-path stats) pins the logical property — the parsed index is
//! published once and then reused.
//!
//! The cold-start storm's bound is [1, N], not == 1: the design accepts
//! the SE2-M4 benign race (every thread that sees the lock empty parses;
//! the losers' copies drop — documented at the parse site). The strict
//! == 1 holds in the sequential leg, where it is a deterministic property;
//! under a simultaneous cold gate it is not, so the pin is the documented
//! tolerance plus identical answers. The value of the duplicate-parse
//! tolerance is the P5-M44 representation benchmark (compact blob vs
//! per-key boxes) — the code comment cites it.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use common::dir;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

/// Reader threads in the storm legs.
const N: usize = 8;
/// Gets per thread per storm.
const GETS: usize = 200;

fn key(i: u32) -> Vec<u8> {
    format!("k/{i:03}").into_bytes()
}

fn val(i: u32) -> Vec<u8> {
    format!("v/{i:03}").into_bytes()
}

/// 100 keys, one flush — one segment, one data block (the default 16 KiB
/// target holds them, restarts at every 16th entry), so every reader
/// resolves against the SAME block's OnceLock.
fn seeded(dir: &Path) -> Db {
    let db = Db::open(Config::new(dir.to_path_buf())).unwrap();
    for i in 0..100u32 {
        db.put(&key(i), &val(i)).unwrap();
    }
    db.flush().unwrap();
    db
}

/// The sequential property: a cold lookup parses the table exactly once,
/// and every later lookup reuses it — zero new parses.
#[test]
fn sequential_cold_read_parses_once_then_reuses() {
    let d = dir("l13-seq");
    let db = seeded(&d);
    let before = db.read_path_stats();
    assert_eq!(db.get(&key(15)).unwrap(), Some(val(15)));
    let delta = common::stats_delta(db.read_path_stats(), before);
    assert_eq!(
        delta.restart_parses, 1,
        "the cold lookup parses the table exactly once"
    );
    let before = db.read_path_stats();
    for i in 0..100u32 {
        assert_eq!(db.get(&key(i)).unwrap(), Some(val(i)));
        assert_eq!(db.get(&key(i)).unwrap(), Some(val(i)));
    }
    let delta = common::stats_delta(db.read_path_stats(), before);
    assert_eq!(
        delta.restart_parses, 0,
        "a published table is reused, never re-parsed"
    );
}

/// Concurrent readers on a warm block: identical answers from every
/// thread, no panic, no re-parse.
#[test]
fn concurrent_warm_readers_answer_identically_without_reparsing() {
    let d = dir("l13-warm");
    let db = Arc::new(seeded(&d));
    // warm the block (one parse), then release the storm
    assert_eq!(db.get(&key(0)).unwrap(), Some(val(0)));
    let before = db.read_path_stats();
    let barrier = Arc::new(Barrier::new(N));
    let mut handles = Vec::new();
    for t in 0..N {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for g in 0..GETS {
                let i = ((t * 7 + g * 13) % 100) as u32;
                let got = db.get(&key(i)).unwrap();
                assert_eq!(
                    got.as_deref(),
                    Some(val(i).as_slice()),
                    "thread {t} get {g}"
                );
            }
        }));
    }
    for h in handles {
        h.join().expect("a reader thread panicked");
    }
    let delta = common::stats_delta(db.read_path_stats(), before);
    assert_eq!(delta.lookups, (N * GETS) as u64, "every storm get counted");
    assert_eq!(
        delta.restart_parses, 0,
        "warm readers reuse the published index"
    );
}

/// Concurrent readers on a COLD block (fresh handle — the OnceLocks are
/// empty): identical answers, no panic, and the parse count lands inside
/// the documented benign race [1, N]. A reopen afterwards proves the
/// storm corrupted nothing.
#[test]
fn concurrent_cold_readers_stay_within_the_documented_parse_bound() {
    let d = dir("l13-cold");
    seeded(&d);
    drop(seeded(&d)); // the fixture above built the segment; reopen cold
    let db = Arc::new(Db::open(Config::new(d.clone())).unwrap());
    let barrier = Arc::new(Barrier::new(N));
    let mut handles = Vec::new();
    for _ in 0..N {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            // every thread resolves the SAME key — the same block's table
            for _ in 0..GETS {
                let got = db.get(&key(16)).unwrap();
                assert_eq!(got.as_deref(), Some(val(16).as_slice()));
            }
        }));
    }
    for h in handles {
        h.join().expect("a reader thread panicked");
    }
    let parses = db.read_path_stats().restart_parses;
    assert!(
        (1..=N as u64).contains(&parses),
        "cold parses {parses} outside the documented [1, {N}] benign race"
    );
    drop(db);
    // the storm answered identically every time (asserted in the threads);
    // the reopen proves zero corruption
    let db = Db::open(Config::new(d)).unwrap();
    for i in 0..100u32 {
        assert_eq!(db.get(&key(i)).unwrap(), Some(val(i)), "post-storm key {i}");
    }
}
