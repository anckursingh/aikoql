//! M17 harness finding — txn-record prefix collision repro. A get for
//! `sys/txn/bench-17` must NEVER answer the record of `sys/txn/bench-1`
//! (whose key is a byte prefix). Memory-engine tests can't catch this —
//! the record lives in a flushed segment here.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use common::dir;

#[test]
fn get_is_exact_after_flush_prefix_collision() {
    let db = Db::open(Config::new(dir("prefix-collision"))).unwrap();
    for tid in ["bench-1", "bench-2", "bench-10", "bench-16"] {
        db.put(format!("sys/txn/{tid}").as_bytes(), b"record".as_slice())
            .unwrap();
    }
    db.flush().unwrap();

    // A key that exists only as a PREFIX of stored keys must miss, in the
    // memtable generation AND after flush.
    assert_eq!(
        db.get(b"sys/txn/bench-1").unwrap().as_deref(),
        Some(b"record".as_slice())
    );
    assert_eq!(db.get(b"sys/txn/bench-17").unwrap(), None);
    assert_eq!(db.get(b"sys/txn/bench").unwrap(), None);
    assert_eq!(db.get(b"sys/txn/bench-1x").unwrap(), None);
}

/// Ingest-shaped history (many journal-event + object keys, flushed), then
/// the txn records, flushed again — the M17 harness's exact shape. Distinct
/// record values so a wrong hit names its source key.
#[test]
fn get_is_exact_after_ingest_history() {
    let db = Db::open(Config::new(dir("prefix-ingest"))).unwrap();
    for i in 0..3000u32 {
        db.put(format!("ke/{i:08}").as_bytes(), b"event".as_slice())
            .unwrap();
        db.put(format!("ko/{i:08}").as_bytes(), b"object".as_slice())
            .unwrap();
    }
    db.flush().unwrap();
    for n in 1..=16u32 {
        db.put(
            format!("sys/txn/bench-{n}").as_bytes(),
            format!("record-{n}").as_bytes(),
        )
        .unwrap();
    }
    db.flush().unwrap();

    assert_eq!(
        db.get(b"sys/txn/bench-16").unwrap().as_deref(),
        Some(b"record-16".as_slice())
    );
    assert_eq!(db.get(b"sys/txn/bench-17").unwrap(), None);
    assert_eq!(db.get(b"sys/txn/bench").unwrap(), None);
}
