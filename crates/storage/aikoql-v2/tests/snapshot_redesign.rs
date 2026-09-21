//! P5-M33 (P1-04) — snapshot lock redesign REDs. `snapshot_to` holds the
//! state READ lock across capture + copy + verify + marker (deliberate
//! today, §58): writers and checkpoints block for the whole snapshot. The
//! redesign captures the file set under the lock, PINS it (the prune
//! surfaces skip pinned names), releases the lock for the bulk copy, and
//! disarms the pin at the end — see docs/snapshot-crash-protocol.md (the
//! review's condition: the protocol is documented before the change).
//!
//! snp000 — the protocol doc is cited from snapshot.rs (grep pin);
//! snp001 — a put completes while the snapshot is parked mid-copy;
//! snp002 — a checkpoint lands during the parked copy (child-kill window:
//!   the snapshot dir is old-or-new, never a mix, and the kill leaves it
//!   inert);
//! snp003 — the prune guard: a concurrent checkpoint's prune skips pinned
//!   files and still removes the unpinned ones it subsumes.
//!
//! One binary: the park env is process-wide and each arm must be serial
//! against the other tests here (the snapshot_matrix.rs pattern).

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::format::Current;
use aikoql_storage_v2::identity::ObjectId;
use aikoql_storage_v2::snapshot::{restore_from, SnapshotInfo};
use common::dir;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PARK_ENV: &str = "AIKOQL_V2_SNAP_PARK";
const CHILD_ENV: &str = "SRD_CHILD";
const DIR_ENV: &str = "SRD_DIR";
const SNAP_ENV: &str = "SRD_SNAP_DIR";

fn walk(db: &Db) -> BTreeMap<Vec<u8>, Vec<u8>> {
    db.scan(b"").unwrap().into_iter().collect()
}

fn wait_for(path: &Path, timeout: Duration) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < timeout,
            "marker {} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Serial guard: env is process-wide; never arm two parks at once.
/// Poison-recovering — a RED test panics by design while holding it, and
/// the siblings still need the serialization.
static PARK_LOCK: Mutex<()> = Mutex::new(());

fn park_lock() -> std::sync::MutexGuard<'static, ()> {
    PARK_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct ParkArm;

impl ParkArm {
    fn new(stage: &'static str) -> Self {
        std::env::set_var(PARK_ENV, stage);
        ParkArm
    }
}

impl Drop for ParkArm {
    fn drop(&mut self) {
        std::env::remove_var(PARK_ENV);
    }
}

/// n puts + a flush: several segments exist, the walk is the oracle.
fn seed(d: &Path, n: u64) -> (Db, BTreeMap<Vec<u8>, Vec<u8>>) {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.memtable_bytes = 512;
    cfg.l0_compact_trigger = 0;
    let db = Db::open(cfg).unwrap();
    for i in 0..n {
        db.put(format!("k{i:03}").as_bytes(), format!("v{i}").as_bytes())
            .unwrap();
    }
    db.flush().unwrap();
    let oracle = walk(&db);
    (db, oracle)
}

// ---------------------------------------------------------------------------
// snp000 — the protocol doc is wired into the module (tx000 pattern)
// ---------------------------------------------------------------------------

#[test]
fn snp000_snapshot_module_cites_the_crash_protocol() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
    let doc = std::fs::read_to_string(format!(
        "{manifest}/../../../docs/snapshot-crash-protocol.md"
    ))
    .expect("docs/snapshot-crash-protocol.md");
    for needle in [
        "## Old protocol",
        "## New protocol (M33 — the pin)",
        "## Failure windows (new protocol)",
    ] {
        assert!(
            doc.contains(needle),
            "snapshot-crash-protocol.md must document {needle:?}"
        );
    }
    let module =
        std::fs::read_to_string(format!("{manifest}/src/snapshot.rs")).expect("snapshot.rs");
    assert!(
        module.contains("snapshot-crash-protocol.md"),
        "snapshot.rs must cite docs/snapshot-crash-protocol.md — the protocol \
         is the module's contract, not the plan's"
    );
}

// ---------------------------------------------------------------------------
// snp001 — writer-latency pin: a put completes while the snapshot is parked
// ---------------------------------------------------------------------------

/// Park the snapshot mid-copy in one thread, run `op` in another while it
/// is parked, record whether the op COMPLETED during the parked window,
/// then release the park and join both. The release always runs (the
/// guard), so today's blocked op cannot hang the suite — the RED is the
/// completion flag.
fn interleave_completed_while_parked(
    db: &Arc<Db>,
    snap: &Path,
    op: impl FnOnce() + Send + 'static,
) -> (SnapshotInfo, bool) {
    let _serial = park_lock();
    let _arm = ParkArm::new("during_copy");
    let snap_t = {
        let (db, snap) = (Arc::clone(db), snap.to_path_buf());
        std::thread::spawn(move || db.snapshot_to(&snap))
    };
    wait_for(&snap.join("during_copy"), Duration::from_secs(30));
    let done = Arc::new(AtomicBool::new(false));
    let op_t = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            op();
            done.store(true, Ordering::SeqCst);
        })
    };
    let deadline = Instant::now() + Duration::from_millis(500);
    while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let completed_while_parked = done.load(Ordering::SeqCst);
    drop(_arm); // disarm future parks — the snapshot thread is parked on the
                // marker FILE itself; deleting it releases the park (the matrix pattern).
    std::fs::remove_file(snap.join("during_copy")).expect("release the park");
    let info = snap_t.join().expect("snapshot thread").expect("snapshot");
    op_t.join().expect("op thread");
    (info, completed_while_parked)
}

#[test]
fn snp001_put_completes_while_the_snapshot_is_parked() {
    let d = dir("snp001-live");
    let (db, before) = seed(&d, 40);
    let db = Arc::new(db);
    let g0 = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;

    let snap = dir("snp001-snap");
    let (info, completed) = interleave_completed_while_parked(&db, &snap, {
        let db = Arc::clone(&db);
        move || {
            db.put(b"late", b"x").unwrap();
        }
    });

    assert!(
        completed,
        "a put must complete while the snapshot is parked mid-copy — the \
         state read lock blocks writers for the snapshot's whole duration"
    );
    // The pin contract still holds regardless of the write's timing: the
    // snapshot is the pre-write generation, the write is invisible to it
    // and visible to the live db.
    assert_eq!(
        info.generation, g0,
        "the snapshot pins the pre-write generation"
    );
    let restored = restore_from(&snap, dir("snp001-target")).unwrap();
    assert_eq!(
        walk(&restored),
        before,
        "the write is invisible to the snapshot"
    );
    assert_eq!(
        db.get(b"late").unwrap(),
        Some(b"x".to_vec()),
        "the write is visible live"
    );
}

// ---------------------------------------------------------------------------
// snp002 — checkpoint-during-copy, child-kill window
// ---------------------------------------------------------------------------

fn child_dir() -> PathBuf {
    PathBuf::from(std::env::var(DIR_ENV).expect("child dir env"))
}

fn child_snap_dir() -> PathBuf {
    PathBuf::from(std::env::var(SNAP_ENV).expect("child snap dir env"))
}

/// A generation carried in a snapshot file name (MANIFEST-{g},
/// CHECKPOINT-{g}.log, IDENTITY/REPLICA/PLACEMENT-{g}.log, SNAPSHOT-{g}).
fn snap_gen(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".log").unwrap_or(name);
    let digits = stem.rsplit_once('-')?.1;
    (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

fn spawn_checkpoint_child(dir: &Path, snap: &Path) -> Child {
    Command::new(std::env::current_exe().expect("current exe"))
        .arg("--exact")
        .arg("snp002_checkpoint_during_copy_kill_window")
        .env(CHILD_ENV, "1")
        .env(DIR_ENV, dir)
        .env(SNAP_ENV, snap)
        .env(PARK_ENV, "during_copy")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child")
}

#[test]
fn snp002_checkpoint_during_copy_kill_window() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let db = Arc::new(Db::open(Config::new(child_dir())).unwrap());
        // The checkpoint must land DURING the parked window — wait for the
        // park marker so it can never race ahead of the snapshot's capture.
        let park = child_snap_dir().join("during_copy");
        let t = {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                while !park.exists() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                db.checkpoint_now()
                    .expect("checkpoint during the parked copy");
            })
        };
        db.snapshot_to(&child_snap_dir()).unwrap();
        let _ = t.join();
        unreachable!("the parent kills the parked child");
    }
    let _serial = park_lock();
    let d = dir("snp002-live");
    let (db, expected) = seed(&d, 40);
    let g0 = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    drop(db); // the child needs the directory lock

    let snap = dir("snp002-snap");
    let mut child = spawn_checkpoint_child(&d, &snap);
    wait_for(&snap.join("during_copy"), Duration::from_secs(30));

    // The timing evidence: the checkpoint must publish while the snapshot
    // is parked (post-fix). Today it blocks on the state read lock.
    let deadline = Instant::now() + Duration::from_secs(5);
    let checkpoint_landed = loop {
        let landed = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("CHECKPOINT-"));
        if landed || Instant::now() >= deadline {
            break landed;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    child.kill().expect("kill child");
    child.wait().expect("wait child");

    assert!(
        checkpoint_landed,
        "the checkpoint blocked behind the snapshot's read lock — it must \
         land during the parked copy"
    );
    // The kill landed mid-copy: the snapshot dir is unmarked and inert,
    // and holds only files of the pinned generation — never a mix.
    let markers: Vec<String> = std::fs::read_dir(&snap)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("SNAPSHOT-"))
        .collect();
    assert!(markers.is_empty(), "a mid-copy kill leaves no valid marker");
    assert!(
        restore_from(&snap, dir("snp002-target")).is_err(),
        "an unmarked dir is never restorable"
    );
    for e in std::fs::read_dir(&snap).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(gen) = snap_gen(&name) {
            assert!(
                gen <= g0,
                "mixed generation: {name} carries {gen} > pinned {g0}"
            );
        }
    }
    // The live db: the checkpoint changed no rows and the prune skipped
    // the pinned files — zero loss across the reopen.
    let live = Db::open(Config::new(d)).unwrap();
    assert_eq!(walk(&live), expected, "the live db is untouched");
}

// ---------------------------------------------------------------------------
// snp003 — pinned-file prune guard
// ---------------------------------------------------------------------------

#[test]
fn snp003_concurrent_checkpoint_prune_skips_pinned_files() {
    let d = dir("snp003-live");
    let mut cfg = Config::new(d.clone());
    cfg.memtable_bytes = 512;
    cfg.l0_compact_trigger = 0;
    let db = Db::open(cfg).unwrap();
    // gen g0 with a SURVIVING identity log: put_object queues the identity
    // record, the flush publishes IDENTITY-g0.log, and no checkpoint
    // trigger fires, so nothing prunes it yet.
    db.put_object(ObjectId::from_bytes([0xA1; 16]), b"k", b"v")
        .unwrap();
    db.flush().unwrap();
    let g0 = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    assert!(
        d.join(format!("IDENTITY-{g0:06}.log")).exists(),
        "the baseline identity log survives its flush"
    );
    let before = walk(&db);
    let db = Arc::new(db);

    // Park the snapshot mid-copy (pins IDENTITY-g0); the interleave bumps
    // the generation and checkpoints — post-fix it lands DURING the parked
    // copy, and its prune must skip the pinned IDENTITY-g0 while still
    // subsuming the unpinned IDENTITY-g1 it just published.
    let snap = dir("snp003-snap");
    let (info, completed) = interleave_completed_while_parked(&db, &snap, {
        let db = Arc::clone(&db);
        move || {
            db.put_object(ObjectId::from_bytes([0xB2; 16]), b"k2", b"v2")
                .unwrap();
            db.flush().unwrap();
            db.checkpoint_now().unwrap();
        }
    });

    assert_eq!(
        info.generation, g0,
        "the snapshot pins the pre-checkpoint generation"
    );
    let g1 = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    assert!(g1 > g0, "the interleave bumped the generation");

    assert!(
        completed,
        "the interleaved flush+checkpoint must land during the parked copy"
    );
    assert!(
        d.join(format!("IDENTITY-{g0:06}.log")).exists(),
        "the prune must skip the pinned IDENTITY-g0 — the snapshot still needs it"
    );
    assert!(
        !d.join(format!("IDENTITY-{g1:06}.log")).exists(),
        "the prune still removes the unpinned IDENTITY-g1 it subsumes"
    );
    assert!(
        d.join(format!("CHECKPOINT-{g1:06}.log")).exists(),
        "the checkpoint published at g1"
    );
    let restored = restore_from(&snap, dir("snp003-target")).unwrap();
    assert_eq!(
        walk(&restored),
        before,
        "the pinned generation restores byte-exact"
    );
    let mut expected_live = before.clone();
    expected_live.insert(b"k2".to_vec(), b"v2".to_vec());
    assert_eq!(
        walk(&db),
        expected_live,
        "the checkpoint changed no rows beyond the put"
    );
}
