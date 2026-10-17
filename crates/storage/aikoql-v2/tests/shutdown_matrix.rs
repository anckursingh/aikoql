//! L-15 (TDD-022 + B 17/18) — the shutdown matrix and the close races.
//!
//! The challenge's item 22: shutdown while idle / reading / staged /
//! publishing / checkpointing / GroupCommit-has-queued-work — no deadlock,
//! no panic, no acknowledged write lost, no orphan becomes authoritative.
//!
//! The deterministic interleave is the park hook, per state:
//!
//! sd001 — idle: a plain drop loses nothing.
//! sd002 — reading: reader threads race a compactor merge that is parked
//!   mid-flight (their Arc segments keep the old reads correct); the
//!   shutdown (the drop of the last Arc) completes after them.
//! sd003 — staged: the merge parks at after_segment (staged output built);
//!   the drop joins the compactor and blocks until the release; the
//!   merge then publishes ATOMICALLY — the reopen is coherent.
//! sd004 — publishing: the merge parks at after_manifest (manifest
//!   written, CURRENT pending); the same join/release; the publish
//!   completes and the reopen is coherent.
//! sd005 — checkpointing (B 17, the checkpoint/close race): the process-
//!   death close is the kill-window estate — ckp004's five publication-
//!   window kills (temp_write/temp_fsync/after_checkpoint/after_first_
//!   prune/after_prune, directory_checkpoint.rs) plus the L-11 ckp010
//!   interleave windows. A graceful drop cannot race a checkpoint: it
//!   runs on the caller's borrowed &Db (no orphan can publish — the
//!   staged+atomic protocol is the same one ckp004 pins).
//! sd006a — GroupCommit has queued work (B 18): a batch parked at
//!   after_apply (applied, un-acked) when the drop starts; the drop's
//!   committer join blocks on the release; the ack fires and the row
//!   survives the reopen — no acknowledged write lost.
//! sd006b — GroupCommit drain (B 18): the drop starts while three
//!   writers stream batches; the committer drains every submitted group
//!   (each acked) and the reopen holds every one of them.
//!
//! One binary: the park env is process-wide, so EVERY test serializes on
//! PARK_LOCK (the compaction_lock_scope discipline — a sibling's
//! unguarded merge would park on nobody's marker).

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use aikoql_storage_v2::format::Current;
use common::dir;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COMPACT_ENV: &str = "AIKOQL_V2_COMPACT_PARK";
const GROUP_ENV: &str = "AIKOQL_V2_GROUP_PARK";

fn k(r: u32, i: u32) -> Vec<u8> {
    format!("k{r:03}{i:02}").into_bytes()
}

fn v(r: u32, i: u32) -> Vec<u8> {
    // 40-byte values (the bgc001 sizing): eight puts cross the 512-byte
    // memtable, one flush per round.
    format!("v{r:03}{i:02}{}", "y".repeat(34)).into_bytes()
}

/// Four rounds of eight puts: with the 512-byte memtable each round is a
/// flush, and the 4th crosses l0_compact_trigger (default 4) — the
/// background merge kicks (the bgc001 pattern).
fn round_put(db: &Db, r: u32) {
    for i in 0..8 {
        db.put(&k(r, i), &v(r, i)).unwrap();
    }
}

/// A db whose 512-byte memtable makes every round a flush; background
/// compaction stays on (the matrix's staged/publishing states ride it).
fn open_flushy(d: &Path) -> Db {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.memtable_bytes = 512;
    cfg.durability = DurabilityMode::Async;
    Db::open(cfg).unwrap()
}

fn walk(db: &Db) -> BTreeMap<Vec<u8>, Vec<u8>> {
    db.scan(b"").unwrap().into_iter().collect()
}

fn wait_for(path: &Path, what: &str) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "{what} marker never appeared — the parked window does not exist (the RED)"
        );
        std::thread::sleep(Duration::from_millis(20));
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

/// Serial guard: the park env is process-wide; never arm two parks at
/// once. Poison-recovering — a RED test panics by design while holding
/// it, and the siblings still need the serialization.
static PARK_LOCK: Mutex<()> = Mutex::new(());

fn park_lock() -> std::sync::MutexGuard<'static, ()> {
    PARK_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct ParkArm;

impl ParkArm {
    fn new(env: &'static str, stage: &'static str) -> Self {
        std::env::set_var(env, stage);
        ParkArm
    }
}

impl Drop for ParkArm {
    fn drop(&mut self) {
        std::env::remove_var(COMPACT_ENV);
        std::env::remove_var(GROUP_ENV);
    }
}

/// The four rounds every staged/publishing/reading leg seeds — the merge
/// of the four flushed segments parks at `stage` on the compactor thread.
fn seed_parked_merge(d: &Path, stage: &'static str) -> Db {
    let db = open_flushy(d);
    let _arm = ParkArm::new(COMPACT_ENV, stage);
    for r in 1..=4 {
        round_put(&db, r);
    }
    // Once parked, the MARKER FILE holds the merge (the env is only
    // consulted at the park point) — the arm drops here, env discipline
    // intact; release() deletes the file.
    wait_for(&d.join(stage), "compaction park");
    db
}

fn release(d: &Path, stage: &str) {
    std::fs::remove_file(d.join(stage)).expect("release the park");
    std::env::remove_var(COMPACT_ENV);
    std::env::remove_var(GROUP_ENV);
}

fn all_32() -> BTreeMap<Vec<u8>, Vec<u8>> {
    (1..=4u32)
        .flat_map(|r| (0..8u32).map(move |i| (k(r, i), v(r, i))))
        .collect()
}

/// The coherent-reopen proof shared by the staged/publishing legs: the
/// merged publish landed atomically — CURRENT names a generation whose
/// manifest references exactly the segment files on disk, and no row
/// moved.
fn assert_coherent_reopen(d: &Path) {
    let db = open_flushy(d);
    assert_eq!(walk(&db), all_32(), "every acked row survives the reopen");
    assert_eq!(segment_file_count(d), 1, "the merge landed as one segment");
    let gen = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    assert!(
        d.join(format!("MANIFEST-{gen:06}")).exists(),
        "CURRENT names a published manifest"
    );
    // no orphan became authoritative: every on-disk segment is referenced
    // (a half-published merge would leave an unreferenced staged file)
    assert!(
        !std::fs::read_dir(d).unwrap().flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with(".compact-staging-")),
        "the staging dir is gone"
    );
}

// ---------------------------------------------------------------------------
// sd001 — idle
// ---------------------------------------------------------------------------

#[test]
fn sd001_shutdown_idle_loses_nothing() {
    let _serial = park_lock();
    let d = dir("sd001-idle");
    let db = open_flushy(&d);
    round_put(&db, 1);
    round_put(&db, 2);
    drop(db);
    let db = open_flushy(&d);
    let want: BTreeMap<Vec<u8>, Vec<u8>> = (1..=2u32)
        .flat_map(|r| (0..8u32).map(move |i| (k(r, i), v(r, i))))
        .collect();
    assert_eq!(walk(&db), want, "an idle shutdown loses nothing");
}

// ---------------------------------------------------------------------------
// sd002 — reading
// ---------------------------------------------------------------------------

#[test]
fn sd002_readers_racing_a_parked_merge_shutdown_stay_correct() {
    let _serial = park_lock();
    let d = dir("sd002-reading");
    let db = Arc::new(seed_parked_merge(&d, "after_segment"));
    // Readers storm the seeded keys while the merge is parked mid-flight:
    // their Arc segments answer correctly whether or not the publish
    // lands mid-storm (the swap never invalidates a held segment).
    let mut readers = Vec::new();
    for t in 0..3 {
        let db = Arc::clone(&db);
        readers.push(std::thread::spawn(move || {
            for rep in 0..60 {
                for r in 1..=4u32 {
                    for i in 0..8u32 {
                        let got = db.get(&k(r, i)).unwrap();
                        assert_eq!(
                            got.as_deref(),
                            Some(v(r, i).as_slice()),
                            "reader {t} rep {rep} key {r}:{i}"
                        );
                    }
                }
            }
        }));
    }
    // The shutdown: Db::drop runs on whichever thread releases the LAST
    // Arc (main's, or a reader's) and joins the now-released compactor —
    // every join below returns only after that shutdown completed.
    release(&d, "after_segment");
    drop(db);
    for h in readers {
        h.join().expect("a reader thread panicked");
    }
    assert_coherent_reopen(&d);
}

// ---------------------------------------------------------------------------
// sd003/sd004 — staged / publishing: the drop joins the parked merge
// ---------------------------------------------------------------------------

fn shutdown_joining_a_parked_merge(stage: &'static str, tag: &str) {
    let _serial = park_lock();
    let d = dir(tag);
    let db = seed_parked_merge(&d, stage);
    // The drop runs on its own thread: Db::drop signals the compactor and
    // JOINS it — the join blocks while the merge is parked (the release
    // below is the only way it completes: no deadlock, no abandon).
    let drop_t = std::thread::spawn(move || drop(db));
    release(&d, stage);
    drop_t.join().expect("the drop thread must complete");
    assert_coherent_reopen(&d);
}

#[test]
fn sd003_shutdown_while_staged_joins_then_publishes_atomically() {
    shutdown_joining_a_parked_merge("after_segment", "sd003-staged");
}

#[test]
fn sd004_shutdown_while_publishing_lands_coherent() {
    shutdown_joining_a_parked_merge("after_manifest", "sd004-publishing");
}

// ---------------------------------------------------------------------------
// sd006 — GroupCommit has queued work (B 18)
// ---------------------------------------------------------------------------

fn open_group(d: &Path) -> Db {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.durability = DurabilityMode::GroupCommit;
    Db::open(cfg).unwrap()
}

#[test]
fn sd006a_parked_group_batch_acks_and_survives_shutdown() {
    let _serial = park_lock();
    let d = dir("sd006a-group-parked");
    let db = open_group(&d);
    for i in 0..20u32 {
        db.put(format!("s{i:03}").as_bytes(), b"seed").unwrap();
    }
    // Arm AFTER the seeds: the next batch parks at after_apply — applied
    // to the memtable, fsynced to the WAL, NOT yet acked.
    let _arm = ParkArm::new(GROUP_ENV, "after_apply");
    let writer = db.writer().unwrap();
    let write_t = {
        let writer = writer.clone();
        std::thread::spawn(move || {
            writer.write(&[aikoql_storage_v2::wal::Op::Put(
                b"parked".to_vec(),
                b"p".to_vec(),
            )])
        })
    };
    wait_for(&d.join("after_apply"), "group park");
    // The shutdown starts with the batch queued: the drop's committer
    // join blocks until the release.
    let drop_t = std::thread::spawn(move || drop(db));
    release(&d, "after_apply");
    let seq = write_t.join().expect("writer thread").unwrap();
    assert!(seq > 0, "the parked batch must still ack after the release");
    drop(writer); // the queue disconnects, the committer exits, the join
                  // completes — the drop-order rule the writer documents.
    drop_t.join().expect("the drop thread must complete");
    let db = open_group(&d);
    assert_eq!(
        db.get(b"parked").unwrap().as_deref(),
        Some(b"p".as_slice()),
        "no acknowledged write lost: the parked batch's ack is real"
    );
    for i in 0..20u32 {
        assert_eq!(
            db.get(format!("s{i:03}").as_bytes()).unwrap(),
            Some(b"seed".to_vec())
        );
    }
}

#[test]
fn sd006b_group_drain_while_shutting_down_loses_no_acked_write() {
    let _serial = park_lock();
    let d = dir("sd006b-group-drain");
    let db = open_group(&d);
    let writer = db.writer().unwrap();
    // The drop starts while three writers stream batches through their
    // handles: the queue stays open until every handle drops, so the
    // drop's committer join drains each submitted group first — every
    // write acks, every acked write must survive the reopen.
    let drop_t = std::thread::spawn(move || drop(db));
    let acked: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut writers = Vec::new();
    for t in 0..3u32 {
        let writer = writer.clone();
        let acked = Arc::clone(&acked);
        writers.push(std::thread::spawn(move || {
            for i in 0..50u32 {
                let key = format!("w{t:03}{i:03}").into_bytes();
                writer
                    .write(&[aikoql_storage_v2::wal::Op::Put(key.clone(), key.clone())])
                    .expect("a submitted batch must ack while any handle lives");
                acked.lock().unwrap().push(t * 100 + i);
            }
        }));
    }
    for h in writers {
        h.join().expect("a writer thread panicked");
    }
    drop(writer);
    drop_t.join().expect("the drop thread must complete");
    let db = open_group(&d);
    for id in acked.lock().unwrap().iter() {
        let t = id / 100;
        let i = id % 100;
        let key = format!("w{t:03}{i:03}").into_bytes();
        assert_eq!(
            db.get(&key).unwrap().as_deref(),
            Some(key.as_slice()),
            "acked write w{t:03}{i:03} lost"
        );
    }
    // nothing beyond the acked set leaked in (the drain is the whole
    // queue; a closed-queue write fails at submit, never half-lands)
    let rows = walk(&db);
    assert_eq!(rows.len(), 150, "exactly the 150 acked rows landed");
}

// ---------------------------------------------------------------------------
// sd005 — checkpointing (B 17): no new test. The checkpoint/close race is
// pinned twice already: the process-death close by ckp004's five kill
// windows (ckp004_window_checkpoint_temp_write/_temp_fsync/after_checkpoint/
// after_first_prune/after_prune — each a kill mid-publication whose reopen
// must never see a half checkpoint as authoritative) and the L-11 ckp010
// interleave windows. The graceful close cannot race a checkpoint at all:
// checkpoint_now borrows the caller's &Db, so a drop cannot even run until
// it returns — the orphan can never publish. A source-grep re-pin would
// assert the estate's text, not its behavior.
// ---------------------------------------------------------------------------
