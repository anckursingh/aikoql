//! P5-M39 (R4-P0-02) — compaction lock-scope split REDs. `compact_impl`
//! runs the whole merge — segment construction, disk I/O, checksumming,
//! reader reopen, publication — under the state WRITE lock: every
//! concurrent reader or writer waits for the complete merge. The split —
//! A short lock (capture the input segment set + publication generation,
//! stage a namespace) → B NO lock (encode/write/validate/reopen into a
//! staging directory) → C short generation-checked publish (rename into
//! the real namespace with fresh ids; a generation changed since A makes
//! the merge stale → discard the staging output, publish nothing) — with
//! the counters compact_total_ns / compact_state_lock_hold_ns /
//! compact_io_ns / compact_publish_ns and the structural invariant
//! hold << total (the state lock must never cover merge construction).
//!
//! csc001 — a put completes while the merge is parked in its segment-I/O
//!   phase (AIKOQL_V2_COMPACT_PARK=in_io — today that window does not
//!   exist: the merge runs under the state lock, so a put blocks for the
//!   whole compaction. RED: the marker never appears);
//! csc002 — the four lock-scope counters exist, are populated by a real
//!   merge, and hold (A+C) + io (B) ≤ total — the disjoint windows must
//!   not overlap (compile-error RED today: the fields do not exist);
//! csc003 — stale-publication pin: a flush COMPLETES while the merge is
//!   parked, and the compaction's publication must not overwrite it —
//!   the merge is discarded (stale), CURRENT stays at the flush's
//!   generation, the input segment files survive (today the flush
//!   blocks on the state lock the merge holds — the scenario cannot
//!   run. RED: the marker never appears).
//!
//! One binary: the park env is process-wide; every park-arming test
//! serializes on PARK_LOCK and the release always runs (the guard), so a
//! blocked op today cannot hang the suite — the RED is the flag, not a
//! hang. (The park helpers mirror tests/flush_lock_scope.rs — two copies
//! is the tolerated class; three would tip into a shared harness module.)

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use aikoql_storage_v2::format::Current;
use common::dir;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PARK_ENV: &str = "AIKOQL_V2_COMPACT_PARK";
const PARK_STAGE: &str = "in_io";

/// A db whose writes never auto-flush (the memtable sits far above the
/// test workload) and never auto-compact: every flush/compact here is
/// the test's own explicit call. Async durability keeps the seeds fast.
fn open_quiet(d: &Path) -> Db {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.memtable_bytes = 64 << 20;
    cfg.l0_compact_trigger = 0;
    cfg.durability = DurabilityMode::Async;
    Db::open(cfg).unwrap()
}

fn walk(db: &Db) -> BTreeMap<Vec<u8>, Vec<u8>> {
    db.scan(b"").unwrap().into_iter().collect()
}

fn put_range(db: &Db, lo: u64, hi: u64) {
    for i in lo..hi {
        db.put(format!("k{i:05}").as_bytes(), format!("v{i}").as_bytes())
            .unwrap();
    }
}

fn segment_file_count(d: &Path) -> usize {
    std::fs::read_dir(d)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("SEGMENT-")
        })
        .count()
}

fn wait_for_park(d: &Path) {
    let marker = d.join(PARK_STAGE);
    let start = Instant::now();
    while !marker.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the compaction must have an unlocked merge phase \
             ({PARK_ENV}={PARK_STAGE}) — today its merge runs under \
             the state lock and no such window exists (the RED)"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Serial guard: the park env is process-wide; never arm two parks at
/// once. Poison-recovering — a RED test panics by design while holding
/// it, and the siblings still need the serialization.
static PARK_LOCK: Mutex<()> = Mutex::new(());

fn park_lock() -> std::sync::MutexGuard<'static, ()> {
    PARK_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct ParkArm;

impl ParkArm {
    fn new() -> Self {
        std::env::set_var(PARK_ENV, PARK_STAGE);
        ParkArm
    }
}

impl Drop for ParkArm {
    fn drop(&mut self) {
        std::env::remove_var(PARK_ENV);
    }
}

/// Park one compaction in its merge phase in a thread; run `op` while it
/// is parked and record whether the op COMPLETED during the parked
/// window; release the park and join. The release always runs, so
/// today's blocked op cannot hang the suite — the RED is the flag.
fn complete_while_parked(db: &Arc<Db>, d: &Path, op: impl FnOnce() + Send + 'static) -> bool {
    let _arm = ParkArm::new();
    let compact_t = {
        let db = Arc::clone(db);
        std::thread::spawn(move || db.compact().unwrap())
    };
    wait_for_park(d);
    let done = Arc::new(AtomicBool::new(false));
    let op_t = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            op();
            done.store(true, Ordering::SeqCst);
        })
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let completed = done.load(Ordering::SeqCst);
    drop(_arm); // disarm future parks — the parked thread is released by
                // deleting the marker FILE, not by the env.
    std::fs::remove_file(d.join(PARK_STAGE)).expect("release the park");
    compact_t.join().expect("compact thread");
    op_t.join().expect("op thread");
    completed
}

// ---------------------------------------------------------------------------
// csc001 — a put completes while the merge is parked in segment I/O
// ---------------------------------------------------------------------------

#[test]
fn csc001_put_completes_while_the_compaction_is_parked_in_segment_io() {
    let _serial = park_lock();
    let d = dir("csc001-isolation");
    let db = Arc::new(open_quiet(&d));
    // Two flushes → two L0 segments for the merge to consume.
    put_range(&db, 0, 20);
    db.flush().unwrap();
    put_range(&db, 20, 40);
    db.flush().unwrap();
    let completed = complete_while_parked(&db, &d, {
        let db = Arc::clone(&db);
        move || {
            db.put(b"park-probe", b"p")
                .expect("put during the parked merge");
        }
    });
    assert!(
        completed,
        "a put must complete while the merge is parked in segment I/O — \
         today the compaction holds the state lock across its merge, so \
         the put blocks for the whole compaction"
    );
}

// ---------------------------------------------------------------------------
// csc002 — the lock-scope counters exist and bound the hold structurally
// ---------------------------------------------------------------------------

#[test]
fn csc002_compaction_lock_scope_counters_exist_and_bound_the_hold() {
    let d = dir("csc002-counters");
    let db = open_quiet(&d);
    put_range(&db, 0, 1000);
    db.flush().unwrap();
    put_range(&db, 1000, 2000);
    db.flush().unwrap();
    let before = db.stats().write;
    db.compact().unwrap();
    let after = db.stats().write;
    let total = after.compact_total_ns - before.compact_total_ns;
    let hold = after.compact_state_lock_hold_ns - before.compact_state_lock_hold_ns;
    let io = after.compact_io_ns - before.compact_io_ns;
    let publish = after.compact_publish_ns - before.compact_publish_ns;
    assert!(
        total > 0 && hold > 0 && io > 0 && publish > 0,
        "a real merge must populate all four lock-scope counters \
         (total={total} hold={hold} io={io} publish={publish})"
    );
    // The structural invariant: A+C (the hold) and B (the I/O) are
    // disjoint windows inside the whole compaction — the state lock must
    // never cover merge construction. Wall-proof: it follows from where
    // the phases live, not from any timing threshold.
    assert!(
        hold + io <= total,
        "hold (A+C) + io (B) must fit inside the total \
         ({hold} + {io} > {total})"
    );
    assert!(
        hold < total && io < total,
        "each phase must be a strict slice of the total (hold={hold} io={io} total={total})"
    );
}

// ---------------------------------------------------------------------------
// csc003 — a flush completing during the merge makes the compaction stale
// ---------------------------------------------------------------------------

#[test]
fn csc003_a_flush_completing_during_the_merge_makes_the_compaction_stale() {
    let _serial = park_lock();
    let d = dir("csc003-stale-publication");
    let db = Arc::new(open_quiet(&d));
    // Two flushes → two L0 segments the parked merge consumes; the third
    // flush (during the parked merge) carries its own data and bumps the
    // generation the merge captured at A.
    put_range(&db, 0, 20);
    db.flush().unwrap();
    put_range(&db, 20, 40);
    db.flush().unwrap();
    let gen_before = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    let _arm = ParkArm::new();
    let compact_t = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.compact().unwrap())
    };
    wait_for_park(&d);
    // The flush that makes the parked merge stale. Today it blocks behind
    // the merge's state lock — the RED fires at wait_for_park above.
    put_range(&db, 40, 60);
    db.flush().unwrap();
    let flushed_gen = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    drop(_arm);
    std::fs::remove_file(d.join(PARK_STAGE)).expect("release the park");
    let stats = compact_t.join().expect("compact thread");
    assert!(
        stats.stale,
        "the interleaved flush must mark the parked merge stale — its \
         publication would otherwise overwrite the flush's segments"
    );
    assert_eq!(
        stats.segments_in, 2,
        "the merge must have consumed the two seeded segments before the stale discard"
    );
    drop(db);
    // The stale publication must have published nothing: CURRENT stays at
    // the flush's generation, the input segment files survive, and all
    // three flushes' data is present.
    assert_eq!(
        Current::read(&d.join("CURRENT"))
            .unwrap()
            .manifest_generation,
        flushed_gen,
        "a stale merge must not advance CURRENT past the interleaved flush"
    );
    assert!(
        flushed_gen > gen_before,
        "the interleaved flush itself must have published (before={gen_before} after={flushed_gen})"
    );
    assert_eq!(
        segment_file_count(&d),
        3,
        "a stale merge must not publish or delete — the three flushed \
         segments must all survive on disk"
    );
    // Residue discipline: the discard must remove its staging directory —
    // the reopen sweep only exists for crash leftovers.
    let staging_left: Vec<String> = std::fs::read_dir(&d)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".compact-staging-")
        })
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        staging_left.is_empty(),
        "the stale discard must remove its staging directory (left: {staging_left:?})"
    );
    let reopened = open_quiet(&d);
    let want: BTreeMap<Vec<u8>, Vec<u8>> = (0..60)
        .map(|i| {
            (
                format!("k{i:05}").into_bytes(),
                format!("v{i}").into_bytes(),
            )
        })
        .collect();
    assert_eq!(
        walk(&reopened),
        want,
        "the stale discard must lose nothing — all three flushes' data survives the reopen"
    );
}
