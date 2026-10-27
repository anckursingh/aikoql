//! MRFC-0080 R1 — the storage replication log: a leader with
//! `retain_log` keeps every frame a flush would truncate, `log_since`
//! serves them contiguously, and a follower's `apply_replicated` rebuilds
//! the same state under the leader's sequence numbers.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::identity::ObjectId;
use common::dir;

fn leader_config(d: std::path::PathBuf) -> Config {
    let mut c = Config::new(d);
    c.retain_log = true;
    c.memtable_bytes = 512; // a flush every few writes — the WAL truncates
    c.compact_background = false;
    c
}

fn ship(leader: &Db, follower: &Db) {
    let frames = leader.log_since(follower.last_seq(), usize::MAX).unwrap();
    for f in frames {
        follower.apply_replicated(f.seq, &f.ops).unwrap();
    }
}

/// Writes byte keys and object rows, flushing many times along the way.
fn seed(leader: &Db) -> Vec<ObjectId> {
    let mut oids = Vec::new();
    for i in 0..60u32 {
        leader
            .put(format!("k{i:03}").as_bytes(), &[i as u8; 40])
            .unwrap();
        if i % 7 == 0 {
            leader.delete(format!("k{:03}", i / 2).as_bytes()).unwrap();
        }
        if i % 5 == 0 {
            let oid = leader.create_object().unwrap();
            leader.put_object(oid, b"body", &[i as u8; 30]).unwrap();
            oids.push(oid);
        }
    }
    oids
}

#[test]
fn follower_matches_leader_across_flush_truncations() {
    let leader = Db::open(leader_config(dir("repl-lead"))).unwrap();
    let follower = Db::open(Config::new(dir("repl-follow"))).unwrap();
    let oids = seed(&leader);
    assert!(leader.stats().segments.count > 1, "the seed must cross flushes");

    ship(&leader, &follower);

    assert_eq!(follower.last_seq(), leader.last_seq());
    assert_eq!(follower.scan(b"").unwrap(), leader.scan(b"").unwrap());
    for oid in &oids {
        assert_eq!(
            follower.get_object(*oid, b"body").unwrap(),
            leader.get_object(*oid, b"body").unwrap()
        );
    }
    // promotion: the follower allocates past every copied id
    let fresh = follower.create_object().unwrap();
    assert!(!oids.contains(&fresh), "promoted follower reused an object id");
}

#[test]
fn redelivery_is_a_no_op_and_a_gap_fails_closed() {
    let leader = Db::open(leader_config(dir("repl-redeliver-lead"))).unwrap();
    let follower = Db::open(Config::new(dir("repl-redeliver-follow"))).unwrap();
    leader.put(b"a", b"1").unwrap();
    leader.put(b"b", b"2").unwrap();
    leader.put(b"c", b"3").unwrap();
    let frames = leader.log_since(0, usize::MAX).unwrap();
    assert_eq!(frames.len(), 3);

    follower.apply_replicated(frames[0].seq, &frames[0].ops).unwrap();
    follower.apply_replicated(frames[0].seq, &frames[0].ops).unwrap(); // re-delivery
    assert!(
        follower.apply_replicated(frames[2].seq, &frames[2].ops).is_err(),
        "skipping a frame must fail, not leave a hole"
    );
    follower.apply_replicated(frames[1].seq, &frames[1].ops).unwrap();
    follower.apply_replicated(frames[2].seq, &frames[2].ops).unwrap();
    assert_eq!(follower.scan(b"").unwrap(), leader.scan(b"").unwrap());

    // max bounds a catch-up batch
    assert_eq!(leader.log_since(0, 2).unwrap().len(), 2);
}

#[test]
fn log_without_retention_reports_a_gap() {
    let mut c = Config::new(dir("repl-noretain"));
    c.memtable_bytes = 512;
    let leader = Db::open(c).unwrap();
    for i in 0..40u32 {
        leader.put(format!("k{i}").as_bytes(), &[0u8; 40]).unwrap();
    }
    assert!(leader.stats().segments.count > 0);
    assert!(
        leader.log_since(0, usize::MAX).is_err(),
        "truncated frames must surface as a gap — the follower re-bootstraps"
    );
}

#[test]
fn release_log_prunes_and_follower_resumes_after_restart() {
    let ldir = dir("repl-release-lead");
    let leader = Db::open(leader_config(ldir.clone())).unwrap();
    let fdir = dir("repl-release-follow");
    let follower = Db::open(Config::new(fdir.clone())).unwrap();
    seed(&leader);
    ship(&leader, &follower);
    let acked = follower.last_seq();
    drop(follower);

    let archive = || std::fs::read_dir(ldir.join("rlog")).unwrap().count();
    let before = archive();
    leader.release_log(acked).unwrap();
    assert!(archive() < before, "acked archive files stay");

    seed(&leader); // more history after the release
    let follower = Db::open(Config::new(fdir)).unwrap();
    assert_eq!(follower.last_seq(), acked, "restart resumes at the acked seq");
    ship(&leader, &follower);
    assert_eq!(follower.scan(b"").unwrap(), leader.scan(b"").unwrap());
    assert!(
        leader.log_since(0, usize::MAX).is_err(),
        "released frames are gone — asking for them is a gap"
    );
}
