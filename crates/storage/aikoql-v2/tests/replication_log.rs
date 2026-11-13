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
    assert!(
        leader.stats().segments.count > 1,
        "the seed must cross flushes"
    );

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
    assert!(
        !oids.contains(&fresh),
        "promoted follower reused an object id"
    );
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

    follower
        .apply_replicated(frames[0].seq, &frames[0].ops)
        .unwrap();
    follower
        .apply_replicated(frames[0].seq, &frames[0].ops)
        .unwrap(); // re-delivery
    assert!(
        follower
            .apply_replicated(frames[2].seq, &frames[2].ops)
            .is_err(),
        "skipping a frame must fail, not leave a hole"
    );
    follower
        .apply_replicated(frames[1].seq, &frames[1].ops)
        .unwrap();
    follower
        .apply_replicated(frames[2].seq, &frames[2].ops)
        .unwrap();
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
    assert_eq!(
        follower.last_seq(),
        acked,
        "restart resumes at the acked seq"
    );
    ship(&leader, &follower);
    assert_eq!(follower.scan(b"").unwrap(), leader.scan(b"").unwrap());
    assert!(
        leader.log_since(0, usize::MAX).is_err(),
        "released frames are gone — asking for them is a gap"
    );
}

/// PROD-6 (failure matrix row 12 — the leader's disk refuses a WAL
/// append): the write fails, is not acknowledged, and takes no seq. A
/// consumed seq with no frame behind it is a hole in the log: `last_seq`
/// would name a write that does not exist (the HA lease records it, and a
/// standby that cannot reach it never takes over), and the next frame
/// would skip a seq no follower can ever apply — one transient I/O error
/// on the leader would cost every standby a re-bootstrap.
#[test]
fn a_failed_wal_append_takes_no_seq_and_leaves_no_hole_in_the_log() {
    let (ld, fd) = (dir("repl-walfail-leader"), dir("repl-walfail-follower"));
    let mut cfg = leader_config(ld);
    cfg.wal_fail_next = true;
    let leader = Db::open(cfg).unwrap();
    let follower = Db::open(Config::new(fd)).unwrap();
    let before = leader.last_seq();

    assert!(
        leader.put(b"lost", b"never acked").is_err(),
        "the injected WAL failure must surface"
    );
    assert_eq!(
        leader.last_seq(),
        before,
        "a write that was never durable must not take a seq"
    );
    assert_eq!(leader.get(b"lost").unwrap(), None, "nor be readable");

    let seq = leader.put(b"kept", b"acked").unwrap();
    assert_eq!(seq, before + 1, "the next write takes the next seq");
    let frames = leader.log_since(before, 16).unwrap();
    assert_eq!(
        frames.iter().map(|f| f.seq).collect::<Vec<_>>(),
        vec![before + 1],
        "the log is contiguous past the failed write"
    );
    ship(&leader, &follower);
    assert_eq!(follower.last_seq(), leader.last_seq());
    assert_eq!(follower.get(b"kept").unwrap(), Some(b"acked".to_vec()));
    assert_eq!(follower.get(b"lost").unwrap(), None);
}

/// PROD-6 review (#42): a failed fsync is not a failed append. After an
/// fsync error the kernel may have dropped the dirty pages, so what the log
/// holds past that point cannot be trusted — the engine stops taking client
/// writes (and says why) instead of cutting the tail and carrying on. The
/// failed write took no seq, and nothing acknowledged is touched.
#[test]
fn a_failed_wal_sync_fences_the_engine_with_the_reason() {
    let mut cfg = leader_config(dir("repl-syncfail-leader"));
    cfg.wal_sync_fail_next = true;
    let leader = Db::open(cfg).unwrap();
    let before = leader.last_seq();

    assert!(leader.put(b"lost", b"never acked").is_err());
    assert_eq!(leader.last_seq(), before, "the failed write takes no seq");
    let why = leader.fence_reason().expect("an fsync error fences");
    assert!(why.contains("sync"), "the reason names the fsync: {why}");
    let refused = leader.put(b"after", b"x").unwrap_err().to_string();
    assert!(
        refused.contains("not leader"),
        "later writes are refused: {refused}"
    );
    assert_eq!(leader.get(b"lost").unwrap(), None);
}

/// PROD-6 (failure matrix row 13 — the standby's disk refuses a WAL
/// append): the replicated frame is not applied and the standby's
/// `last_seq` — what it acks to the leader — does not move, so a
/// synchronous leader is never told a write is held that is not. The
/// same frame, delivered again once the disk recovers, applies.
#[test]
fn a_standby_that_cannot_append_does_not_advance_what_it_acks() {
    let (ld, fd) = (dir("repl-sbfail-leader"), dir("repl-sbfail-follower"));
    let leader = Db::open(leader_config(ld)).unwrap();
    let mut cfg = Config::new(fd);
    cfg.wal_fail_next = true;
    let follower = Db::open(cfg).unwrap();
    leader.put(b"k", b"v").unwrap();
    let frames = leader.log_since(follower.last_seq(), 16).unwrap();
    let first = &frames[0];
    let before = follower.last_seq();

    assert!(
        follower.apply_replicated(first.seq, &first.ops).is_err(),
        "the injected WAL failure must surface"
    );
    assert_eq!(follower.last_seq(), before, "nothing to ack");
    assert_eq!(follower.get(b"k").unwrap(), None);

    ship(&leader, &follower); // the leader re-sends from the standby's seq
    assert_eq!(follower.last_seq(), leader.last_seq());
    assert_eq!(follower.get(b"k").unwrap(), Some(b"v".to_vec()));
}
