//! P3-M3 — engine-native snapshot/restore (§58–60, docs/IMPLEMENTATION-PLAN-PHASE3.md).
//!
//! The snapshot marker (bkp006 pins its bytes — python-computed FIRST, before
//! the Rust writer existed, per testing-plan rule 4):
//!
//! `AKSN | format_version u16 LE | generation u64 LE | file_count u32 LE |
//!  per file, sorted by name: name_len u32 LE | name | file_size u64 LE |
//!  sha256-8 of the file content | sha256-8 over everything before it`
//!
//! The marker is the commit point: `snapshot_to` copies the pinned generation
//! (CURRENT, MANIFEST-{gen}, its segments, the identity/replica/placement logs
//! and checkpoints ≤ gen, and the torn-safe WAL prefix), verifies every copied
//! byte, then publishes the marker last. A dir without a valid marker is an
//! incomplete snapshot and `restore_from` refuses it — the crash windows
//! (bkp003) can therefore never leave a partial snapshot visible.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::format::{checksum8, Current, FormatError};
use aikoql_storage_v2::snapshot::{
    marker_path, restore_from, SnapshotFile, SnapshotInfo, SnapshotMarker,
};
use aikoql_storage_v2::wal::Op;
use common::{dir, hex, report_write};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn walk(db: &Db) -> BTreeMap<Vec<u8>, Vec<u8>> {
    db.scan(b"").unwrap().into_iter().collect()
}

/// bkp001 — snapshot → restore into a fresh dir → the full key-value walk of
/// the restored db byte-equals the live db (the oracle). The snapshot dir is
/// never consumed: the same snapshot restores again into a second fresh dir.
#[test]
fn bkp001_snapshot_restore_roundtrip_oracle() {
    let d = dir("bkp001-live");
    let mut cfg = Config::new(d.clone());
    cfg.memtable_bytes = 1024; // force real flushes and log generations
    cfg.l0_compact_trigger = 0;
    let db = Db::open(cfg).unwrap();
    for i in 0..200u64 {
        let k = format!("key{i:04}").into_bytes();
        db.put(&k, format!("value-{i}-{}", i * 31).as_bytes())
            .unwrap();
        if i % 25 == 24 {
            db.flush().unwrap();
        }
    }
    // Deletes and updates across generations — tombstones must survive.
    for i in (0..200u64).step_by(7) {
        let k = format!("key{i:04}").into_bytes();
        if i % 14 == 0 {
            db.delete(&k).unwrap();
        } else {
            db.put(&k, b"updated").unwrap();
        }
    }
    db.flush().unwrap();
    db.compact().unwrap(); // L1 output + relocation records ride along
    db.put(b"tail", b"after-compact").unwrap();
    let live = walk(&db);

    let snap = dir("bkp001-snap");
    let info = db.snapshot_to(&snap).unwrap();
    let current = Current::read(&d.join("CURRENT")).unwrap();
    assert_eq!(
        info.generation, current.manifest_generation,
        "the snapshot pins the CURRENT generation"
    );
    assert!(
        marker_path(&snap, info.generation).exists(),
        "the marker is the commit point"
    );

    let target = dir("bkp001-target");
    let restored = restore_from(&snap, target.clone()).unwrap();
    assert_eq!(
        walk(&restored),
        live,
        "restored walk diverged from the live db"
    );

    let target2 = dir("bkp001-target2");
    let restored2 = restore_from(&snap, target2).unwrap();
    assert_eq!(
        walk(&restored2),
        live,
        "the snapshot restores a second time"
    );

    // A non-empty target is refused, not clobbered.
    drop(restored);
    std::fs::write(target.join("junk"), b"x").unwrap();
    assert!(
        restore_from(&snap, target).is_err(),
        "restore must refuse a non-empty target"
    );

    // The live db keeps serving after the snapshot.
    db.put(b"more", b"x").unwrap();
    assert_eq!(db.get(b"more").unwrap(), Some(b"x".to_vec()));
}

/// bkp002 — concurrent-writer stress: snapshots land while a writer thread is
/// mid-run; every restored snapshot must be an exact dense prefix of the
/// committed batches (keys k{000000..n}, values exact) — a consistent pinned
/// generation, never a mix of two.
#[test]
fn bkp002_concurrent_writer_snapshots_pin_consistent_generations() {
    let d = dir("bkp002-live");
    let mut cfg = Config::new(d.clone());
    cfg.memtable_bytes = 4096;
    cfg.l0_compact_trigger = 0;
    let db = Arc::new(Db::open(cfg).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicU64::new(0));
    let writer = {
        let (db, stop, done) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&done));
        std::thread::spawn(move || {
            let mut b = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let k = format!("k{b:06}").into_bytes();
                let v = format!("v{b:06}-{}", b * 2654435761).into_bytes();
                db.put(&k, &v).unwrap();
                b += 1;
                done.store(b, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(1)); // keep the race window open
            }
        })
    };
    let snaps: Vec<PathBuf> = (0..8)
        .map(|i| {
            let s = dir(&format!("bkp002-snap-{i}"));
            db.snapshot_to(&s).unwrap();
            s
        })
        .collect();
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    let total = done.load(Ordering::Relaxed);
    assert!(
        total > 8,
        "the writer must still be mid-run during the snapshots"
    );

    for (i, s) in snaps.iter().enumerate() {
        let target = dir(&format!("bkp002-target-{i}"));
        let restored = restore_from(s, target).unwrap();
        let rows = walk(&restored);
        let n = rows.len() as u64;
        assert!(n <= total, "restored {n} rows, writer reached {total}");
        for b in 0..n {
            let k = format!("k{b:06}").into_bytes();
            let want = format!("v{b:06}-{}", b * 2654435761).into_bytes();
            assert_eq!(
                rows.get(&k),
                Some(&want),
                "batch {b} diverged in the restore"
            );
        }
        // rows.len() == n and every 0..n key matches: an exact dense prefix.
    }
}

// --- bkp003 child-kill harness (the KSE-15 pattern, cf. compact_crash.rs) ---

const CHILD_ENV: &str = "AIKOQL_V2_KILL_CHILD";
const DIR_ENV: &str = "AIKOQL_V2_KILL_DIR";
const SNAP_ENV: &str = "AIKOQL_V2_KILL_SNAP_DIR";
const SNAP_PARK_ENV: &str = "AIKOQL_V2_SNAP_PARK";
const PLACE_PARK_ENV: &str = "AIKOQL_V2_PLACE_PARK";

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
        .env(SNAP_PARK_ENV, stage)
        .env(PLACE_PARK_ENV, stage)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child")
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

/// xorshift64 — parent and child derive the same deterministic workload.
fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut s = seed;
    move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    }
}

fn ops() -> Vec<(bool, Vec<u8>, Vec<u8>)> {
    let mut next = rng(7);
    (0..120)
        .map(|i| {
            let k = format!("k{:03}", next() % 40).into_bytes();
            match next() % 10 {
                0..=6 => (true, k, format!("v{i:03}").into_bytes()),
                _ => (false, k, Vec::new()),
            }
        })
        .collect()
}

fn expected() -> BTreeMap<Vec<u8>, Option<Vec<u8>>> {
    ops()
        .into_iter()
        .map(|(p, k, v)| (k, if p { Some(v) } else { None }))
        .collect()
}

/// The live db survives the kill at every window — same oracle as the
/// compaction crash matrix (all values, no phantoms, seq resumes at 121).
fn verify_live(d: &Path) {
    let db = Db::open(Config::new(d.to_path_buf())).unwrap();
    for (k, want) in expected() {
        assert_eq!(
            db.get(&k).unwrap(),
            want,
            "key {:?} diverged after the kill",
            String::from_utf8_lossy(&k)
        );
    }
    for probe in 0..30u64 {
        let k = format!("z{probe:03}").into_bytes();
        assert_eq!(db.get(&k).unwrap(), None, "phantom key {probe}");
    }
    assert_eq!(db.put(b"next", b"x").unwrap(), 121, "no acked write lost");
}

/// bkp003 — child-kill at each snapshot window. The four windows are the
/// protocol's stages: after the copy, after the verify, and the two marker
/// publication boundaries (write / fsync). In every window the marker has
/// not committed, so no partial snapshot is visible: `restore_from` refuses
/// the dir — and the live db is untouched.
#[test]
fn bkp003_snapshot_crash_windows_never_leave_partial_visible() {
    const STAGES: [&str; 4] = [
        "after_copy",
        "after_verify",
        "FAIL_AFTER_SNAPSHOT_WRITE",
        "FAIL_AFTER_SNAPSHOT_FSYNC",
    ];
    if std::env::var_os(CHILD_ENV).is_some() {
        // The child reopens the seeded db and snapshots until it parks —
        // the parent kills it at the env-named stage.
        let db = Db::open(Config::new(child_dir())).unwrap();
        db.snapshot_to(&child_snap_dir()).unwrap();
        unreachable!("the parent kills the parked child");
    }
    for (i, stage) in STAGES.iter().enumerate() {
        let d = dir(&format!("bkp003-live-{i}"));
        let mut cfg = Config::new(d.clone());
        cfg.memtable_bytes = 512;
        cfg.l0_compact_trigger = 0;
        let db = Db::open(cfg).unwrap();
        for (put, k, v) in ops() {
            if put {
                db.put(&k, &v).unwrap();
            } else {
                db.delete(&k).unwrap();
            }
        }
        db.flush().unwrap();
        drop(db); // the child needs the directory lock

        let snap = dir(&format!("bkp003-snap-{i}"));
        let mut child = spawn_child(
            "bkp003_snapshot_crash_windows_never_leave_partial_visible",
            &d,
            &snap,
            stage,
        );
        wait_for(&snap.join(stage), Duration::from_secs(60));
        child.kill().expect("kill child");
        child.wait().expect("wait child");

        verify_live(&d);
        let target = dir(&format!("bkp003-target-{i}"));
        assert!(
            restore_from(&snap, target.clone()).is_err(),
            "window {stage}: a killed snapshot must never be restorable"
        );
        assert!(
            !target.join("CURRENT").exists(),
            "window {stage}: restore must leave no partial state"
        );
    }
}

/// bkp004 — byte-flip in a copied file → restore fails closed (no partial
/// restore, no silent degradation). Covers a segment, the marker itself,
/// and CURRENT.
#[test]
fn bkp004_damaged_snapshot_fails_closed() {
    fn flip(dir: &Path, name: &str) {
        let p = dir.join(name);
        let mut b = std::fs::read(&p).unwrap();
        assert!(!b.is_empty(), "{name} is empty");
        let mid = b.len() / 2;
        b[mid] ^= 0x01;
        std::fs::write(&p, b).unwrap();
    }
    fn assert_fails_closed(snap: &Path, target: &Path, what: &str) {
        let err = match restore_from(snap, target.to_path_buf()) {
            Err(e) => e,
            Ok(_) => panic!("{what}: restore of a damaged snapshot must fail"),
        };
        assert!(
            matches!(err, FormatError::Corrupt(_)),
            "{what}: expected fail-closed Corrupt, got {err}"
        );
        assert!(
            !target.join("CURRENT").exists(),
            "{what}: restore must leave no partial state"
        );
    }

    let d = dir("bkp004-live");
    let db = Db::open(Config::new(d.clone())).unwrap();
    for i in 0..30u64 {
        db.put(format!("k{i:03}").as_bytes(), b"v").unwrap();
    }
    db.flush().unwrap();

    // (a) a copied segment
    let snap = dir("bkp004-snap-seg");
    db.snapshot_to(&snap).unwrap();
    let seg = std::fs::read_dir(&snap)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("SEGMENT-"))
        .expect("the snapshot carries a segment");
    flip(&snap, &seg);
    assert_fails_closed(&snap, &dir("bkp004-target-seg"), "flipped segment");

    // (b) the marker
    let snap = dir("bkp004-snap-marker");
    db.snapshot_to(&snap).unwrap();
    let m = std::fs::read_dir(&snap)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("SNAPSHOT-"))
        .expect("the snapshot carries a marker");
    flip(&snap, &m);
    assert_fails_closed(&snap, &dir("bkp004-target-marker"), "flipped marker");

    // (c) CURRENT
    let snap = dir("bkp004-snap-current");
    db.snapshot_to(&snap).unwrap();
    flip(&snap, "CURRENT");
    assert_fails_closed(&snap, &dir("bkp004-target-current"), "flipped CURRENT");
}

/// bkp006 — the marker golden bytes. Computed in python BEFORE the Rust
/// writer existed (testing-plan rule 4), over a fixed synthetic manifest:
/// six files, deterministic zero-padded contents, sorted names. A change in
/// this hex is a format break — a visible diff.
#[test]
fn bkp006_marker_golden_bytes() {
    let files = [
        ("CHECKPOINT-000004.log", 240u64),
        ("CURRENT", 22),
        ("IDENTITY-000005.log", 128),
        ("MANIFEST-000007", 57),
        ("SEGMENT-000003.seg", 4096),
        ("WAL-000001.log", 512),
    ];
    let marker = SnapshotMarker {
        format_version: 1,
        generation: 7,
        files: files
            .iter()
            .map(|(name, size)| {
                let mut c = name.as_bytes().to_vec();
                c.resize(*size as usize, 0);
                SnapshotFile {
                    name: name.to_string(),
                    size: *size,
                    checksum: checksum8(&c),
                }
            })
            .collect(),
    };
    assert_eq!(
        hex(&marker.encode()),
        "414b534e010007000000000000000600000015000000434845434b504f494e542d3030303030342e6c6f67\
         f000000000000000cd740f746e89b9720700000043555252454e5416000000000000008880ea4fd6cc96d7\
         130000004944454e544954592d3030303030352e6c6f67800000000000000098cf5ee521ee4e690f000000\
         4d414e49464553542d30303030303739000000000000002b0c896458933c31120000005345474d454e542d\
         3030303030332e7365670010000000000000baa2c5609b8035250e00000057414c2d3030303030312e6c6f67\
         00020000000000004d4cfcd717d07cfa6453a46db9cced68",
        "snapshot marker golden bytes changed — format break"
    );
}

#[test]
fn bkp006_marker_round_trip_and_fail_closed() {
    let marker = SnapshotMarker {
        format_version: 1,
        generation: 3,
        files: vec![
            SnapshotFile {
                name: "CURRENT".into(),
                size: 22,
                checksum: checksum8(b"x"),
            },
            SnapshotFile {
                name: "WAL-000001.log".into(),
                size: 9,
                checksum: checksum8(b"y"),
            },
        ],
    };
    let decoded = SnapshotMarker::decode(&marker.encode()).unwrap();
    assert_eq!(decoded.generation, 3);
    assert_eq!(decoded.files.len(), 2);
    assert_eq!(decoded.files[0].name, "CURRENT");
    assert_eq!(decoded.files[1].name, "WAL-000001.log");

    // Checksum mismatch fails closed.
    let mut bytes = marker.encode();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    assert!(matches!(
        SnapshotMarker::decode(&bytes),
        Err(FormatError::Corrupt(_))
    ));

    // Truncation fails closed.
    let bytes = marker.encode();
    assert!(matches!(
        SnapshotMarker::decode(&bytes[..bytes.len() - 3]),
        Err(FormatError::Corrupt(_))
    ));

    // An unknown-but-clean version is Unsupported, never trusted.
    let mut newer = marker;
    newer.format_version = 2;
    assert!(matches!(
        SnapshotMarker::decode(&newer.encode()),
        Err(FormatError::Unsupported(_))
    ));
}

/// The 100K copy-time cell (P3M3_SNAP_CELL=1, report-only): the snapshot of a
/// 100K-key db must be O(disk) file copies — measured, never asserted — and
/// the restored db holds all 100K rows (the correctness sanity).
#[test]
fn p3m3_snapshot_copy_time_cell() {
    if std::env::var("P3M3_SNAP_CELL").as_deref() != Ok("1") {
        return;
    }
    let d = dir("p3m3-cell-live");
    let mut cfg = Config::new(d.clone());
    cfg.memtable_bytes = 1 << 20; // ~100 flushes → real segment bytes to copy
    cfg.l0_compact_trigger = 0;
    let db = Db::open(cfg).unwrap();
    let mut batch = Vec::with_capacity(64);
    for i in 0..100_000u64 {
        let k = format!("key{i:06}").into_bytes();
        let v = format!("value-{i}").into_bytes();
        batch.push(Op::Put(k, v));
        if batch.len() == 64 {
            db.write(&batch).unwrap();
            batch.clear();
        }
    }
    if !batch.is_empty() {
        db.write(&batch).unwrap();
    }
    let snap = dir("p3m3-cell-snap");
    let t = Instant::now();
    let info = db.snapshot_to(&snap).unwrap();
    let copy_s = t.elapsed().as_secs_f64();

    let target = dir("p3m3-cell-target");
    let restored = restore_from(&snap, target).unwrap();
    assert_eq!(
        walk(&restored).len(),
        100_000,
        "the restored db holds all rows"
    );

    let report = "E:/dreams/Mnemosyne/artifacts/storage-engine-v2/snapshot-copy-time.md";
    let _ = SnapshotInfo {
        generation: info.generation,
        file_count: info.file_count,
        bytes_copied: info.bytes_copied,
    };
    let body = format!(
        "# Snapshot Copy Time — P3-M3 (100K cell)\n\n\
         Generated only when `P3M3_SNAP_CELL=1`. Report cells, never asserts.\n\n\
         - keys: 100000 · batches of 64 · memtable 1 MiB · l0 trigger off\n\
         - snapshot generation: {} · files: {} · bytes copied: {}\n\
         - copy wall: {copy_s:.3} s\n",
        info.generation, info.file_count, info.bytes_copied,
    );
    report_write(std::path::Path::new(report), body);
}
