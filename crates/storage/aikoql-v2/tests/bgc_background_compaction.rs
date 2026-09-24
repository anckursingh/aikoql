//! P3-M8 — background compaction + hard-bound backpressure (§70–71).
//! The L0 trigger gates stay on the write path (the M2 gauge scan), but the
//! merge itself moves to a compactor thread: the write that crosses the
//! trigger returns without waiting (bgc001), and the background end state
//! is byte-equal to the synchronous oracle (bgc002). When the L0 backlog
//! crosses the hard bound while a merge is already in flight, writes block
//! until the compactor drains it (bgc003) — and a failed merge clears the
//! block, so the write path can never deadlock behind a doomed merge
//! (bgc003b). A kill mid-background-merge recovers the same logical state
//! (bgc004), and a randomized flush+compact interleave loses no keys
//! against an in-memory oracle (bgc005). Park windows reuse the SE2-M4
//! child-kill harness.

mod common;

use aikoql_storage_v2::db::{manifest_path, segment_path, Config, Db};
use aikoql_storage_v2::format::{Current, Manifest};
use common::dir;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_ENV: &str = "AIKOQL_V2_KILL_CHILD";
const DIR_ENV: &str = "AIKOQL_V2_KILL_DIR";
const PARK_ENV: &str = "AIKOQL_V2_COMPACT_PARK";
/// Tiny on purpose: any L0 pile of real segments crosses it, so the
/// backpressure gate is what the tests exercise, not its calibration.
const HARD_BOUND: u64 = 64;

fn child_dir() -> PathBuf {
    PathBuf::from(std::env::var(DIR_ENV).expect("child dir env"))
}

fn spawn_child(test_name: &str, dir: &Path, stage: &str) -> Child {
    Command::new(std::env::current_exe().expect("current exe"))
        .arg("--exact")
        .arg(test_name)
        .env(CHILD_ENV, "1")
        .env(DIR_ENV, dir)
        .env(PARK_ENV, stage)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child")
}

fn wait_for(path: &Path, timeout: Duration) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < timeout,
            "marker {} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn marker(path: &Path) {
    std::fs::write(path, b"1").expect("write marker");
}

/// One flush per round: 8 puts x ~64 B crosses a 512-byte memtable from
/// empty, exactly one L0 segment per round (the db_tiered_compact fixture).
/// All keys distinct across rounds.
fn round_put(db: &Db, r: usize) {
    for i in 0..8 {
        let k = format!("k{r:03}{i:02}").into_bytes();
        let v = format!("v{r:03}{i:02}{}", "y".repeat(34)).into_bytes();
        db.put(&k, &v).unwrap();
    }
}

fn round_value(r: usize, i: usize) -> Vec<u8> {
    format!("v{r:03}{i:02}{}", "y".repeat(34)).into_bytes()
}

/// xorshift64 — deterministic across runs (bgc005).
fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut s = seed;
    move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    }
}

/// The logical disk state: SEGMENT-*, MANIFEST-* and CURRENT, sorted by
/// name. WAL/checkpoint/log files carry wall-clock bookkeeping — excluded.
fn disk_state(d: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(d)
        .unwrap()
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with("SEGMENT-") || n.starts_with("MANIFEST-") || n == "CURRENT"
        })
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn bgc001_triggering_write_returns_without_the_merge() {
    // dir() AFTER the child branch (compact_crash's rule): the child would
    // otherwise create its own pid-namespaced dir and, being hard-killed,
    // never sweep it.
    if std::env::var_os(CHILD_ENV).is_some() {
        let mut cfg = Config::new(child_dir());
        cfg.memtable_bytes = 512;
        let db = Db::open(cfg).unwrap();
        // Four rounds = four flushes: the 4th crosses l0_compact_trigger
        // (default 4) and kicks the background merge, which parks at
        // after_segment on the compactor thread. The crossing put must
        // return without waiting on the merge — reaching the marker IS the
        // assertion (inline synchronous compaction would block here and
        // the parent's marker wait would time out).
        for r in 1..=4 {
            round_put(&db, r);
        }
        marker(&child_dir().join("returned"));
        std::thread::sleep(Duration::from_secs(3600));
        unreachable!("the parent kills the parked child");
    }
    let d = dir("bgc001");
    let mut child = spawn_child(
        "bgc001_triggering_write_returns_without_the_merge",
        &d,
        "after_segment",
    );
    wait_for(&d.join("after_segment"), Duration::from_secs(60));
    wait_for(&d.join("returned"), Duration::from_secs(60));
    child.kill().expect("kill child");
    child.wait().expect("wait child");

    // Reopen after the kill: every acked put survives the mid-merge crash.
    let db = Db::open(Config::new(d.clone())).unwrap();
    for r in 1..=4 {
        for i in 0..8 {
            let k = format!("k{r:03}{i:02}").into_bytes();
            assert_eq!(db.get(&k).unwrap(), Some(round_value(r, i)));
        }
    }
    for probe in 0..16u64 {
        assert_eq!(
            db.get(format!("z{probe:03}").as_bytes()).unwrap(),
            None,
            "phantom key {probe}"
        );
    }
    assert_eq!(db.put(b"next", b"x").unwrap(), 33, "no acked write lost");
}

#[test]
fn bgc002_background_end_state_byte_equals_synchronous_oracle() {
    // The synchronous oracle first: the same four rounds with the
    // compactor off — its flush-4 merge runs inline on the write path.
    let sync = dir("bgc002-sync");
    let mut cfg = Config::new(sync.clone());
    cfg.memtable_bytes = 512;
    cfg.compact_background = false;
    let db = Db::open(cfg).unwrap();
    for r in 1..=4 {
        round_put(&db, r);
    }
    drop(db);

    // The background run (defaults): the flush-4 merge moves to the
    // compactor; wait_compactor_idle drains it before the compare.
    let bg = dir("bgc002-bg");
    let mut cfg = Config::new(bg.clone());
    cfg.memtable_bytes = 512;
    let db = Db::open(cfg).unwrap();
    for r in 1..=4 {
        round_put(&db, r);
    }
    db.wait_compactor_idle();
    drop(db);

    assert_eq!(disk_state(&sync), disk_state(&bg));
}

#[test]
fn bgc003_backlog_over_hard_bound_blocks_writes() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let mut cfg = Config::new(child_dir());
        cfg.memtable_bytes = 512;
        cfg.l0_compact_trigger = 2;
        cfg.backlog_hard_bound_bytes = HARD_BOUND;
        let db = Db::open(cfg).unwrap();
        round_put(&db, 1); // flush 1: L0 = 1 < 2, no trigger
        round_put(&db, 2); // flush 2: crosses — kicks the merge, returns
        marker(&child_dir().join("kicked"));
        round_put(&db, 3); // merge parked in flight + L0 > bound: blocks
        marker(&child_dir().join("round3-done"));
        std::thread::sleep(Duration::from_secs(3600));
        unreachable!("the parent kills the parked child");
    }
    let d = dir("bgc003");
    let mut child = spawn_child(
        "bgc003_backlog_over_hard_bound_blocks_writes",
        &d,
        "after_segment",
    );
    wait_for(&d.join("after_segment"), Duration::from_secs(60));
    wait_for(&d.join("kicked"), Duration::from_secs(60));
    // The parked merge holds the compactor; round 3 must still be blocked.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !d.join("round3-done").exists(),
        "the round-3 write returned while the merge was parked over the bound"
    );
    child.kill().expect("kill child");
    child.wait().expect("wait child");
}

#[test]
fn bgc003b_failed_merge_clears_the_backpressure() {
    let d = dir("bgc003b");
    let mut cfg = Config::new(d.clone());
    cfg.memtable_bytes = 512;
    cfg.l0_compact_trigger = 1;
    cfg.backlog_hard_bound_bytes = HARD_BOUND;
    cfg.cache_bytes = 0; // the merge must read the garbaged file, not the cache
    let db = Db::open(cfg).unwrap();
    round_put(&db, 1); // flush 1: one segment — below the merge floor
    round_put(&db, 2); // flush 2: crosses (two segments) — merges
    db.wait_compactor_idle();
    assert_eq!(db.stats().write.compaction_error_count, 0);
    round_put(&db, 3); // one L0 segment alongside the L1

    // Garbage the L0 segment ON DISK, same length: the flush-time size
    // validation (P4-M3, manifest vs file) still passes, but the merge's
    // block decode fails closed — the failure lands where the test needs
    // it, on the compactor.
    let current = Current::read(&d.join("CURRENT")).unwrap();
    let manifest = Manifest::read(&manifest_path(&d, current.manifest_generation)).unwrap();
    let l0 = manifest
        .segments
        .iter()
        .find(|r| r.level == 0)
        .expect("one L0 segment");
    let garbage = vec![0x5A; l0.file_size as usize];
    std::fs::write(segment_path(&d, l0.segment_id), garbage).unwrap();

    round_put(&db, 4); // kick: the merge fails on the garbaged segment
    db.wait_compactor_idle();
    assert_eq!(
        db.stats().write.compaction_error_count,
        1,
        "one failed background merge recorded"
    );
    assert!(db.last_compaction_error().is_some());

    // Liveness: with the hard bound tiny and every retry failing, the
    // write path must still drain — a write that arrives while a doomed
    // merge is in flight blocks at most until that merge fails.
    for r in 5..=7 {
        round_put(&db, r);
    }
    assert!(db.get(b"k00700").unwrap().is_some());
    assert!(db.stats().write.compaction_error_count >= 1);
}

#[test]
fn bgc004_kill_during_background_merge_recovers_consistent() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let mut cfg = Config::new(child_dir());
        cfg.memtable_bytes = 1 << 30; // nothing flushes mid-workload
        cfg.l0_compact_trigger = 1;
        let db = Db::open(cfg).unwrap();
        // All 120 ops ack into the memtable/WAL, split across two
        // explicit flushes (two segments) — nothing triggers mid-workload.
        for (i, (put, k, v)) in ops().into_iter().enumerate() {
            if put {
                db.put(&k, &v).unwrap();
            } else {
                db.delete(&k).unwrap();
            }
            if i == 59 {
                db.flush().unwrap();
            }
        }
        db.flush().unwrap();
        // The crossing write kicks the background merge (2 >= 1) and
        // returns; the merge parks at after_segment on the compactor.
        db.put(b"trigger", b"x").unwrap();
        std::thread::sleep(Duration::from_secs(3600));
        unreachable!("the parent kills the parked child");
    }
    let d = dir("bgc004");
    let mut child = spawn_child(
        "bgc004_kill_during_background_merge_recovers_consistent",
        &d,
        "after_segment",
    );
    wait_for(&d.join("after_segment"), Duration::from_secs(60));
    child.kill().expect("kill child");
    child.wait().expect("wait child");

    // Zero loss: every acked op plus the triggering put survive the crash
    // mid-merge — the pre-merge manifest still governs the two flushed
    // segments and the WAL carries the memtable tail.
    let db = Db::open(Config::new(d.clone())).unwrap();
    for (k, want) in &expected() {
        assert_eq!(
            db.get(k).unwrap(),
            *want,
            "key {:?} diverged",
            String::from_utf8_lossy(k)
        );
    }
    assert_eq!(db.get(b"trigger").unwrap(), Some(b"x".to_vec()));
    for probe in 0..30u64 {
        assert_eq!(
            db.get(format!("z{probe:03}").as_bytes()).unwrap(),
            None,
            "phantom key {probe}"
        );
    }
    assert_eq!(db.put(b"next", b"x").unwrap(), 122, "no acked write lost");
}

#[test]
fn bgc005_randomized_interleave_matches_oracle() {
    let d = dir("bgc005");
    let mut cfg = Config::new(d.clone());
    cfg.memtable_bytes = 512;
    let db = Db::open(cfg).unwrap();
    let mut next = rng(42);
    let mut oracle: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();
    for i in 0..2000 {
        let k = format!("k{:03}", next() % 200).into_bytes();
        if next() % 10 < 7 {
            let v = format!("v{i:04}").into_bytes();
            oracle.insert(k.clone(), Some(v.clone()));
            db.put(&k, &v).unwrap();
        } else {
            oracle.insert(k.clone(), None);
            db.delete(&k).unwrap();
        }
        if i % 50 == 49 {
            db.flush().unwrap();
        }
        if i % 200 == 199 {
            db.compact().unwrap(); // explicit: synchronous, both modes
        }
    }
    db.flush().unwrap();
    db.wait_compactor_idle();
    drop(db);

    // Zero lost keys against the oracle across a reopen.
    let db = Db::open(Config::new(d.clone())).unwrap();
    for (k, want) in &oracle {
        assert_eq!(
            db.get(k).unwrap(),
            *want,
            "key {:?} diverged",
            String::from_utf8_lossy(k)
        );
    }
    for probe in 0..30u64 {
        assert_eq!(
            db.get(format!("z{probe:03}").as_bytes()).unwrap(),
            None,
            "phantom key {probe}"
        );
    }
}

// — bgc004 workload: the compact_crash fixture (120 single-op batches
// over 40 hot keys) with its oracle. —

fn ops() -> Vec<(bool, Vec<u8>, Vec<u8>)> {
    let mut next = rng(7);
    (0..120)
        .map(|i| {
            let k = format!("k{:03}", next() % 40).into_bytes();
            match next() % 10 {
                0..=6 => (true, k, format!("v{i:03}").into_bytes()),
                _ => (false, k, Vec::new()),
            }
        })
        .collect()
}

fn expected() -> HashMap<Vec<u8>, Option<Vec<u8>>> {
    let mut m = HashMap::new();
    for (put, k, v) in ops() {
        m.insert(k, if put { Some(v) } else { None });
    }
    m
}
