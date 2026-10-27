//! MRFC-0080 R1b — follower bootstrap: a leader whose early history is
//! gone (truncated before `retain_log`, or released) seeds a follower
//! with `snapshot_to` + `restore_from`, and the follower then catches up
//! through `log_since` from its restored `last_seq`.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::snapshot::restore_from;
use common::dir;

fn config(d: std::path::PathBuf, retain: bool) -> Config {
    let mut c = Config::new(d);
    c.retain_log = retain;
    c.memtable_bytes = 512; // flush every few writes
    c.compact_background = false;
    c
}

fn write_round(db: &Db, round: u32, oids: &mut Vec<aikoql_storage_v2::identity::ObjectId>) {
    for i in 0..30u32 {
        db.put(format!("r{round}-k{i:02}").as_bytes(), &[i as u8; 40])
            .unwrap();
        if i % 6 == 0 {
            let oid = db.create_object().unwrap();
            db.put_object(oid, b"body", &[round as u8; 30]).unwrap();
            oids.push(oid);
        }
    }
    db.delete(format!("r{round}-k00").as_bytes()).unwrap();
}

#[test]
fn snapshot_bootstrap_then_log_catch_up() {
    let ldir = dir("boot-lead");
    let mut oids = Vec::new();
    {
        // history written before retention existed: unshippable by log
        let old = Db::open(config(ldir.clone(), false)).unwrap();
        write_round(&old, 0, &mut oids);
    }
    let leader = Db::open(config(ldir, true)).unwrap();
    assert!(
        leader.log_since(0, usize::MAX).is_err(),
        "a fresh follower cannot start from seq 0 — bootstrap is required"
    );

    let snap = dir("boot-snap");
    std::fs::remove_dir_all(&snap).ok();
    leader.snapshot_to(&snap).unwrap();
    write_round(&leader, 1, &mut oids); // written after the snapshot

    let fdir = dir("boot-follow");
    std::fs::remove_dir_all(&fdir).ok();
    let follower = restore_from(&snap, fdir).unwrap();
    assert!(follower.last_seq() > 0 && follower.last_seq() < leader.last_seq());

    for f in leader.log_since(follower.last_seq(), usize::MAX).unwrap() {
        follower.apply_replicated(f.seq, &f.ops).unwrap();
    }

    assert_eq!(follower.last_seq(), leader.last_seq());
    assert_eq!(follower.scan(b"").unwrap(), leader.scan(b"").unwrap());
    for oid in &oids {
        assert_eq!(
            follower.get_object(*oid, b"body").unwrap(),
            leader.get_object(*oid, b"body").unwrap(),
        );
    }
    let fresh = follower.create_object().unwrap();
    assert!(
        !oids.contains(&fresh),
        "promoted follower reused an object id"
    );
}
