//! P4-M5 — read attribution → batch read wave (TDD-READ-001/002/003).
//! Boundary (a): the per-request read trace — sampled (Config.trace_every,
//! 0 = off), value-opaque (no user bytes leave the engine), attribution-
//! complete (wall / lock / memtable / bloom / index / cache / io / decode).
//! The RED list: rd003 trace records per request when enabled, zero records
//! when disabled (the zero-cost pin — one plain load on the disabled path);
//! rd001 N keys in one block → one block read (the read_many grouping pin);
//! rd002 get_many answers == N scalar gets elementwise (SE2-M25 contract).

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::stats::ReadTraceRecord;
use common::dir;

const N: usize = 64;

fn seed(d: &std::path::Path) {
    let db = Db::open(Config::new(d.to_path_buf())).unwrap();
    for i in 0..N {
        db.put(format!("k{i:03}").as_bytes(), format!("v{i:03}").as_bytes())
            .unwrap();
    }
    db.flush().unwrap();
}

// ---------------------------------------------------------------------------
// rd003 — trace: per-request records when enabled, nothing when disabled
// ---------------------------------------------------------------------------

#[test]
fn rd003_trace_records_per_request_enabled_zero_when_disabled() {
    let d = dir("trace-on-off");

    // Disabled (the default): answers flow, the trace accumulates nothing.
    let db = Db::open(Config::new(d.clone())).unwrap();
    db.put(b"a", b"1").unwrap();
    for _ in 0..10 {
        db.get(b"a").unwrap();
    }
    assert_eq!(db.trace_len(), 0, "trace disabled: no records accumulate");

    // Enabled at rate 1: every request is one record, fields populated.
    let d2 = dir("trace-on");
    let mut cfg = Config::new(d2.clone());
    cfg.trace_every = 1;
    let db = Db::open(cfg).unwrap();
    for i in 0..N {
        db.put(format!("k{i:03}").as_bytes(), format!("v{i:03}").as_bytes())
            .unwrap();
    }
    db.flush().unwrap();
    let gets: Vec<Vec<u8>> = (0..N).map(|i| format!("k{i:03}").into_bytes()).collect();
    let keys: Vec<&[u8]> = gets.iter().map(|k| k.as_slice()).collect();
    for _ in 0..2 {
        db.get_many(&keys).unwrap();
    }
    assert_eq!(db.trace_len(), 2, "rate 1 records every read request");

    let recs: Vec<ReadTraceRecord> = db.drain_trace();
    assert_eq!(recs.len(), 2);
    assert!(
        recs.iter().all(|r| r.wall_ns > 0),
        "every record carries wall time"
    );
    assert!(
        recs.iter().all(|r| r.segments_considered >= 1),
        "flushed data resolves through segments"
    );
    assert!(!recs[0].cache_hit, "first wave: cold blocks");
    assert!(
        recs[1].cache_hit,
        "second wave: served from the block cache"
    );
    assert_eq!(recs[0].seq, 0, "request seqs are unique and ordered");
    assert_eq!(recs[1].seq, 1);
}

// ---------------------------------------------------------------------------
// rd001 — same-block batch: N keys cost one block read
// ---------------------------------------------------------------------------

#[test]
fn rd001_same_block_batch_reads_one_block() {
    let d = dir("trace-one-block");
    seed(&d);
    let db = Db::open(Config::new(d.clone())).unwrap();
    let gets: Vec<Vec<u8>> = (0..N).map(|i| format!("k{i:03}").into_bytes()).collect();
    let keys: Vec<&[u8]> = gets.iter().map(|k| k.as_slice()).collect();

    let before = db.read_path_stats();
    let answers = db.get_many(&keys).unwrap();
    let after = db.read_path_stats();

    assert!(
        answers.iter().all(|a| a.is_some()),
        "all seeded keys resolve"
    );
    assert_eq!(
        after.blocks_read - before.blocks_read,
        1,
        "N keys in one block must cost exactly one block read"
    );
}

// ---------------------------------------------------------------------------
// rd002 — batch answers == scalar answers elementwise
// ---------------------------------------------------------------------------

#[test]
fn rd002_get_many_matches_scalar_gets() {
    let d = dir("trace-parity");
    let db = Db::open(Config::new(d.clone())).unwrap();
    for i in 0..N {
        db.put(format!("k{i:03}").as_bytes(), format!("v{i:03}").as_bytes())
            .unwrap();
    }
    db.delete(b"k010").unwrap(); // tombstone: shadows to None
    db.flush().unwrap();
    drop(db);

    let db = Db::open(Config::new(d.clone())).unwrap();
    let probe: Vec<Vec<u8>> = (0..N)
        .step_by(7)
        .map(|i| format!("k{i:03}").into_bytes())
        .chain(std::iter::once(b"k999".to_vec())) // absent
        .collect();
    let keys: Vec<&[u8]> = probe.iter().map(|k| k.as_slice()).collect();

    let batched = db.get_many(&keys).unwrap();
    let scalar: Vec<Option<Vec<u8>>> = keys.iter().map(|k| db.get(k).unwrap()).collect();
    assert_eq!(
        batched, scalar,
        "batch answers must equal scalar gets elementwise"
    );
}
