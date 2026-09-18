//! PR6-002 — post-checkpoint delta coverage (review P0 Recovery): a valid
//! checkpoint plus an incomplete delta set is an invalid state, so the open
//! must fail closed when a required authoritative delta generation is
//! missing. "Required" is family-precise — a generation with no work for a
//! family publishes no log, so gaps are normal — and the review's exact
//! shape is pinned: an INTERMEDIATE placement log deleted while a newer one
//! survives (checkpoint = 2, CURRENT = 4, PLACEMENT-3 gone, PLACEMENT-4
//! present). Its records exist nowhere else; a silent open builds the
//! invalid state.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::identity::ObjectId;
use common::dir;
use std::path::{Path, PathBuf};

fn oid(i: u8) -> ObjectId {
    ObjectId([i; 16])
}

fn reopen_cfg(d: &Path) -> Config {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.checkpoint_bytes = 0; // no trigger: the admin checkpoint sets the baseline
    cfg.l0_compact_trigger = 0;
    cfg
}

/// Phase 1 — five objects, flushed at generation 2, then an admin
/// checkpoint at 2 (which prunes the generation-2 logs: they are the
/// subsumed baseline). Phase 2 — five NEW objects (identity + replica +
/// placement work), flushed at generation 3: every family publishes a
/// post-checkpoint log.
fn build_checkpointed_db(name: &str) -> PathBuf {
    let d = dir(name);
    {
        let db = Db::open(reopen_cfg(&d)).unwrap();
        for i in 0x01u8..=0x05 {
            db.put_object(oid(i), b"k", &[i]).unwrap();
        }
        db.flush().unwrap(); // generation 2
        db.checkpoint_now().unwrap(); // CHECKPOINT-2, prunes the ≤ 2 logs
        for i in 0x06u8..=0x0A {
            db.put_object(oid(i), b"k", &[i]).unwrap();
        }
        db.flush().unwrap(); // generation 3: all three families
    }
    d
}

/// Phase 3 — a flush with NO identity/replica work (puts to existing
/// objects): generation 4 publishes PLACEMENT-4 only, so the placement log
/// at generation 3 is INTERMEDIATE, not the newest — the review's
/// `checkpoint = 100, CURRENT = 120, PLACEMENT-113.log missing` scenario.
fn add_placement_only_phase(d: &Path) {
    let db = Db::open(reopen_cfg(d)).unwrap();
    for i in 0x01u8..=0x05 {
        db.put_object(oid(i), b"k2", &[i, i]).unwrap();
    }
    db.flush().unwrap(); // generation 4: PLACEMENT-4 only
}

/// Reopen and require the coverage error for `family` at generation 3.
fn expect_open_fails(d: &Path, family: &str) {
    match Db::open(reopen_cfg(d)) {
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains(family) && msg.contains("missing"),
                "coverage error names the family and the miss: {msg}"
            );
        }
        Ok(_) => panic!("an incomplete post-checkpoint delta set must fail closed"),
    }
}

#[test]
fn missing_post_checkpoint_delta_fails_closed() {
    let d = build_checkpointed_db("pr6-002-missing");
    add_placement_only_phase(&d);
    // The review's scenario, verbatim in shape: the intermediate placement
    // log is deleted while the newer one survives.
    std::fs::remove_file(d.join("PLACEMENT-000003.log")).unwrap();
    expect_open_fails(&d, "PLACEMENT");
}

#[test]
fn complete_post_checkpoint_delta_range_recovers() {
    let d = build_checkpointed_db("pr6-002-complete");
    add_placement_only_phase(&d);
    let db = Db::open(reopen_cfg(&d)).unwrap();
    for i in 0x01u8..=0x0A {
        assert_eq!(
            db.get_object(oid(i), b"k").unwrap(),
            Some(vec![i]),
            "phase-1/2 keys survive the checkpoint + delta replay"
        );
    }
    for i in 0x01u8..=0x05 {
        assert_eq!(
            db.get_object(oid(i), b"k2").unwrap(),
            Some(vec![i, i]),
            "phase-3 keys survive"
        );
    }
}

#[test]
fn missing_identity_delta_fails_closed() {
    let d = build_checkpointed_db("pr6-002-missing-identity");
    std::fs::remove_file(d.join("IDENTITY-000003.log")).unwrap();
    expect_open_fails(&d, "IDENTITY");
}

#[test]
fn missing_replica_delta_fails_closed() {
    let d = build_checkpointed_db("pr6-002-missing-replica");
    std::fs::remove_file(d.join("REPLICA-000003.log")).unwrap();
    expect_open_fails(&d, "REPLICA");
}

#[test]
fn missing_placement_delta_fails_closed() {
    // The independent placement arm: the newest placement log missing while
    // the identity/replica coverage is intact.
    let d = build_checkpointed_db("pr6-002-missing-placement");
    std::fs::remove_file(d.join("PLACEMENT-000003.log")).unwrap();
    expect_open_fails(&d, "PLACEMENT");
}
