//! P3-M2 (met001–003) — write-path observability (design §21): the counters
//! must move EXACTLY with real operations, never with evaluation. met001
//! pins the deterministic ledger (wal_bytes deltas, flush_count); met002 pins
//! the fsync-latency histogram's Sync/Async split; met003 pins the compaction
//! backlog gauges and the checkpoint_now admin surface.

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use aikoql_storage_v2::wal::Op;

fn config(tag: &str) -> Config {
    let mut c = Config::new(common::tmp(tag));
    c.memtable_bytes = usize::MAX; // no auto flush
    c.l0_compact_trigger = 0; // no auto compact
    c.checkpoint_bytes = 0; // no auto checkpoint
    c
}

/// met001 — N writes move wal_bytes and flush_count EXACTLY: identical frames
/// append identical byte deltas (cumulative across flushes), and a flush
/// fires only when the memtable threshold is actually crossed.
#[test]
fn met001_wal_bytes_and_flush_count_move_exactly() {
    let db = Db::open(config("met001")).unwrap();
    let s0 = db.stats();

    // Phase 1: no flush possible — every frame lands in the WAL ledger.
    let v = vec![0x5a; 300];
    db.write(&[Op::Put(b"k1".to_vec(), v.clone())]).unwrap();
    let s1 = db.stats();
    db.write(&[Op::Put(b"k1".to_vec(), v.clone())]).unwrap();
    let s2 = db.stats();

    let d1 = s2.write.wal_bytes - s1.write.wal_bytes;
    let d0 = s1.write.wal_bytes - s0.write.wal_bytes;
    assert!(
        d0 > 0,
        "wal_bytes must count the first frame: {s0:?} {s1:?}"
    );
    assert_eq!(d0, d1, "identical frames append identical bytes");
    assert_eq!(
        s2.write.flush_count, 0,
        "no flush without a threshold cross"
    );
    assert_eq!(s2.write.fsync_count, 2, "Sync: one fsync per batch");

    // Phase 2: 512 B memtable, 300 B values — a flush fires exactly when
    // the active table crosses the threshold (put 2, 4, ...).
    let mut c = Config::new(common::tmp("met001b"));
    c.memtable_bytes = 512;
    c.l0_compact_trigger = 0;
    c.checkpoint_bytes = 0;
    let db = Db::open(c).unwrap();
    for i in 0..5 {
        db.write(&[Op::Put(format!("k{i}").into_bytes(), v.clone())])
            .unwrap();
    }
    let s = db.stats();
    assert_eq!(s.write.flush_count, 2, "flushes at puts 2 and 4 exactly");
    assert_eq!(s.segments.count, 2, "one segment per flush");
    // The ledger is cumulative — flushes truncate the FILE, not the counter.
    assert!(
        s.write.wal_bytes >= 5 * d0,
        "wal_bytes is cumulative across flushes: {s:?}"
    );
}

/// met002 — the fsync-latency histogram records ONLY real syncs: Sync mode
/// logs every batch, Async mode never syncs and stays empty.
#[test]
fn met002_fsync_latency_histogram_sync_only() {
    let db = Db::open(config("met002")).unwrap();
    db.write(&[Op::Put(b"k".to_vec(), b"v".to_vec())]).unwrap();
    let s = db.stats();
    assert_eq!(s.write.fsync_count, 1);
    let buckets = s.write.fsync_latency_us_buckets.iter().sum::<u64>();
    assert!(buckets >= 1, "the Sync path must record its fsync: {s:?}");

    let mut c = Config::new(common::tmp("met002b"));
    c.memtable_bytes = usize::MAX;
    c.l0_compact_trigger = 0;
    c.checkpoint_bytes = 0;
    c.durability = DurabilityMode::Async;
    let db = Db::open(c).unwrap();
    db.write(&[Op::Put(b"k".to_vec(), b"v".to_vec())]).unwrap();
    let s = db.stats();
    assert_eq!(s.write.fsync_count, 0, "Async never fsyncs");
    assert_eq!(
        s.write.fsync_latency_us_buckets.iter().sum::<u64>(),
        0,
        "no fsync, no latency sample"
    );
}

/// met003 — the backlog gauges are Σ uncompacted L0 bytes/segments while the
/// trigger is unsatisfied, and the admin surface (compact + checkpoint_now)
/// refreshes them.
#[test]
fn met003_backlog_gauges_and_checkpoint_now() {
    let mut c = Config::new(common::tmp("met003"));
    c.memtable_bytes = 512;
    c.l0_compact_trigger = 1000; // unsatisfied by construction
    c.checkpoint_bytes = 0;
    let db = Db::open(c).unwrap();
    let v = vec![0x3c; 300];
    for i in 0..5 {
        db.write(&[Op::Put(format!("k{i}").into_bytes(), v.clone())])
            .unwrap();
    }
    let s = db.stats();
    assert_eq!(s.segments.count, 2, "2 flushes crossed the threshold");
    assert_eq!(
        s.write.compaction_pending_segments, s.segments.count,
        "the trigger is unsatisfied: every L0 segment is pending"
    );
    assert_eq!(
        s.write.compaction_backlog_bytes, s.segments.bytes,
        "backlog = Σ uncompacted L0 bytes"
    );
    assert_eq!(s.write.checkpoint_count, 0, "checkpoint_bytes = 0");

    let compact = db.compact().unwrap();
    assert!(
        compact.segments_in >= 2,
        "merged the pending L0 pile: {compact:?}"
    );
    let s = db.stats();
    assert_eq!(s.write.compaction_pending_segments, 0, "L0 is drained");
    assert_eq!(s.write.compaction_backlog_bytes, 0);
    assert!(
        s.write.last_compaction_ms > 0,
        "compaction stamped the clock"
    );

    let info = db.checkpoint_now().unwrap();
    assert!(info.generation >= 1, "checkpoint published: {info:?}");
    assert_eq!(db.stats().write.checkpoint_count, 1);
}
