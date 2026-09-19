//! PR6-R2-003 — the id/generation reuse contract, stated exactly and
//! pinned. An ACKNOWLEDGED reservation rode its acked frame to the WAL and
//! is never reused after a restart; an UNACKNOWLEDGED reservation (the WAL
//! append failed and Err was returned) was observed by nobody and MAY be
//! recycled after a restart — the pins below assert no collision with any
//! acknowledged state either way. The failure is injected with
//! `Config::wal_fail_next` (one-shot, per-db — process-local, so parallel
//! tests can never eat it).

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::format::FormatError;
use aikoql_storage_v2::identity::ObjectId;
use common::dir;

/// A failed create left nothing durable: after restart the engine may hand
/// the failed reservation out again (or advance past it) — either way it
/// must never collide with acknowledged state, and every acknowledged
/// write must survive the restart.
#[test]
fn failed_create_reservation_recycles_without_collision() {
    let d = dir("r2-003-a-live");
    let acked: Vec<ObjectId> = {
        let mut cfg = Config::new(d.clone());
        cfg.wal_fail_next = true;
        let db = Db::open(cfg).unwrap();
        assert!(
            matches!(db.create_object(), Err(FormatError::Io(_))),
            "the injected WAL failure must surface on the next frame"
        );
        // The hook is consumed — subsequent creates are acknowledged.
        let mut v = Vec::new();
        for i in 0..3u8 {
            let oid = db.create_object().unwrap();
            db.put_object(oid, &[i], b"acked").unwrap();
            v.push(oid);
        }
        v
    };

    let db = Db::open(Config::new(d.clone())).unwrap();
    for (i, oid) in acked.iter().enumerate() {
        assert_eq!(
            db.get_object(*oid, &[i as u8]).unwrap(),
            Some(b"acked".to_vec()),
            "acknowledged create+put lost across restart"
        );
    }
    let fresh: Vec<ObjectId> = (0..3).map(|_| db.create_object().unwrap()).collect();
    for oid in &fresh {
        db.put_object(*oid, b"k", b"v").unwrap();
        assert_eq!(
            db.get_object(*oid, b"k").unwrap(),
            Some(b"v".to_vec()),
            "a post-restart create must be a fully working object"
        );
        assert!(!acked.contains(oid), "an acknowledged ObjectId was reused");
    }
}

/// The unknown-ObjectId arm reserves a triple and rides Create+Put in ONE
/// frame. A failed frame means the ObjectId was never acknowledged: the
/// same oid may be put again (a fresh create), and an acknowledged
/// sibling is untouched by any of it.
#[test]
fn failed_put_object_reservation_recycles_without_collision() {
    let d = dir("r2-003-b-live");
    let oid = ObjectId([7; 16]);
    let ok = {
        let db = Db::open(Config::new(d.clone())).unwrap();
        let ok = db.create_object().unwrap();
        db.put_object(ok, b"ok", b"v").unwrap();
        ok
    };
    {
        // The first Db is dropped at the block end above — re-open with the hook.
        let mut cfg = Config::new(d.clone());
        cfg.wal_fail_next = true;
        let db = Db::open(cfg).unwrap();
        assert!(
            matches!(db.put_object(oid, b"k", b"v1"), Err(FormatError::Io(_))),
            "the injected WAL failure must surface on the create+put frame"
        );
        // Never acknowledged — a retry is a fresh create, not a collision.
        db.put_object(oid, b"k", b"v2").unwrap();
        assert_eq!(db.get_object(oid, b"k").unwrap(), Some(b"v2".to_vec()));
        assert_eq!(
            db.get_object(ok, b"ok").unwrap(),
            Some(b"v".to_vec()),
            "the acknowledged sibling must be untouched"
        );
    }
    let db = Db::open(Config::new(d.clone())).unwrap();
    assert_eq!(
        db.get_object(oid, b"k").unwrap(),
        Some(b"v2".to_vec()),
        "the acknowledged first-put must survive restart"
    );
    assert_eq!(db.get_object(ok, b"ok").unwrap(), Some(b"v".to_vec()));
}

/// The placement generation rides the same reservation: a failed frame
/// burns one in memory only. After restart, durable placements (the
/// acknowledged ones, checkpointed) must win and new ones must not
/// collide with them.
#[test]
fn failed_placement_generation_recycles_without_collision() {
    let d = dir("r2-003-c-live");
    let mut cfg = Config::new(d.clone());
    cfg.wal_fail_next = true;
    let (a, b) = {
        let db = Db::open(cfg).unwrap();
        assert!(matches!(db.create_object(), Err(FormatError::Io(_))));
        let a = db.create_object().unwrap();
        let b = db.create_object().unwrap();
        db.put_object(a, b"x", b"va").unwrap();
        db.put_object(b, b"x", b"vb").unwrap();
        db.checkpoint_now().unwrap(); // the generations become durable
        (a, b)
    };
    let db = Db::open(Config::new(d.clone())).unwrap();
    db.checkpoint_now().unwrap(); // full placement round-trip after restart
    let c = db.create_object().unwrap();
    db.put_object(c, b"x", b"vc").unwrap();
    assert_eq!(db.get_object(a, b"x").unwrap(), Some(b"va".to_vec()));
    assert_eq!(db.get_object(b, b"x").unwrap(), Some(b"vb".to_vec()));
    assert_eq!(
        db.get_object(c, b"x").unwrap(),
        Some(b"vc".to_vec()),
        "the post-restart placement must be a working, non-colliding generation"
    );
}

/// The positive half of the contract: every acknowledged reservation
/// survives restart and is never handed out again.
#[test]
fn acknowledged_ids_never_reused_after_restart() {
    let d = dir("r2-003-d-live");
    let acked: Vec<ObjectId> = {
        let db = Db::open(Config::new(d.clone())).unwrap();
        (0..5u8)
            .map(|i| {
                let oid = db.create_object().unwrap();
                db.put_object(oid, &[i], b"v").unwrap();
                oid
            })
            .collect()
    };
    let db = Db::open(Config::new(d.clone())).unwrap();
    for (i, oid) in acked.iter().enumerate() {
        assert_eq!(
            db.get_object(*oid, &[i as u8]).unwrap(),
            Some(b"v".to_vec()),
            "acknowledged write lost across restart"
        );
    }
    let fresh: Vec<ObjectId> = (0..5).map(|_| db.create_object().unwrap()).collect();
    for oid in &fresh {
        assert!(
            !acked.contains(oid),
            "an acknowledged ObjectId was reused after restart"
        );
    }
}
