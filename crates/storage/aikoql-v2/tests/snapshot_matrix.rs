//! PR6-007 — the snapshot composed failure-injection matrix (review §9).
//! Eight rows: four interleaves (write/flush/checkpoint/compaction issued
//! while the snapshot is mid-copy — deterministic: each blocks on the state
//! lock the snapshot holds and can only land after it completes) and four
//! crash/corruption windows (crash mid-copy, crash after the marker commit,
//! torn WAL tail, corrupt copied file).
//!
//! One mechanism serves both kinds of row: AIKOQL_V2_SNAP_PARK "during_copy"
//! parks the copy loop after the first file, "after_marker" parks after the
//! marker publication. A kill-based row kills the parked child; an interleave
//! row releases the park by deleting the marker file.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::format::{Current, FormatError};
use aikoql_storage_v2::snapshot::{marker_path, restore_from, SnapshotInfo};
use common::dir;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PARK_ENV: &str = "AIKOQL_V2_SNAP_PARK";
const CHILD_ENV: &str = "SFM_CHILD";
const DIR_ENV: &str = "SFM_DIR";
const SNAP_ENV: &str = "SFM_SNAP_DIR";

fn walk(db: &Db) -> BTreeMap<Vec<u8>, Vec<u8>> {
    db.scan(b"").unwrap().into_iter().collect()
}

fn wait_for(path: &Path, timeout: Duration) {
    let start = std::time::Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < timeout,
            "marker {} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn segs_in(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(d)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("SEGMENT-"))
        .collect();
    v.sort();
    v
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
// rows 1–4 — the interleaves
// ---------------------------------------------------------------------------

/// The interleave rows arm AIKOQL_V2_SNAP_PARK in THIS process (env is
/// process-wide and the harness runs tests in parallel). EVERY row holds
/// this lock: rows 1–4 while their arm is live, rows 5–8 so a snapshot of
/// theirs can never run inside a sibling's armed window and park by
/// accident (the exact hang sfm007 caught). Cheap — the matrix is small.
static PARK_LOCK: Mutex<()> = Mutex::new(());

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

/// Park the snapshot mid-copy in one thread, run `op` in another (it blocks
/// on the state lock the snapshot holds), release the park, join both. The
/// op lands only after the snapshot has fully completed — deterministic, no
/// timing window.
fn interleave(db: &Arc<Db>, snap: &Path, op: impl FnOnce() + Send + 'static) -> SnapshotInfo {
    let _serial = PARK_LOCK.lock().unwrap();
    let _arm = ParkArm::new("during_copy");
    let snap_t = {
        let (db, snap) = (Arc::clone(db), snap.to_path_buf());
        std::thread::spawn(move || db.snapshot_to(&snap))
    };
    wait_for(&snap.join("during_copy"), Duration::from_secs(30));
    let op_t = std::thread::spawn(op);
    std::fs::remove_file(snap.join("during_copy")).expect("release the park");
    let info = snap_t.join().expect("snapshot thread").expect("snapshot");
    op_t.join().expect("op thread");
    info
}

/// row 1 — write during snapshot → the snapshot stays at the pinned
/// generation: the write is invisible to it and lands only after it.
#[test]
fn sfm001_write_during_snapshot_stays_at_pinned_generation() {
    let d = dir("sfm001-live");
    let (db, before) = seed(&d, 40);
    let db = Arc::new(db);
    let g0 = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;

    let snap = dir("sfm001-snap");
    let info = interleave(&db, &snap, {
        let db = Arc::clone(&db);
        move || {
            db.put(b"late", b"x").unwrap();
        }
    });

    assert_eq!(
        info.generation, g0,
        "the snapshot pins the pre-write generation"
    );
    assert!(
        marker_path(&snap, g0).exists(),
        "the marker is the commit point"
    );
    let restored = restore_from(&snap, dir("sfm001-target")).unwrap();
    assert_eq!(
        walk(&restored),
        before,
        "the write is invisible to the snapshot"
    );
    assert_eq!(
        db.get(b"late").unwrap(),
        Some(b"x".to_vec()),
        "the write landed after the snapshot"
    );
}

/// row 2 — flush during snapshot → the pinned generation remains complete:
/// the concurrent put+flush lands only after the snapshot, which restores
/// byte-exact at the pre-flush state.
#[test]
fn sfm002_flush_during_snapshot_pins_complete_generation() {
    let d = dir("sfm002-live");
    let (db, _) = seed(&d, 40);
    let db = Arc::new(db);
    for i in 40..48 {
        db.put(format!("k{i:03}").as_bytes(), format!("v{i}").as_bytes())
            .unwrap();
    }
    let g0 = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    let before = walk(&db);

    let snap = dir("sfm002-snap");
    let info = interleave(&db, &snap, {
        let db = Arc::clone(&db);
        move || {
            db.put(b"late", b"x").unwrap();
            db.flush().unwrap();
        }
    });

    assert_eq!(
        info.generation, g0,
        "the snapshot pins the pre-flush generation"
    );
    let restored = restore_from(&snap, dir("sfm002-target")).unwrap();
    assert_eq!(
        walk(&restored),
        before,
        "the pinned generation restores complete"
    );
    assert_eq!(db.get(b"late").unwrap(), Some(b"x".to_vec()));
    assert!(
        Current::read(&d.join("CURRENT"))
            .unwrap()
            .manifest_generation
            > g0,
        "the flush landed after the snapshot"
    );
}

/// A generation carried in a snapshot file name (MANIFEST-{g},
/// CHECKPOINT-{g}.log, IDENTITY/REPLICA/PLACEMENT-{g}.log, SNAPSHOT-{g}).
/// Segment ids are NOT generations and never parse here — their names don't
/// end in six digits.
fn snap_gen(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".log").unwrap_or(name);
    let digits = stem.rsplit_once('-')?.1;
    (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// row 3 — write+flush during snapshot → no mixed generations: the snapshot
/// carries only files of its pinned generation. (The name says "checkpoint";
/// the actual checkpoint-during-snapshot interleave is the row-3b test right
/// below — kept separate per the Round-2 review.)
#[test]
fn sfm003_checkpoint_during_snapshot_no_mixed_generations() {
    let d = dir("sfm003-live");
    let (db, _) = seed(&d, 40);
    let db = Arc::new(db);
    for i in 40..48 {
        db.put(format!("k{i:03}").as_bytes(), format!("v{i}").as_bytes())
            .unwrap();
    }
    let g0 = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    let before = walk(&db);

    let snap = dir("sfm003-snap");
    let info = interleave(&db, &snap, {
        let db = Arc::clone(&db);
        move || {
            db.put(b"late", b"x").unwrap();
            db.flush().unwrap();
        }
    });

    assert_eq!(info.generation, g0);
    for e in std::fs::read_dir(&snap).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(gen) = snap_gen(&name) {
            assert!(
                gen <= g0,
                "mixed generation: {name} carries {gen} > pinned {g0}"
            );
        }
    }
    let restored = restore_from(&snap, dir("sfm003-target")).unwrap();
    assert_eq!(walk(&restored), before);
    assert!(
        Current::read(&d.join("CURRENT"))
            .unwrap()
            .manifest_generation
            > g0
    );
}

/// row 3b — CHECKPOINT during snapshot (PR6-R2-004): a real
/// `checkpoint_now()` — publish CHECKPOINT-{g}, prune older deltas — waits
/// on the state lock the snapshot copy loop holds, so it can only land
/// after the snapshot completed. The dangerous interaction the original
/// review named (checkpoint prunes while the snapshot still needs ≤g
/// files) is structurally impossible: pinned here, not assumed.
#[test]
fn sfm003_checkpoint_during_snapshot_preserves_pinned_generation() {
    let d = dir("sfm003b-live");
    let (db, _) = seed(&d, 40);
    let db = Arc::new(db);
    for i in 40..48 {
        db.put(format!("k{i:03}").as_bytes(), format!("v{i}").as_bytes())
            .unwrap();
    }
    let g0 = Current::read(&d.join("CURRENT"))
        .unwrap()
        .manifest_generation;
    let before = walk(&db);
    let ckps_in = |p: &Path| -> usize {
        std::fs::read_dir(p)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("CHECKPOINT-"))
            .count()
    };
    assert_eq!(ckps_in(&d), 0, "no checkpoint has been published yet");

    let snap = dir("sfm003b-snap");
    let info = interleave(&db, &snap, {
        let db = Arc::clone(&db);
        move || {
            db.checkpoint_now().unwrap();
        }
    });

    assert_eq!(
        info.generation, g0,
        "the snapshot pins the pre-checkpoint generation"
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
    let restored = restore_from(&snap, dir("sfm003b-target")).unwrap();
    assert_eq!(
        walk(&restored),
        before,
        "the pinned generation restores exact at the pre-checkpoint state"
    );
    assert!(
        ckps_in(&d) > 0,
        "the checkpoint landed (published) after the snapshot"
    );
    assert_eq!(walk(&db), before, "the checkpoint changed no state");
}

/// row 4 — compaction during snapshot → the referenced segments remain
/// available: the interleaved compact merges and deletes the LIVE segments
/// only after the snapshot completed; its copies still restore byte-exact.
#[test]
fn sfm004_compaction_during_snapshot_referenced_segments_remain_available() {
    let d = dir("sfm004-live");
    let (db, before) = seed(&d, 200);
    let db = Arc::new(db);
    let live_segs_before = segs_in(&d).len();
    assert!(
        live_segs_before > 1,
        "the baseline must span several segments"
    );

    let snap = dir("sfm004-snap");
    let _info = interleave(&db, &snap, {
        let db = Arc::clone(&db);
        move || {
            db.compact().unwrap();
        }
    });

    assert!(
        segs_in(&d).len() < live_segs_before,
        "the interleaved compaction pruned the live segments"
    );
    let restored = restore_from(&snap, dir("sfm004-target")).unwrap();
    assert_eq!(
        walk(&restored),
        before,
        "referenced segments remain available"
    );
    assert_eq!(walk(&db), before, "the live db converged to the same state");
}

// ---------------------------------------------------------------------------
// rows 5–6 — the kill-based crash windows (child process, bkp003 pattern)
// ---------------------------------------------------------------------------

fn child_dir() -> PathBuf {
    PathBuf::from(std::env::var(DIR_ENV).expect("child dir env"))
}

fn child_snap_dir() -> PathBuf {
    PathBuf::from(std::env::var(SNAP_ENV).expect("child snap dir env"))
}

fn spawn_child(test_name: &str, dir: &Path, snap: &Path, stage: &str) -> Child {
    Command::new(std::env::current_exe().expect("current exe"))
        .arg("--exact")
        .arg(test_name)
        .env(CHILD_ENV, "1")
        .env(DIR_ENV, dir)
        .env(SNAP_ENV, snap)
        .env(PARK_ENV, stage)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child")
}

/// row 5 — crash during file copy → no valid snapshot marker: the marker is
/// published last, so a mid-copy kill leaves an unmarked (unrestorable) dir
/// and the live db untouched.
#[test]
fn sfm005_crash_during_copy_leaves_no_valid_marker() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let db = Db::open(Config::new(child_dir())).unwrap();
        db.snapshot_to(&child_snap_dir()).unwrap();
        unreachable!("the parent kills the parked child");
    }
    let _serial = PARK_LOCK.lock().unwrap();
    let d = dir("sfm005-live");
    let (db, expected) = seed(&d, 40);
    drop(db); // the child needs the directory lock

    let snap = dir("sfm005-snap");
    let mut child = spawn_child(
        "sfm005_crash_during_copy_leaves_no_valid_marker",
        &d,
        &snap,
        "during_copy",
    );
    wait_for(&snap.join("during_copy"), Duration::from_secs(30));
    child.kill().expect("kill child");
    child.wait().expect("wait child");

    assert!(snap.join("CURRENT").exists(), "the kill landed mid-copy");
    let markers: Vec<String> = std::fs::read_dir(&snap)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("SNAPSHOT-"))
        .collect();
    assert!(
        markers.is_empty(),
        "a mid-copy kill must leave no valid marker"
    );
    let target = dir("sfm005-target");
    assert!(
        restore_from(&snap, target.clone()).is_err(),
        "an unmarked dir is never restorable"
    );
    assert!(!target.join("CURRENT").exists(), "no partial restore");

    let live = Db::open(Config::new(d)).unwrap();
    assert_eq!(walk(&live), expected, "the live db is untouched");
}

/// row 6 — crash after marker → restore succeeds: the marker is the commit
/// point; a kill after it leaves a fully restorable snapshot.
#[test]
fn sfm006_crash_after_marker_restores() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let db = Db::open(Config::new(child_dir())).unwrap();
        db.snapshot_to(&child_snap_dir()).unwrap();
        unreachable!("the parent kills the parked child");
    }
    let _serial = PARK_LOCK.lock().unwrap();
    let d = dir("sfm006-live");
    let (db, expected) = seed(&d, 40);
    drop(db);

    let snap = dir("sfm006-snap");
    let mut child = spawn_child(
        "sfm006_crash_after_marker_restores",
        &d,
        &snap,
        "after_marker",
    );
    wait_for(&snap.join("after_marker"), Duration::from_secs(30));
    child.kill().expect("kill child");
    child.wait().expect("wait child");

    let restored = restore_from(&snap, dir("sfm006-target")).unwrap();
    assert_eq!(
        walk(&restored),
        expected,
        "a committed marker survives the crash"
    );

    let live = Db::open(Config::new(d)).unwrap();
    assert_eq!(walk(&live), expected, "the live db is untouched");
}

// ---------------------------------------------------------------------------
// rows 7–8 — deterministic corruption windows (no crash harness needed)
// ---------------------------------------------------------------------------

/// row 7 — torn WAL tail → acknowledged state preserved: garbage appended
/// past the acked frames (the exact bytes a crash mid-append leaves) never
/// rides into the snapshot — only the torn-safe prefix is copied.
#[test]
fn sfm007_torn_wal_tail_preserves_acknowledged_state() {
    let _serial = PARK_LOCK.lock().unwrap();
    let d = dir("sfm007-live");
    let (db, expected) = seed(&d, 40);
    let wal = std::fs::read_dir(&d)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("WAL-"))
        .expect("the WAL file");
    // The flush in seed() drained the committer, so nothing can append
    // between this garbage and the snapshot's locked WAL read.
    let mut f = std::fs::OpenOptions::new().append(true).open(&wal).unwrap();
    std::io::Write::write_all(&mut f, &[0xAB; 16]).unwrap();
    drop(f);

    let snap = dir("sfm007-snap");
    db.snapshot_to(&snap).unwrap();
    let restored = restore_from(&snap, dir("sfm007-target")).unwrap();
    assert_eq!(walk(&restored), expected, "the torn tail never rides along");
    let snap_wal = std::fs::metadata(snap.join(wal.file_name().unwrap()))
        .unwrap()
        .len();
    let live_wal = std::fs::metadata(&wal).unwrap().len();
    assert!(
        snap_wal < live_wal,
        "the snapshot carries only the torn-safe prefix ({snap_wal} of {live_wal} B)"
    );
}

/// row 8 — corrupt copied file → restore fails closed (the matrix's own
/// re-pin of the bkp004 property through the matrix machinery).
#[test]
fn sfm008_corrupt_copied_file_restore_fails_closed() {
    let _serial = PARK_LOCK.lock().unwrap();
    let d = dir("sfm008-live");
    let db = Db::open(Config::new(d)).unwrap();
    for i in 0..30u64 {
        db.put(format!("k{i:03}").as_bytes(), b"v").unwrap();
    }
    db.flush().unwrap();

    let snap = dir("sfm008-snap");
    db.snapshot_to(&snap).unwrap();
    let seg = segs_in(&snap)
        .into_iter()
        .next()
        .expect("a segment was copied");
    let p = snap.join(seg);
    let mut b = std::fs::read(&p).unwrap();
    let mid = b.len() / 2;
    b[mid] ^= 0x01;
    std::fs::write(&p, b).unwrap();

    let target = dir("sfm008-target");
    assert!(
        matches!(
            restore_from(&snap, target.clone()),
            Err(FormatError::Corrupt(_))
        ),
        "a corrupted copied file must fail closed"
    );
    assert!(!target.join("CURRENT").exists(), "no partial restore");
}
