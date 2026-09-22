//! P5-M38 (R4-P0-01) — flush lock-scope split REDs. `flush()` holds the
//! state WRITE lock across segment construction, disk I/O, checksumming,
//! reader reopen, and publication (flush_locked_impl): every concurrent
//! reader or writer waits for the complete flush, and the write path can
//! pay one inline. The split — A short lock (rotate active→immutable,
//! capture the publication generation, detach the work) → B NO lock
//! (encode/write/validate/reopen) → C short generation-checked publish —
//! with the counters flush_total_ns / flush_state_lock_hold_ns /
//! flush_io_ns / flush_publish_ns and the structural invariant
//! hold << total (the state lock must never cover segment file
//! construction). Flushes serialize on a flush pipe so one flush's WAL
//! truncate can never race another flush's unpublished segments.
//!
//! fsc001 — a put completes while the flush is parked in its segment-I/O
//!   phase (AIKOQL_V2_FLUSH_IO_PARK=in_io — today that window does not
//!   exist: the flush's I/O runs under the state lock, so a put blocks
//!   for the whole flush. RED: the marker never appears);
//! fsc002 — the four lock-scope counters exist, are populated by a real
//!   flush, and hold (A+C) + io (B) ≤ total — the disjoint windows must
//!   not overlap (compile-error RED today: the fields do not exist);
//! fsc003 — stale-publication pin: a compaction COMPLETES while the
//!   flush is parked in I/O, and the flush's publication must not
//!   overwrite it — the merge AND the flush survive a reopen (today the
//!   compaction blocks on the state lock the flush holds — the scenario
//!   cannot run. RED: the marker never appears);
//! fsc004 — the flush pipe: a second flush cannot publish while the
//!   first is parked in I/O (CURRENT stays put until the release) — its
//!   WAL truncate must never race the first's unpublished segments.
//!
//! One binary: the park env is process-wide; every park-arming test
//! serializes on PARK_LOCK and the release always runs (the guard), so a
//! blocked op today cannot hang the suite — the RED is the flag, not a
//! hang.

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use aikoql_storage_v2::format::Current;
use common::dir;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PARK_ENV: &str = "AIKOQL_V2_FLUSH_IO_PARK";
const PARK_STAGE: &str = "in_io";

/// A db whose writes never auto-flush (the memtable sits far above the
/// test workload) and never auto-compact: every flush/compact here is
/// the test's own explicit call. Async durability keeps the seeds fast —
/// the flush's own segment fsyncs are unaffected.
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

fn wait_for_park(d: &Path) {
    let marker = d.join(PARK_STAGE);
    let start = Instant::now();
    while !marker.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the flush must have an unlocked segment-I/O phase \
             ({PARK_ENV}={PARK_STAGE}) — today its segment I/O runs under \
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

/// Park one flush in its segment-I/O phase in a thread; run `op` while it
/// is parked and record whether the op COMPLETED during the parked
/// window; release the park and join. The release always runs, so
/// today's blocked op cannot hang the suite — the RED is the flag.
fn complete_while_parked(db: &Arc<Db>, d: &Path, op: impl FnOnce() + Send + 'static) -> bool {
    let _arm = ParkArm::new();
    let flush_t = {
        let db = Arc::clone(db);
        std::thread::spawn(move || db.flush().unwrap())
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
    flush_t.join().expect("flush thread");
    op_t.join().expect("op thread");
    completed
}

// ---------------------------------------------------------------------------
// fsc001 — a put completes while the flush is parked in segment I/O
// ---------------------------------------------------------------------------

#[test]
fn fsc001_put_completes_while_the_flush_is_parked_in_segment_io() {
    let _serial = park_lock();
    let d = dir("fsc001-isolation");
    let db = Arc::new(open_quiet(&d));
    put_range(&db, 0, 100);
    let completed = complete_while_parked(&db, &d, {
        let db = Arc::clone(&db);
        move || {
            db.put(b"park-probe", b"p").expect("put during the parked I/O");
        }
    });
    assert!(
        completed,
        "a put must complete while the flush is parked in segment I/O — \
         today the flush holds the state lock across its I/O, so the put \
         blocks for the whole flush"
    );
}

// ---------------------------------------------------------------------------
// fsc002 — the lock-scope counters exist and bound the hold structurally
// ---------------------------------------------------------------------------

#[test]
fn fsc002_flush_lock_scope_counters_exist_and_bound_the_hold() {
    let d = dir("fsc002-counters");
    let db = open_quiet(&d);
    put_range(&db, 0, 2000);
    let before = db.stats().write;
    db.flush().unwrap();
    let after = db.stats().write;
    let total = after.flush_total_ns - before.flush_total_ns;
    let hold = after.flush_state_lock_hold_ns - before.flush_state_lock_hold_ns;
    let io = after.flush_io_ns - before.flush_io_ns;
    let publish = after.flush_publish_ns - before.flush_publish_ns;
    assert!(
        total > 0 && hold > 0 && io > 0 && publish > 0,
        "a real flush must populate all four lock-scope counters \
         (total={total} hold={hold} io={io} publish={publish})"
    );
    // The structural invariant: A+C (the hold) and B (the I/O) are
    // disjoint windows inside the whole flush — the state lock must
    // never cover segment file construction. Wall-proof: it follows from
    // where the phases live, not from any timing threshold.
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
// fsc003 — a compaction completing during the flush is not overwritten
// ---------------------------------------------------------------------------

#[test]
fn fsc003_a_compaction_completing_during_the_flush_is_not_overwritten() {
    let _serial = park_lock();
    let d = dir("fsc003-stale-publication");
    let db = Arc::new(open_quiet(&d));
    // Two flushes → two L0 segments for the merge to consume; the third
    // flush (parked) carries its own data.
    put_range(&db, 0, 20);
    db.flush().unwrap();
    put_range(&db, 20, 40);
    db.flush().unwrap();
    put_range(&db, 40, 60);
    let gen_before = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    let compacted = complete_while_parked(&db, &d, {
        let db = Arc::clone(&db);
        move || {
            db.compact().expect("merge during the parked I/O");
        }
    });
    assert!(
        compacted,
        "the compaction must complete while the flush is parked in segment \
         I/O — today the flush holds the state lock across its I/O, so the \
         merge blocks behind it"
    );
    drop(db);
    // The interleaved publication must survive: the merge AND the parked
    // flush both present, nothing overwritten.
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
        "the flush's publication must not overwrite the interleaved \
         compaction — both sets of data must survive the reopen"
    );
    let gen_after = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    assert!(
        gen_after > gen_before,
        "CURRENT must name a generation past the interleave (before={gen_before} after={gen_after})"
    );
}

// ---------------------------------------------------------------------------
// fsc004 — a second flush cannot publish while the first is in I/O
// ---------------------------------------------------------------------------

#[test]
fn fsc004_a_second_flush_cannot_publish_while_the_first_is_in_io() {
    let _serial = park_lock();
    let d = dir("fsc004-flush-pipe");
    let db = Arc::new(open_quiet(&d));
    put_range(&db, 0, 100);
    let gen_before = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    let _arm = ParkArm::new();
    let flush_t = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.flush().unwrap())
    };
    wait_for_park(&d);
    // A second flush (with its own data) must serialize behind the
    // first's I/O: its WAL truncate must never race the first flush's
    // unpublished segments.
    put_range(&db, 100, 200);
    let second_done = Arc::new(AtomicBool::new(false));
    let second_t = {
        let (db, flag) = (Arc::clone(&db), Arc::clone(&second_done));
        std::thread::spawn(move || {
            db.flush().unwrap();
            flag.store(true, Ordering::SeqCst);
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    let advanced =
        Current::read(&d.join("CURRENT")).unwrap().manifest_generation > gen_before;
    let done_early = second_done.load(Ordering::SeqCst);
    drop(_arm);
    std::fs::remove_file(d.join(PARK_STAGE)).expect("release the park");
    flush_t.join().expect("first flush thread");
    second_t.join().expect("second flush thread");
    assert!(
        !advanced && !done_early,
        "a second flush must not publish while the first is parked in I/O \
         — CURRENT must stay put until the release"
    );
    drop(db);
    let reopened = open_quiet(&d);
    assert_eq!(
        walk(&reopened).len(),
        200,
        "both flushes' data survives the reopen"
    );
}
