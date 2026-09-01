//! SE2-M6 — group commit REDs (design §7): a commit queue drains into
//! groups bounded by `max_batch_ops` / `max_batch_bytes` / `max_wait_duration`
//! and commits each group with ONE fsync, then applies and acks (never ack
//! before apply). Sync mode stays the correctness baseline — its WAL bytes
//! are the golden the group-commit path must reproduce exactly.

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode, WAL_FILE};
use aikoql_storage_v2::wal::{replay_frames, Op};
use common::dir;
use std::path::PathBuf;
use std::time::Duration;

fn gc_config(dir: PathBuf) -> Config {
    let mut c = Config::new(dir);
    c.durability = DurabilityMode::GroupCommit; // explicit opt-in, never silent
    c
}

/// The engine's byte accounting: the sum over ops of key+value bytes
/// (a Delete carries only its key).
fn batch_bytes(ops: &[Op]) -> usize {
    ops.iter()
        .map(|op| match op {
            Op::Put(k, v) => k.len() + v.len(),
            Op::Delete(k) => k.len(),
        })
        .sum()
}

#[test]
fn group_commit_coalesces_batches_into_one_fsync() {
    let d = dir("gc-coalesce");
    let mut cfg = gc_config(d.clone());
    cfg.max_wait_duration = Duration::from_millis(200); // long window
    let db = Db::open(cfg).unwrap();
    let writer = db.writer().unwrap();
    // 8 concurrent writers queue within microseconds of each other — the
    // window is 200 ms, so the drain must take every batch into ONE group.
    // (A single synchronous submitter cannot coalesce by construction: its
    // write blocks until the ack, so the queue never holds two batches.)
    let threads: Vec<_> = (0..8u64)
        .map(|i| {
            let writer = writer.clone();
            std::thread::spawn(move || {
                writer
                    .write(&[Op::Put(
                        format!("k{i}").into_bytes(),
                        format!("v{i}").into_bytes(),
                    )])
                    .unwrap()
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(
        db.fsync_count(),
        1,
        "one fsync for the whole group, not one per batch"
    );
    // Ack implies apply: each write returned only after its ack, and every
    // key is already visible.
    for i in 0..8u64 {
        assert_eq!(
            db.get(&format!("k{i}").into_bytes()).unwrap(),
            Some(format!("v{i}").into_bytes())
        );
    }
    drop(writer);
    drop(db);
    let db = Db::open(gc_config(d.clone())).unwrap();
    for i in 0..8u64 {
        assert_eq!(
            db.get(&format!("k{i}").into_bytes()).unwrap(),
            Some(format!("v{i}").into_bytes())
        );
    }
}

#[test]
fn group_commit_respects_max_batch_ops() {
    let d = dir("gc-cap-ops");
    let mut cfg = gc_config(d.clone());
    cfg.max_wait_duration = Duration::from_millis(200);
    cfg.max_batch_ops = 4;
    let db = Db::open(cfg).unwrap();
    let writer = db.writer().unwrap();
    // 10 concurrent one-op batches with cap 4 → exact-fit groups 4/4/2:
    // any packing of 10 into groups of ≤4 needs at least 3 groups, and the
    // 200 ms window never expires mid-burst, so exactly 3. Concurrent
    // submitters are what group commit is FOR — a single synchronous
    // submitter's write blocks until its ack, so nothing can coalesce.
    let threads: Vec<_> = (0..10u64)
        .map(|i| {
            let writer = writer.clone();
            std::thread::spawn(move || {
                writer
                    .write(&[Op::Put(
                        format!("k{i}").into_bytes(),
                        format!("v{i}").into_bytes(),
                    )])
                    .unwrap()
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(db.fsync_count(), 3, "cap 4 over 10 batches → 3 groups");
    drop(writer);
    drop(db);
    let db = Db::open(gc_config(d)).unwrap();
    for i in 0..10u64 {
        assert_eq!(
            db.get(&format!("k{i}").into_bytes()).unwrap(),
            Some(format!("v{i}").into_bytes())
        );
    }
}

#[test]
fn group_commit_respects_max_batch_bytes() {
    let d = dir("gc-cap-bytes");
    let mut cfg = gc_config(d.clone());
    cfg.max_wait_duration = Duration::from_millis(200);
    cfg.max_batch_bytes = 250;
    let db = Db::open(cfg).unwrap();
    let writer = db.writer().unwrap();
    // 100-byte batches with cap 250 → exact-fit groups of 2 (200 ≤ 250,
    // 300 > 250) → 5 groups. Concurrent submitters, same reasoning as the
    // ops-cap test.
    let v = vec![b'x'; 90];
    let threads: Vec<_> = (0..10u64)
        .map(|i| {
            let writer = writer.clone();
            let v = v.clone();
            std::thread::spawn(move || {
                let ops = [Op::Put(format!("key-{i:06}").into_bytes(), v)];
                assert_eq!(batch_bytes(&ops), 100, "the pin assumes 100-byte batches");
                writer.write(&ops).unwrap()
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(
        db.fsync_count(),
        5,
        "cap 250 over ten 100-byte batches → groups of 2"
    );
    drop(writer);
    drop(db);
    let db = Db::open(gc_config(d)).unwrap();
    for i in 0..10u64 {
        assert_eq!(
            db.get(&format!("key-{i:06}").into_bytes()).unwrap(),
            Some(v.clone())
        );
    }
}

#[test]
fn ack_after_apply_and_orders_hold() {
    let d = dir("gc-order");
    let mut cfg = gc_config(d.clone());
    cfg.max_wait_duration = Duration::ZERO; // every batch its own group
    let db = Db::open(cfg).unwrap();
    let writer = db.writer().unwrap();
    let mut last_seq = 0;
    for i in 1..=20u64 {
        let ops = [Op::Put(
            format!("k{i:02}").into_bytes(),
            format!("v{i:02}").into_bytes(),
        )];
        let seq = writer.write(&ops).unwrap();
        // Ack order == apply order == log order: the seq strictly
        // increases in submission order...
        assert!(seq > last_seq, "seq {seq} after {last_seq}");
        assert_eq!(seq, i, "seqs are 1..=20 in submission order");
        last_seq = seq;
        // ...and the ack is only sent AFTER the apply — the just-acked
        // batch is already visible.
        assert_eq!(
            db.get(&format!("k{i:02}").into_bytes()).unwrap(),
            Some(format!("v{i:02}").into_bytes()),
            "batch {i} visible immediately after its ack"
        );
    }
    drop(writer);
    drop(db);

    // Log order: the WAL holds 20 frames with seqs 1..=20 in order.
    let wal_bytes = std::fs::read(d.join(WAL_FILE)).unwrap();
    let (frames, consumed) = replay_frames(&wal_bytes).unwrap();
    assert_eq!(consumed, wal_bytes.len(), "no torn tail after clean ack");
    assert_eq!(frames.len(), 20);
    for (i, frame) in frames.iter().enumerate() {
        let i = i as u64 + 1;
        assert_eq!(frame.seq, i, "WAL frames strictly ordered by seq");
        assert_eq!(
            frame.ops,
            vec![Op::Put(
                format!("k{i:02}").into_bytes(),
                format!("v{i:02}").into_bytes()
            )],
            "frame {i} payload byte-exact"
        );
    }

    let db = Db::open(gc_config(d)).unwrap();
    for i in 1..=20u64 {
        assert_eq!(
            db.get(&format!("k{i:02}").into_bytes()).unwrap(),
            Some(format!("v{i:02}").into_bytes())
        );
    }
}

/// The deterministic 50-op workload both modes must commit identically.
fn workload() -> Vec<Op> {
    let mut ops = Vec::new();
    for i in 0..50u64 {
        ops.push(match i % 5 {
            0 => Op::Put(
                format!("k{:03}", i % 20).into_bytes(),
                format!("v{i:03}").into_bytes(),
            ),
            1 => Op::Put(
                format!("k{:03}", i % 20).into_bytes(),
                format!("w{i:03}").into_bytes(),
            ),
            2 => Op::Delete(format!("k{:03}", (i + 3) % 20).into_bytes()),
            _ => Op::Put(
                format!("k{:03}", i % 20).into_bytes(),
                format!("u{i:03}").into_bytes(),
            ),
        });
    }
    ops
}

#[test]
fn sync_and_group_commit_wals_are_byte_identical() {
    let d_sync = dir("gc-parity-sync");
    {
        let mut db = Db::open(Config::new(d_sync.clone())).unwrap();
        for op in &workload() {
            db.write(std::slice::from_ref(op)).unwrap();
        }
    }
    let d_gc = dir("gc-parity-gc");
    {
        let mut cfg = gc_config(d_gc.clone());
        cfg.max_wait_duration = Duration::ZERO;
        let db = Db::open(cfg).unwrap();
        let writer = db.writer().unwrap();
        for op in &workload() {
            writer.write(std::slice::from_ref(op)).unwrap();
        }
        drop(writer);
    }

    // The WAL bytes are the durability contract: Sync assigns per-batch
    // seqs and appends frames; group commit must produce the exact same
    // frames — same seqs, same payloads, byte for byte.
    let sync_wal = std::fs::read(d_sync.join(WAL_FILE)).unwrap();
    let gc_wal = std::fs::read(d_gc.join(WAL_FILE)).unwrap();
    assert_eq!(
        sync_wal, gc_wal,
        "group commit must not change a single WAL byte"
    );

    // Same state, too.
    let db_sync = Db::open(Config::new(d_sync)).unwrap();
    let db_gc = Db::open(gc_config(d_gc)).unwrap();
    for i in 0..20u64 {
        let k = format!("k{i:03}").into_bytes();
        assert_eq!(db_sync.get(&k).unwrap(), db_gc.get(&k).unwrap());
    }
}

// ---------------------------------------------------------------------------
// SE2M6_NIGHTLY — the throughput matrix (KSE-120C shape). Strict opt-in:
// unset skips, any value other than "1" panics. Perf numbers are report
// cells, never asserts; the report regenerates only with the env set.
// ---------------------------------------------------------------------------

const GATE: &str = "SE2M6_NIGHTLY";

fn nightly_on() -> bool {
    match std::env::var(GATE) {
        Err(_) => false,
        Ok(v) if v == "1" => true,
        Ok(v) => panic!("{GATE} must be unset or \"1\", got {v:?} (strict opt-in)"),
    }
}

#[test]
fn group_commit_throughput_matrix() {
    if !nightly_on() {
        eprintln!("SKIPPED (set SE2M6_NIGHTLY=1 to run the throughput matrix)");
        return;
    }
    const BATCHES: usize = 200;
    let batches: Vec<Op> = (0..BATCHES as u64)
        .map(|i| Op::Put(format!("key-{i:04}").into_bytes(), vec![b'x'; 128]))
        .collect();
    let mut report = String::new();

    // (a) Sync baseline: single writer, one fsync per batch.
    let d = dir("gc-perf-sync");
    let t0 = std::time::Instant::now();
    let sync_fsyncs = {
        let mut db = Db::open(Config::new(d.clone())).unwrap();
        for op in &batches {
            db.write(std::slice::from_ref(op)).unwrap();
        }
        db.fsync_count()
    };
    let sync_ms = t0.elapsed().as_millis();
    report.push_str(&format!(
        "- Sync, 1 writer, {BATCHES} batches: {sync_ms} ms, {sync_fsyncs} fsyncs\n"
    ));

    // (b) Group commit, single writer, no wait window.
    let d = dir("gc-perf-gc1");
    let t0 = std::time::Instant::now();
    let gc1_fsyncs = {
        let mut cfg = gc_config(d.clone());
        cfg.max_wait_duration = Duration::ZERO;
        let db = Db::open(cfg).unwrap();
        let writer = db.writer().unwrap();
        for op in &batches {
            writer.write(std::slice::from_ref(op)).unwrap();
        }
        let n = db.fsync_count();
        drop(writer);
        n
    };
    let gc1_ms = t0.elapsed().as_millis();
    report.push_str(&format!(
        "- GroupCommit, 1 writer, wait=0, {BATCHES} batches: {gc1_ms} ms, {gc1_fsyncs} fsyncs\n"
    ));

    // (c) Group commit, 8 concurrent writers, 5 ms wait window.
    let d = dir("gc-perf-gc8");
    let t0 = std::time::Instant::now();
    let gc8_fsyncs = {
        let mut cfg = gc_config(d.clone());
        cfg.max_wait_duration = Duration::from_millis(5);
        let db = Db::open(cfg).unwrap();
        let writers: Vec<_> = (0..8).map(|_| db.writer().unwrap()).collect();
        let mut threads = Vec::new();
        for writer in &writers {
            let writer = writer.clone();
            let batches: Vec<Op> = batches.clone();
            threads.push(std::thread::spawn(move || {
                for op in &batches {
                    writer.write(std::slice::from_ref(op)).unwrap();
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        let n = db.fsync_count();
        drop(writers);
        n
    };
    let gc8_ms = t0.elapsed().as_millis();
    report.push_str(&format!(
        "- GroupCommit, 8 writers × {} batches, wait=5ms: {gc8_ms} ms, {gc8_fsyncs} fsyncs\n",
        BATCHES / 8
    ));
    // Correctness under load still holds: every acked batch is present.
    let db = Db::open(gc_config(d)).unwrap();
    for (i, op) in batches.iter().enumerate() {
        let Op::Put(k, v) = op else { unreachable!() };
        assert_eq!(
            db.get(k).unwrap(),
            Some(v.clone()),
            "acked batch {i} lost under 8-writer load"
        );
    }

    let report = format!(
        "# Group Commit Throughput Matrix — SE2-M6\n\n\
         Generated only when `SE2M6_NIGHTLY=1` (strict opt-in). Perf numbers are\n\
         report cells, never asserts — the report regenerates only with the env set.\n\n\
         - Test: `group_commit_throughput_matrix`\n\
         - Build mode: {}\n\
         - Workload: {BATCHES} single-op batches, 128-byte values, 1 MiB+ memtable (no flush during the run)\n\n{}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        report
    );
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("artifacts")
        .join("storage-engine-v2");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("group-commit.md"), report).unwrap();
}
