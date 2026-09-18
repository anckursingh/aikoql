//! P5-M27 — IDX-P0-01 / IDX-P1-01 (PR6 P0-01 / P1-01): the checkpoint
//! crash matrix and the concurrent-verify rule.
//!
//! The maintainer checkpoint publishes through one fail-closed funnel
//! (water captured first → vectors → text → each property index → water.txt
//! → COMPLETE → remove old dir → rename). These tests freeze the funnel at
//! a named stage via the CHECKPOINT_PARK_* env hooks — armed in a CHILD
//! process (env hooks are process-global, and the killed child's persistent
//! aikoql-v2 journal survives the kill) — then either kill the child inside
//! the window (021/022) or let its writer thread release the park (023) and
//! restart from the journal / the checkpoint, pinning contents.
//!
//! RED: the checkpoint_crash_child harness bin does not exist yet.
//!
//! IDX-P1-01 pins the consistency rule explicitly (030): verify() is exact
//! AT CALL START — a clean report (missing and stale both empty) covers the
//! rows committed before the check's snapshot; rows committed after belong
//! to the next report. A clean report observed after a commit must find
//! that row.

use aikoql_kernel::transaction::kernel::{KnowledgeContext, RememberRequest, Subject};
use aikoql_kernel::*;
use aikoql_scheduler::{IndexMaintainer, SchedulerJob};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

// --- helpers ------------------------------------------------------------------

fn note(k: &Kernel, body: &str, tag: &str) -> KOID {
    let mut req = RememberRequest::create(
        KnowledgeContext::new(Subject::new("crash-test")),
        Metadata {
            type_name: "note".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("body".into(), Value::Text(body.into()));
    req.properties
        .insert("tag".into(), Value::Text(tag.into()));
    k.remember(req).unwrap().koid
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "idx-crash-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Spawn the harness: a persistent v2 db at `tmp/db`, checkpoint dir
/// `tmp/ckpt`, parked at `stage`; `release` lets the harness's writer
/// thread release the park so the funnel completes (`ckpt.done`).
fn spawn_crash_child(tmp: &std::path::Path, stage: &str, release: bool) -> std::process::Child {
    let mut cmd = std::process::Command::new(crash_child());
    cmd.arg(tmp.join("db")).arg(tmp.join("ckpt")).arg(stage);
    if release {
        cmd.arg("release");
    }
    cmd.spawn()
        .unwrap_or_else(|e| panic!("spawn checkpoint_crash_child for {stage}: {e}"))
}

fn crash_child() -> &'static str {
    env!("CARGO_BIN_EXE_checkpoint_crash_child")
}

fn wait_for_file(p: &std::path::Path, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !p.exists() {
        if std::time::Instant::now() >= deadline {
            panic!("{what} never appeared ({})", p.display());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_ack_lines(p: &std::path::Path, n: usize, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let lines = std::fs::read_to_string(p)
            .map(|s| s.lines().count())
            .unwrap_or(0);
        if lines >= n {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!("{what}: expected {n} park-ack lines, saw {lines}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Reopen the SAME persistent db and start a fresh maintainer — from the
/// checkpoint when COMPLETE exists (the production restart), else from the
/// journal alone.
fn restart(db: &std::path::Path, ckpt: Option<&std::path::Path>) -> (Kernel, Arc<IndexMaintainer>) {
    let engine = Arc::new(
        aikoql_storage_v2::AikoqlStorageEngineV2::open(db.to_str().unwrap()).unwrap(),
    );
    let k = Kernel::open(engine, Arc::new(SystemClock), 0x5EED).unwrap();
    let v: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
    let t: Arc<dyn TextIndex> = Arc::new(TokenTextIndex::new());
    let water = ckpt.and_then(|c| IndexMaintainer::checkpoint_water(c).unwrap());
    let m = match water {
        Some(w) => IndexMaintainer::start_at(&k, v, t, Some(w), ckpt).unwrap(),
        None => IndexMaintainer::start_at(&k, v, t, None, None).unwrap(),
    };
    m.wait_caught_up(&k, Duration::from_secs(30)).unwrap();
    (k, m)
}

/// The seeded contents (10 seeds + the window commit) in both property
/// indexes — the crash-matrix completion oracle.
fn assert_complete_contents(k: &Kernel) {
    assert_eq!(
        k.scan_index("by_body", &[Value::Text("late-into-the-window".into())])
            .unwrap()
            .len(),
        1,
        "the window commit is in by_body"
    );
    assert_eq!(
        k.scan_index("by_tag", &[Value::Text("group-b".into())])
            .unwrap()
            .len(),
        1,
        "the window commit is in by_tag"
    );
    assert_eq!(
        k.scan_index("by_tag", &[Value::Text("group-a".into())])
            .unwrap()
            .len(),
        10,
        "all seed rows are in by_tag"
    );
}

// --- IDX-TDD-021 — kill during the vector checkpoint --------------------------

/// A commit lands while the funnel is between the water capture and the
/// text checkpoint, and the process dies there. The half-written tmp is
/// never published: restart fails closed to the full journal replay, and
/// the window commit is not lost.
#[test]
fn idx_tdd_021_kill_during_vector_checkpoint_restarts_to_full_replay() {
    let d = tmpdir("021");
    let mut child = spawn_crash_child(&d, "vectors", false);
    wait_for_ack_lines(&d.join("ckpt.ack"), 1, "021 vectors park");
    wait_for_file(&d.join("ckpt.late-committed"), "the window commit");
    child.kill().unwrap();
    child.wait().unwrap();

    assert_eq!(
        IndexMaintainer::checkpoint_water(&d.join("ckpt")).unwrap(),
        None,
        "a checkpoint killed mid-funnel publishes nothing"
    );
    let (k, m) = restart(&d.join("db"), Some(&d.join("ckpt")));
    assert_complete_contents(&k);
    m.shutdown();
    drop(k);
    let _ = std::fs::remove_dir_all(&d);
}

// --- IDX-TDD-022 — kill after every checkpoint stage --------------------------

/// Every kill window restarts fail-closed to a complete index: nothing is
/// published without the COMPLETE gate, and the journal replay covers the
/// rest. The publication window itself (a previous checkpoint removed, the
/// rename not yet done) is pinned separately: the operator's directory is
/// gone, the COMPLETE tmp beside it is never read, and the next checkpoint
/// sweeps it.
#[test]
fn idx_tdd_022_kill_after_every_checkpoint_stage_restarts_fail_closed() {
    for stage in ["water", "vectors", "text", "properties", "finalize"] {
        let d = tmpdir(&format!("022-{stage}"));
        let mut child = spawn_crash_child(&d, stage, false);
        wait_for_ack_lines(&d.join("ckpt.ack"), 1, &format!("022 {stage} park"));
        wait_for_file(&d.join("ckpt.late-committed"), "the window commit");
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(
            IndexMaintainer::checkpoint_water(&d.join("ckpt")).unwrap(),
            None,
            "killed at {stage}: no checkpoint is published"
        );
        let (k, m) = restart(&d.join("db"), Some(&d.join("ckpt")));
        assert_complete_contents(&k);
        m.shutdown();
        drop(k);
        let _ = std::fs::remove_dir_all(&d);
    }

    // The publication window: checkpoint #1 completes (the test releases
    // its park), checkpoint #2 is killed between the removal and the rename.
    let d = tmpdir("022-publish");
    let mut child = spawn_crash_child(&d, "publish", false);
    wait_for_ack_lines(&d.join("ckpt.ack"), 1, "022 publish first park");
    std::fs::write(d.join("ckpt.release"), b"1").unwrap();
    wait_for_ack_lines(&d.join("ckpt.ack"), 2, "022 publish second park");
    wait_for_file(&d.join("ckpt.late-committed"), "the window commit");
    child.kill().unwrap();
    child.wait().unwrap();

    assert!(
        !d.join("ckpt").exists(),
        "the old checkpoint was removed before the rename"
    );
    assert_eq!(
        IndexMaintainer::checkpoint_water(&d.join("ckpt")).unwrap(),
        None,
        "the COMPLETE tmp beside a removed dir is never read"
    );
    let (k, m) = restart(&d.join("db"), Some(&d.join("ckpt")));
    assert_complete_contents(&k);
    // The stale .tmp (COMPLETE included) must not block the next publication.
    m.checkpoint(&k, &d.join("ckpt")).unwrap();
    assert!(
        IndexMaintainer::checkpoint_water(&d.join("ckpt"))
            .unwrap()
            .is_some(),
        "the next checkpoint sweeps the stale tmp and publishes"
    );
    m.shutdown();
    drop(k);
    let _ = std::fs::remove_dir_all(&d);
}

// --- IDX-TDD-023 — commit between two property-index checkpoints --------------

/// The harness parks between by_body and by_tag; the window commit lands
/// there and the park is released. The restart from the checkpoint must
/// heal the index whose file predates the commit via the tail replay, and
/// the journal-only replay (the oracle) agrees.
#[test]
fn idx_tdd_023_commit_between_property_index_checkpoints_survives_restart_from_checkpoint() {
    let d = tmpdir("023");
    let mut child = spawn_crash_child(&d, "properties", true);
    wait_for_file(&d.join("ckpt.done"), "the completed checkpoint");
    child.wait().unwrap();
    assert!(
        IndexMaintainer::checkpoint_water(&d.join("ckpt"))
            .unwrap()
            .is_some(),
        "the released funnel publishes"
    );

    let (k, m) = restart(&d.join("db"), Some(&d.join("ckpt")));
    assert_complete_contents(&k);
    m.shutdown();
    drop(k);

    let (k2, m2) = restart(&d.join("db"), None);
    assert_complete_contents(&k2);
    m2.shutdown();
    drop(k2);
    let _ = std::fs::remove_dir_all(&d);
}

// --- IDX-TDD-030 — P1-01: verify() under a concurrent writer -----------------

/// The rule: verify() is exact AT CALL START — a clean report covers the
/// rows committed before the check's snapshot; rows committed after belong
/// to the next report. Phase A races verify() against a gated writer and
/// pins that the first clean report after a commit finds the row. Phase B
/// floods 25 un-gated commits through a spinning verify() — no panic, no
/// false completion — and pins the converged index holds every row.
#[test]
fn idx_tdd_030_concurrent_verify_pins_the_call_start_snapshot_rule() {
    let engine = Arc::new(MemoryEngine::new());
    let k = Kernel::open(engine, Arc::new(ManualClock::new(20_000)), 0xCAFE).unwrap();
    k.catalog_create_index("by_body", "note", &["body"])
        .unwrap();
    for i in 0..10 {
        note(&k, &format!("seed-{i:02}"), "group-a");
    }
    let v: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
    let t: Arc<dyn TextIndex> = Arc::new(TokenTextIndex::new());
    let m = Arc::new(IndexMaintainer::new(v, t));
    SchedulerJob::start(&*m, &k).unwrap();
    m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
    let idx = k.property_indexes().unwrap().into_iter().next().unwrap();

    // Phase A — gated ping-pong. The writer never starts row i+1 before
    // the clean report for row i was observed and pinned.
    let go = Arc::new(AtomicBool::new(false));
    let committed = Arc::new(AtomicBool::new(false));
    let k2 = k.clone_handle();
    let go2 = go.clone();
    let committed2 = committed.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..30 {
            while !go2.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            go2.store(false, Ordering::SeqCst);
            note(&k2, &format!("row-{i:03}"), "group-a");
            committed2.store(true, Ordering::SeqCst);
        }
    });

    let clean = |r: &VerifyReport| r.missing.is_empty() && r.stale.is_empty();
    for i in 0..30 {
        go.store(true, Ordering::SeqCst);
        // The race itself: verify() spins while the commit lands. Any
        // answer is legal (the report covers its own snapshot) — none may
        // panic.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            let r = idx.verify(&k).unwrap();
            if clean(&r) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // The commit is journaled; the next clean report must cover it.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !committed.load(Ordering::SeqCst) {
            if std::time::Instant::now() >= deadline {
                panic!("writer never committed row {i}");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        committed.store(false, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let r = idx.verify(&k).unwrap();
            if clean(&r) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("no clean report after row {i} committed");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            k.scan_index("by_body", &[Value::Text(format!("row-{i:03}").into())])
                .unwrap()
                .len(),
            1,
            "a clean report after the commit must find row {i}"
        );
    }
    writer.join().unwrap();

    // Phase B — the un-gated storm. 25 commits flood through while
    // verify() spins; every answer must stay legal, and the end state
    // converges to complete.
    let k3 = k.clone_handle();
    let storm = std::thread::spawn(move || {
        for i in 30..55 {
            note(&k3, &format!("row-{i:03}"), "group-a");
        }
    });
    for _ in 0..300 {
        let _ = idx.verify(&k).unwrap(); // no panic, answer whatever it is
    }
    storm.join().unwrap();
    m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
    let r = idx.verify(&k).unwrap();
    assert!(clean(&r), "the converged index verifies clean");
    for i in 30..55 {
        assert_eq!(
            k.scan_index("by_body", &[Value::Text(format!("row-{i:03}").into())])
                .unwrap()
                .len(),
            1,
            "the converged index holds every storm row"
        );
    }
    m.shutdown();
}
