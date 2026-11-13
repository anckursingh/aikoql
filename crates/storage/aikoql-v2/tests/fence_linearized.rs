//! PROD-3 review (#39 item 2) — the write fence is linearized with commits
//! already in flight.
//!
//! A leader stopping for a planned exit fences, reads `last_seq()`, and
//! releases its lease stamped with that seq. A commit that had passed the
//! fenced check but not yet taken its seq would be acknowledged ABOVE the
//! value read — the lease would be released below an acknowledged write.
//!
//! fl001/fl002 park a writer right after it passed the check (the
//! `after_admit` stage), then fence on another thread: `fence()` must not
//! return while that commit is in flight, and the seq read after it must
//! cover the acknowledged write. One binary: the park env is process-wide,
//! so the legs serialize on PARK_LOCK.

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use common::dir;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ADMIT_ENV: &str = "AIKOQL_V2_ADMIT_PARK";

static PARK_LOCK: Mutex<()> = Mutex::new(());

fn wait_for(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "park never reached"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Disarms the park on every exit, a failed assertion included — a leftover
/// env would park the next leg's own seed writes.
struct Disarm(std::path::PathBuf);

impl Drop for Disarm {
    fn drop(&mut self) {
        std::env::remove_var(ADMIT_ENV);
        let _ = std::fs::remove_file(self.0.join("after_admit"));
    }
}

fn leg(tag: &str, durability: DurabilityMode) {
    let _serial = PARK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = dir(tag);
    let mut cfg = Config::new(d.clone());
    cfg.durability = durability;
    let db = Arc::new(Db::open(cfg).unwrap());
    for i in 0..5u32 {
        db.put(format!("s{i}").as_bytes(), b"seed").unwrap();
    }

    std::env::set_var(ADMIT_ENV, "after_admit");
    let _disarm = Disarm(d.clone());
    let writer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.put(b"in-flight", b"x"))
    };
    wait_for(&d.join("after_admit")); // passed the check, no seq yet

    let seen = Arc::new(AtomicU64::new(0));
    let fenced = {
        let (db, seen) = (Arc::clone(&db), Arc::clone(&seen));
        std::thread::spawn(move || {
            db.fence("stopping".into());
            seen.store(db.last_seq(), Ordering::SeqCst);
        })
    };
    // fence() has to wait out the in-flight commit — it cannot be done yet.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !fenced.is_finished(),
        "fence() returned while a commit that passed the check was still in flight"
    );

    std::fs::remove_file(d.join("after_admit")).unwrap(); // release the park
    let acked = writer.join().unwrap().expect("the in-flight write acks");
    fenced.join().unwrap();
    assert!(
        seen.load(Ordering::SeqCst) >= acked,
        "last_seq read after fence() ({}) is below an acknowledged write ({acked})",
        seen.load(Ordering::SeqCst)
    );
    assert!(
        db.put(b"after", b"y").is_err(),
        "a write after fence() must be refused"
    );
}

#[test]
fn fl001_sync_fence_waits_for_the_in_flight_commit() {
    leg("fl001-sync", DurabilityMode::Sync);
}

#[test]
fn fl002_group_commit_fence_waits_for_the_in_flight_commit() {
    leg("fl002-group", DurabilityMode::GroupCommit);
}
