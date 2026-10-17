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

// ---------------------------------------------------------------------------
// TDD-024 — the review's exact sequence: reserve A → WAL failure →
// reserve B → checkpoint → restart → reserve C. An acknowledged
// reservation never collides; a failed pre-ack reservation may recycle.
// ---------------------------------------------------------------------------

use aikoql_storage_v2::identity::directory::{IdentityResolver, LocalIdentityDirectory};
use aikoql_storage_v2::identity::topology::{LocalReplicaDirectory, ReplicaDirectory};
use aikoql_storage_v2::identity::ReplicaId;
use aikoql_storage_v2::placement::directory::{
    LocalPlacementResolver, Placement, PlacementResolver,
};
use std::path::Path;

fn rid_of(db: &Db, a: ObjectId) -> ReplicaId {
    let lid = LocalIdentityDirectory::new(db).resolve(a).unwrap().unwrap();
    LocalReplicaDirectory::new(db)
        .resolve_local(lid)
        .unwrap()
        .unwrap()
}

fn placement_of(db: &Db, rid: ReplicaId) -> Option<Placement> {
    LocalPlacementResolver::new(db).resolve(rid).unwrap()
}

/// The delta-log count — the prune's own measure (the checkpoint trigger's
/// prune deletes the history; zero proves the checkpoint is the sole
/// directory source).
fn delta_log_count(d: &Path) -> usize {
    ["IDENTITY-", "REPLICA-", "PLACEMENT-"]
        .iter()
        .map(|stem| {
            std::fs::read_dir(d)
                .expect("read dir")
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with(stem))
                .count()
        })
        .sum()
}

/// A checkpoint between the ack and the restart: reserve A burns on the
/// injected WAL failure, B is acknowledged and checkpointed (the trigger's
/// prune deletes the delta history), the restart recovers B from the
/// checkpoint alone, and the fresh C collides with nothing.
#[test]
fn checkpointed_reservations_survive_prune_alone_and_never_collide() {
    let d = dir("r2-003-e-live");
    let (b1, b2) = {
        let mut cfg = Config::new(d.clone());
        cfg.wal_fail_next = true;
        cfg.checkpoint_bytes = 1; // any flush publishes a checkpoint and prunes
        let db = Db::open(cfg).unwrap();
        // reserve A: burned — acknowledged by nobody
        assert!(matches!(db.create_object(), Err(FormatError::Io(_))));
        // reserve B: acknowledged, then checkpointed
        let b1 = db.create_object().unwrap();
        let b2 = db.create_object().unwrap();
        db.put_object(b1, b"k", b"vb1").unwrap();
        db.put_object(b2, b"k", b"vb2").unwrap();
        db.flush().unwrap();
        (b1, b2)
    };
    assert_eq!(
        delta_log_count(&d),
        0,
        "the prune must leave only the checkpoint"
    );
    let db = Db::open(Config::new(d.clone())).unwrap();
    // B survives from the checkpoint alone
    assert_eq!(db.get_object(b1, b"k").unwrap(), Some(b"vb1".to_vec()));
    assert_eq!(db.get_object(b2, b"k").unwrap(), Some(b"vb2".to_vec()));
    // reserve C: fresh ids above the checkpointed floors, colliding with nothing
    let c1 = db.create_object().unwrap();
    let c2 = db.create_object().unwrap();
    db.put_object(c1, b"k", b"vc1").unwrap();
    db.put_object(c2, b"k", b"vc2").unwrap();
    for c in [c1, c2] {
        assert!(![b1, b2].contains(&c), "a checkpointed ObjectId was reused");
    }
    for (o, v) in [(b1, b"vb1"), (b2, b"vb2"), (c1, b"vc1"), (c2, b"vc2")] {
        assert_eq!(db.get_object(o, b"k").unwrap(), Some(v.to_vec()));
    }
}

/// PlacementGeneration rides the same sequence: a failed frame burns a
/// generation nobody observed; the acknowledged sibling's placement rides
/// the checkpoint and survives the prune with its generation intact; the
/// post-restart write publishes ABOVE the checkpointed floor (INV-05).
#[test]
fn placement_generations_never_collide_across_checkpointed_restart() {
    let d = dir("r2-003-f-live");
    let (ok_oid, new_oid, g_ok) = {
        let mut cfg = Config::new(d.clone());
        cfg.wal_fail_next = true;
        cfg.checkpoint_bytes = 1;
        let db = Db::open(cfg).unwrap();
        // reserve A: a triple burned by the injected WAL failure
        let new_oid = ObjectId([0xE1; 16]);
        assert!(matches!(
            db.put_object(new_oid, b"k", b"v"),
            Err(FormatError::Io(_))
        ));
        // reserve B: acknowledged, then checkpointed
        let ok = db.create_object().unwrap();
        db.put_object(ok, b"k", b"vok").unwrap();
        db.flush().unwrap();
        let g_ok = match placement_of(&db, rid_of(&db, ok)).unwrap() {
            Placement::Segment(loc) => loc.generation,
            other => panic!("flushed placement must be a segment, got {other:?}"),
        };
        (ok, new_oid, g_ok)
    };
    assert_eq!(
        delta_log_count(&d),
        0,
        "the prune must leave only the checkpoint"
    );
    let db = Db::open(Config::new(d.clone())).unwrap();
    // B's placement survived the checkpoint-only restart, generation intact
    match placement_of(&db, rid_of(&db, ok_oid)).unwrap() {
        Placement::Segment(loc) => assert_eq!(
            loc.generation, g_ok,
            "the checkpointed placement generation must survive the prune"
        ),
        other => panic!("the checkpointed placement must stay a segment, got {other:?}"),
    }
    // reserve C: the fresh write publishes ABOVE the checkpointed floor
    db.put_object(new_oid, b"k", b"v2").unwrap();
    let g_c = match placement_of(&db, rid_of(&db, new_oid)).unwrap() {
        Placement::Memtable { generation } => generation,
        other => panic!("a pre-flush placement must be Memtable, got {other:?}"),
    };
    assert!(
        g_c > g_ok,
        "a post-restart placement reused a checkpointed generation ({g_c} vs {g_ok})"
    );
    assert_eq!(db.get_object(ok_oid, b"k").unwrap(), Some(b"vok".to_vec()));
    assert_eq!(db.get_object(new_oid, b"k").unwrap(), Some(b"v2".to_vec()));
}

/// SegmentId ("where applicable" in the review): not WAL-reserved at all —
/// derived from the manifest generation at open, so a restart can never
/// re-hand a published segment id.
#[test]
fn segment_ids_are_manifest_derived_and_never_reused() {
    let d = dir("r2-003-g-live");
    let seg_before = {
        let db = Db::open(Config::new(d.clone())).unwrap();
        let a = db.create_object().unwrap();
        db.put_object(a, b"k", b"v").unwrap();
        db.flush().unwrap();
        match placement_of(&db, rid_of(&db, a)).unwrap() {
            Placement::Segment(loc) => loc.segment_id,
            other => panic!("flushed placement must be a segment, got {other:?}"),
        }
    };
    let db = Db::open(Config::new(d.clone())).unwrap();
    let b = db.create_object().unwrap();
    db.put_object(b, b"k", b"v").unwrap();
    db.flush().unwrap();
    let seg_after = match placement_of(&db, rid_of(&db, b)).unwrap() {
        Placement::Segment(loc) => loc.segment_id,
        other => panic!("flushed placement must be a segment, got {other:?}"),
    };
    assert!(
        seg_after != seg_before,
        "a segment id was reused across restart ({seg_before:?})"
    );
}
