//! PR6-004 — the review's P0 Concurrency finding: background compaction,
//! checkpoint, flush, write and close share one lock set, and the claimed
//! ordering (`state` → `wal`, wal innermost — see Db.wal's doc) must be
//! PROVED by a real concurrent matrix, not asserted in prose. The matrix
//! runs all five actors together under a hard wall-clock timeout — any
//! deadlock in write/flush/compact/checkpoint/close hangs the supervisor's
//! swap or join and trips the timeout — then reopens and verifies EVERY
//! acknowledged write is present. The ack half is the mandatory one:
//! no-deadlock alone says nothing about what the survivors wrote.
//!
//! PR6-R2-005: the close/reopen swap no longer trusts a fixed drain sleep.
//! A per-actor acknowledgement barrier (Slot::episode / Slot::drained)
//! proves every actor has stopped entering new work before the write guard
//! is taken; the write guard itself then waits for the ops already inside
//! (RwLock semantics), and Db's Drop joins the engine's own threads.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::identity::ObjectId;
use common::dir;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

/// The review's actor budget: 8 writers, 2 flushers, 1 background
/// compactor (the engine's own thread), 1 checkpoint worker, 1
/// close/reopen actor.
const WRITERS: usize = 8;
const FLUSHERS: usize = 2;
/// The actors that must acknowledge each park round: writers + flushers +
/// the checkpoint worker. (The supervisor parks; it is not an actor.)
const ACTORS: usize = WRITERS + FLUSHERS + 1;
const PHASES: usize = 3;
const PHASE_MS: u64 = 600;
/// The review's "hard timeout": the supervisor must finish the whole
/// matrix inside this budget or the test panics (a deadlocked actor blocks
/// the swap/join and trips it).
const HARD_TIMEOUT: Duration = Duration::from_secs(180);

/// The shared Db slot. Every op holds a read guard for its whole duration;
/// the close/reopen actor swaps the Db under the write guard — so a close
/// waits for in-flight ops, then drops the Db mid-matrix, which joins the
/// background compactor (possibly mid-merge) and the committer thread —
/// the exact shutdown the finding wants raced.
struct Slot {
    db: RwLock<Option<Db>>,
    park: AtomicBool,
    /// PR6-R2-005 — the deterministic drain barrier. `episode` is the
    /// current park round; `drained` counts the actors that acknowledged
    /// "stopped entering new work" for it. The supervisor bumps `episode`
    /// when parking and waits for drained == ACTORS * episode before
    /// taking the write guard — no sleep-as-synchronization.
    episode: AtomicUsize,
    drained: AtomicUsize,
}

fn open_cfg(path: &std::path::Path) -> Config {
    let mut cfg = Config::new(path.to_path_buf());
    cfg.l0_compact_trigger = 2; // every flush-pair arms the compactor
    cfg.checkpoint_bytes = 0; // the checkpoint actor owns checkpoints
    cfg
}

fn oid(writer: u8) -> ObjectId {
    ObjectId([writer; 16])
}

/// (oid, key, value) for every acknowledged write — the oracle.
type Ack = (ObjectId, Vec<u8>, Vec<u8>);

/// The actor side of the drain barrier: when parked, acknowledge the
/// current round exactly once ("I have stopped entering new work"), then
/// wait for the release. The once-per-round guard is per-actor state, so
/// a slow actor can never double-ack and fake the barrier.
fn ack_if_parked(slot: &Slot, my_episode: &mut usize) -> bool {
    if !slot.park.load(Ordering::SeqCst) {
        return false;
    }
    let ep = slot.episode.load(Ordering::SeqCst);
    if *my_episode != ep {
        *my_episode = ep;
        slot.drained.fetch_add(1, Ordering::SeqCst);
    }
    thread::sleep(Duration::from_millis(2));
    true
}

/// The supervisor side: start the next park round and wait for every
/// actor to acknowledge it. The bound is a panic, not a hope — a
/// deadlocked actor (stuck inside the Db) trips it instead of a fixed
/// sleep letting the swap race it.
fn park_and_drain(slot: &Slot) {
    slot.episode.fetch_add(1, Ordering::SeqCst);
    slot.park.store(true, Ordering::SeqCst);
    let ep = slot.episode.load(Ordering::SeqCst);
    let t0 = Instant::now();
    loop {
        if slot.drained.load(Ordering::SeqCst) >= ACTORS * ep {
            return;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "drain barrier stuck at {}/{} acks for episode {ep} — an actor is deadlocked",
            slot.drained.load(Ordering::SeqCst),
            ACTORS * ep
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn concurrent_write_flush_compact_checkpoint_close_has_no_deadlock() {
    let d = dir("pr6-004-matrix");
    let slot = Arc::new(Slot {
        db: RwLock::new(Some(Db::open(open_cfg(&d)).unwrap())),
        park: AtomicBool::new(false),
        episode: AtomicUsize::new(0),
        drained: AtomicUsize::new(0),
    });
    let stop = Arc::new(AtomicBool::new(false));
    // (oid, key, value) for every acknowledged write — the oracle.
    let acks: Arc<Mutex<Vec<Ack>>> = Arc::new(Mutex::new(Vec::new()));
    let (done_tx, done_rx) = mpsc::channel();

    let mut handles = Vec::new();
    for w in 0..WRITERS as u8 {
        let slot = Arc::clone(&slot);
        let stop = Arc::clone(&stop);
        let acks = Arc::clone(&acks);
        handles.push(thread::spawn(move || {
            let mut iter = 0u64;
            let mut my_episode = 0usize;
            while !stop.load(Ordering::SeqCst) {
                if ack_if_parked(&slot, &mut my_episode) {
                    continue;
                }
                let guard = slot.db.read().unwrap();
                let db = match guard.as_ref() {
                    Some(db) => db,
                    None => continue,
                };
                let key = iter.to_be_bytes().to_vec();
                let mut value = vec![w];
                value.extend_from_slice(&key);
                if db.put_object(oid(w), &key, &value).is_ok() {
                    acks.lock().unwrap().push((oid(w), key, value));
                    iter += 1;
                }
            }
        }));
    }
    for _ in 0..FLUSHERS as u8 {
        let slot = Arc::clone(&slot);
        let stop = Arc::clone(&stop);
        handles.push(thread::spawn(move || {
            let mut my_episode = 0usize;
            while !stop.load(Ordering::SeqCst) {
                if ack_if_parked(&slot, &mut my_episode) {
                    continue;
                }
                let guard = slot.db.read().unwrap();
                if let Some(db) = guard.as_ref() {
                    let _ = db.flush();
                }
                thread::sleep(Duration::from_millis(10));
            }
        }));
    }
    {
        let slot = Arc::clone(&slot);
        let stop = Arc::clone(&stop);
        handles.push(thread::spawn(move || {
            let mut my_episode = 0usize;
            while !stop.load(Ordering::SeqCst) {
                if ack_if_parked(&slot, &mut my_episode) {
                    continue;
                }
                let guard = slot.db.read().unwrap();
                if let Some(db) = guard.as_ref() {
                    let _ = db.checkpoint_now();
                }
                thread::sleep(Duration::from_millis(20));
            }
        }));
    }

    // The close/reopen actor and matrix supervisor: PHASES swap rounds,
    // then one final close + reopen for the ack oracle.
    let d_sup = d.clone();
    let sup_slot = Arc::clone(&slot);
    let sup_stop = Arc::clone(&stop);
    let supervisor = thread::spawn(move || {
        let t0 = Instant::now();
        for phase in 0..PHASES {
            thread::sleep(Duration::from_millis(PHASE_MS));
            park_and_drain(&sup_slot);
            {
                let mut guard = sup_slot.db.write().unwrap();
                *guard = None; // close: joins compactor/committer mid-flight
                *guard = Some(Db::open(open_cfg(&d_sup)).unwrap());
            }
            sup_slot.park.store(false, Ordering::SeqCst);
            eprintln!("pr6-004 phase {phase} at {}s", t0.elapsed().as_secs());
        }
        // Final park + close; actors exit; one last reopen verifies acks.
        park_and_drain(&sup_slot);
        *sup_slot.db.write().unwrap() = None;
        *sup_slot.db.write().unwrap() = Some(Db::open(open_cfg(&d_sup)).unwrap());
        sup_stop.store(true, Ordering::SeqCst);
        done_tx.send(()).unwrap();
    });

    match done_rx.recv_timeout(HARD_TIMEOUT) {
        Ok(()) => {}
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "deadlock: the write/flush/compact/checkpoint/close matrix did not finish in {HARD_TIMEOUT:?}"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("matrix supervisor died early"),
    }
    supervisor.join().unwrap();
    for h in handles {
        h.join().unwrap();
    }

    // The mandatory second assertion: EVERY acknowledged write survives.
    let acks = acks.lock().unwrap().clone();
    // The structural floor is per-writer progress, not throughput: CI
    // evidence (Windows runs at bf79d41: 80 acks on a loaded runner, 54 on
    // the rerun — ~22-33ms per group-commit fsync on GitHub's Windows
    // hosts, vs ~2ms on Linux) shows a `> WRITERS*10` floor measures the
    // runner's disk, not the matrix. Every writer must still have made
    // visible progress through the drain barrier, and the loop below proves
    // every acknowledged write survives close/reopen.
    let distinct_writers: std::collections::BTreeSet<ObjectId> =
        acks.iter().map(|(o, _, _)| *o).collect();
    assert_eq!(
        distinct_writers.len(),
        WRITERS,
        "not every writer acknowledged a write ({distinct_writers:?}) — \
         the matrix did not exercise the full write path"
    );
    let db = slot.db.read().unwrap();
    let db = db.as_ref().unwrap();
    db.wait_compactor_idle();
    assert!(
        db.last_compaction_error().is_none(),
        "a background merge failed: {:?}",
        db.last_compaction_error()
    );
    for (oid, key, value) in acks {
        assert_eq!(
            db.get_object(oid, &key).unwrap(),
            Some(value.clone()),
            "acknowledged write {oid} {key:?} lost across close/reopen"
        );
    }
}
