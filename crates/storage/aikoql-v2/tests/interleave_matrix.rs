//! L-04 (TDD-006) — byte/object interleave matrix: one key carries
//! interleaved byte and object rows, and the review's 5-row sequence must
//! answer byte→v3 (the newest BYTE row — object rows never shadow it),
//! rid7→o7b (its own newest row), rid8→o8, and a never-written oid→None —
//! identically through memtable → flush → compact → checkpoint → reopen.
//! A byte-only key must never answer an object read at any stage.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::identity::ObjectId;
use common::dir;

/// The 5-row write block (create_object consumes two seqs first; the
/// answer rows sit at block positions 3/4/5 — byte v3, rid8 o8, rid7 o7b).
fn write_matrix(db: &Db) -> (ObjectId, ObjectId, ObjectId) {
    let oid7 = db.create_object().unwrap();
    let oid8 = db.create_object().unwrap();
    let oid9 = db.create_object().unwrap(); // never written
    db.put(b"k", b"v0").unwrap();
    db.put_object(oid7, b"k", b"o7a").unwrap();
    db.put(b"k", b"v3").unwrap(); // the newest byte row
    db.put_object(oid8, b"k", b"o8").unwrap();
    db.put_object(oid7, b"k", b"o7b").unwrap(); // the newest rid7 row
    db.put(b"b", b"byte-only").unwrap();
    (oid7, oid8, oid9)
}

fn assert_answers(db: &Db, oid7: ObjectId, oid8: ObjectId, oid9: ObjectId, stage: &str) {
    assert_eq!(
        db.get(b"k").unwrap(),
        Some(b"v3".to_vec()),
        "{stage}: byte read = the newest byte row"
    );
    assert_eq!(
        db.get_object(oid7, b"k").unwrap(),
        Some(b"o7b".to_vec()),
        "{stage}: rid7 read = its own newest row"
    );
    assert_eq!(
        db.get_object(oid8, b"k").unwrap(),
        Some(b"o8".to_vec()),
        "{stage}: rid8 read = its own row"
    );
    assert_eq!(
        db.get_object(oid9, b"k").unwrap(),
        None,
        "{stage}: a never-written oid answers None"
    );
    assert_eq!(
        db.get_object(oid7, b"b").unwrap(),
        None,
        "{stage}: a byte-only key never answers an object read"
    );
}

#[test]
fn interleave_matrix_answers_through_every_lifecycle_stage() {
    let d = dir("interleave-matrix");
    let db = Db::open(Config::new(d.clone())).unwrap();
    let (oid7, oid8, oid9) = write_matrix(&db);
    assert_answers(&db, oid7, oid8, oid9, "memtable");

    db.rotate();
    db.flush().unwrap();
    assert_answers(&db, oid7, oid8, oid9, "flush");

    db.compact().unwrap();
    assert_answers(&db, oid7, oid8, oid9, "compact");

    db.checkpoint_now().unwrap();
    assert_answers(&db, oid7, oid8, oid9, "checkpoint");

    drop(db);
    let db = Db::open(Config::new(d.clone())).unwrap();
    assert_answers(&db, oid7, oid8, oid9, "reopen");
}
