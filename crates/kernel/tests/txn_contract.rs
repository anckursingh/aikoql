//! P5-M10 (ND-10) — the public transaction contract. tx001–007.
//!
//! `begin → stage → commit / rollback` over the kernel's existing
//! OCC/MVCC/atomic-batch machinery. The contract (docs/transaction-contract.md):
//! isolation = SNAPSHOT only — a transaction's reads are pinned to the
//! version set visible at begin; staged writes apply atomically at commit
//! with an OCC pin taken at stage time (a moved head = the deterministic
//! VersionConflict); a commit records its outcome under the txn id in the
//! SAME engine batch as the writes, so a retry after any crash is a recorded
//! no-op; rollback is pure (nothing is ever staged to storage).
//!
//! Observable contract pinned here:
//! - tx001 write/write conflict → deterministic VersionConflict
//! - tx002 snapshot read sees pre-write state
//! - tx003 concurrent readers share their pinned snapshot
//! - tx004 concurrent writers serialize (kernel single-writer — pin, not
//!   new machinery)
//! - tx005 rollback leaves zero residue
//! - tx006 crash during commit (child-kill park windows, rule 5): before the
//!   batch = nothing applied, after the batch = committed + deduped retry
//! - tx007 idempotent retry: the same txn id re-applies nothing (in-process
//!   AND across reopen)
//! - tx000 the contract doc exists and the commit path stays synchronous

use aikoql_kernel::transaction::kernel::{KnowledgeContext, RememberRequest, Subject};
use aikoql_kernel::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

// --- helpers ------------------------------------------------------------------

fn mk() -> Kernel {
    Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap()
}

fn mk_shared() -> (Kernel, Arc<dyn StorageEngine>) {
    let engine: Arc<dyn StorageEngine> = Arc::new(MemoryEngine::new());
    let k = Kernel::open(
        Arc::clone(&engine),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap();
    (k, engine)
}

fn reopen(engine: &Arc<dyn StorageEngine>) -> KResult<Kernel> {
    Kernel::open(
        Arc::clone(engine),
        Arc::new(ManualClock::new(20_000)),
        0xC0FFEE,
    )
}

fn alice() -> KnowledgeContext {
    KnowledgeContext::new(Subject::new("alice"))
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn create_req(who: &str, t: &str, prop: &str, v: i64) -> RememberRequest {
    let mut req = RememberRequest::create(KnowledgeContext::new(Subject::new(who)), meta(t));
    req.properties.insert(prop.into(), Value::Int(v));
    req
}

fn node_count(k: &Kernel) -> usize {
    k.type_koids("Node").unwrap().len()
}

// --- tx000 — contract doc + synchronous commit path ----------------------------

#[test]
fn tx000_contract_doc_and_sync_commit_path() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    for (name, rel) in [
        ("kernel.rs", "src/transaction/kernel.rs"),
        ("txn.rs", "src/transaction/kernel/txn.rs"),
    ] {
        let src = std::fs::read_to_string(format!("{manifest}/{rel}"))
            .unwrap_or_else(|e| panic!("{name} unreadable: {e}"));
        // No async suspension anywhere in the commit path — the pipe lock is
        // a std Mutex held across validation + batch publication.
        assert!(
            !src.contains("async fn"),
            "{name}: async fn in the transaction path"
        );
        assert!(
            !src.contains(".await"),
            "{name}: .await in the transaction path"
        );
    }
    let doc = std::fs::read_to_string(format!("{manifest}/../../docs/transaction-contract.md"))
        .expect("docs/transaction-contract.md");
    for needle in [
        "# Transaction Contract",
        "SNAPSHOT",
        "begin",
        "stage",
        "commit",
        "rollback",
        "idempotent retry",
        "VersionConflict",
        "AIKOQL_TXN_PARK",
        "READ COMMITTED",
    ] {
        assert!(
            doc.contains(needle),
            "transaction-contract.md must document {needle:?}"
        );
    }
}

// --- tx001 — write/write conflict ---------------------------------------------

#[test]
fn tx001_write_write_conflict_is_a_deterministic_error() {
    let k = mk();
    let koid = k
        .remember(create_req("alice", "Node", "i", 1))
        .unwrap()
        .koid;

    // Both transactions pin the SAME snapshot (v1) at stage time.
    let mut t1 = k.begin_transaction(Subject::new("alice"), "tx1").unwrap();
    let mut t2 = k.begin_transaction(Subject::new("alice"), "tx2").unwrap();
    let mut u1 = RememberRequest::update(alice(), koid, meta("Node"));
    u1.properties.insert("i".into(), Value::Int(2));
    let mut u2 = RememberRequest::update(alice(), koid, meta("Node"));
    u2.properties.insert("i".into(), Value::Int(3));
    t1.stage(u1).unwrap();
    t2.stage(u2).unwrap();

    let (r1, _) = t1.commit().unwrap();
    assert_eq!(r1[0].version, 2);

    // The loser gets the deterministic conflict: the version it pinned
    // against vs the winner's committed version.
    match t2.commit().map(|t| t.0) {
        Err(KError::VersionConflict {
            koid: got,
            expected,
            found,
        }) => {
            assert_eq!(got, koid);
            assert_eq!(expected, 1);
            assert_eq!(found, 2);
        }
        other => panic!("expected VersionConflict, got {other:?}"),
    }

    let head = k.get(alice(), &koid).unwrap();
    assert_eq!(head.properties.get("i"), Some(&Value::Int(2)));

    let m = k.transaction_metrics();
    assert_eq!(m.begun, 2);
    assert_eq!(m.committed, 1);
    assert_eq!(m.conflicts, 1);
}

// --- tx002 — snapshot read sees pre-write state --------------------------------

#[test]
fn tx002_snapshot_read_sees_pre_write_state() {
    let k = mk();
    let koid = k
        .remember(create_req("alice", "Node", "i", 1))
        .unwrap()
        .koid;

    let t = k
        .begin_transaction(Subject::new("alice"), "reader")
        .unwrap();

    // A writer commits v2 AFTER the reader's snapshot is pinned.
    let mut u = RememberRequest::update(alice(), koid, meta("Node"));
    u.properties.insert("i".into(), Value::Int(2));
    k.remember(u).unwrap();

    // The pinned reader still sees v1…
    assert_eq!(
        t.get(&koid).unwrap().properties.get("i"),
        Some(&Value::Int(1))
    );
    // …and a fresh transaction sees v2.
    let t2 = k
        .begin_transaction(Subject::new("alice"), "reader2")
        .unwrap();
    assert_eq!(
        t2.get(&koid).unwrap().properties.get("i"),
        Some(&Value::Int(2))
    );
}

// --- tx003 — concurrent readers -------------------------------------------------

#[test]
fn tx003_concurrent_readers_share_the_snapshot() {
    let k = Arc::new(mk());
    let koid = k
        .remember(create_req("alice", "Node", "i", 1))
        .unwrap()
        .koid;

    // Pin eight reader snapshots at v1.
    let readers: Vec<_> = (0..8)
        .map(|n| {
            k.begin_transaction(Subject::new("alice"), format!("reader-{n}"))
                .unwrap()
        })
        .collect();

    // The writer commits v2 after every snapshot is pinned.
    let mut u = RememberRequest::update(alice(), koid, meta("Node"));
    u.properties.insert("i".into(), Value::Int(2));
    k.remember(u).unwrap();

    let mut handles = Vec::new();
    for t in readers {
        handles.push(std::thread::spawn(move || {
            t.get(&koid).unwrap().properties.get("i") == Some(&Value::Int(1))
        }));
    }
    for h in handles {
        assert!(h.join().unwrap(), "a reader drifted off its snapshot");
    }
}

// --- tx004 — concurrent writers serialize ---------------------------------------

#[test]
fn tx004_concurrent_writers_serialize() {
    let k = Arc::new(mk());
    let mut handles = Vec::new();
    for w in 0..4u64 {
        let k = Arc::clone(&k);
        handles.push(std::thread::spawn(move || {
            for i in 0..5u64 {
                let mut t = k
                    .begin_transaction(Subject::new("alice"), format!("writer-{w}-{i}"))
                    .unwrap();
                t.stage(create_req("alice", "Node", "i", (w * 10 + i) as i64))
                    .unwrap();
                let (r, _) = t.commit().unwrap();
                assert_eq!(r[0].version, 1, "writer {w}-{i} lost its create");
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    // Every create committed exactly once — distinct koids, no conflicts.
    assert_eq!(node_count(&k), 20);
}

// --- tx005 — rollback leaves zero residue ----------------------------------------

#[test]
fn tx005_rollback_leaves_zero_residue() {
    let k = mk();
    let koid = k
        .remember(create_req("alice", "Node", "i", 1))
        .unwrap()
        .koid;
    let seq_before = k.journal_head().unwrap().0;
    let count_before = node_count(&k);

    let mut t = k
        .begin_transaction(Subject::new("alice"), "aborted")
        .unwrap();
    t.stage(create_req("alice", "Node", "i", 2)).unwrap();
    let mut u = RememberRequest::update(alice(), koid, meta("Node"));
    u.properties.insert("i".into(), Value::Int(9));
    t.stage(u).unwrap();
    t.rollback();

    // Nothing was staged to storage: journal, objects, and the head are all
    // exactly as before the transaction.
    assert_eq!(k.journal_head().unwrap().0, seq_before);
    assert_eq!(node_count(&k), count_before);
    let head = k.get(alice(), &koid).unwrap();
    assert_eq!(head.properties.get("i"), Some(&Value::Int(1)));

    let m = k.transaction_metrics();
    assert_eq!(m.rolled_back, 1);
    assert_eq!(m.committed, 0);
}

// --- tx006 — crash during commit (child-kill park windows) ------------------------

fn txn_crasher_exe() -> PathBuf {
    let mut exe = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    exe.push("../../target/debug/examples/txn_crasher");
    #[cfg(windows)]
    exe.set_extension("exe");
    assert!(
        exe.exists(),
        "txn_crasher example not built at {:?}; run `cargo build --example txn_crasher` first",
        exe
    );
    exe
}

fn tmp_db(name: &str) -> (PathBuf, PathBuf) {
    let mut db = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    db.push(format!(
        "aikoql_txn_{}_{}_{}",
        name,
        std::process::id(),
        stamp
    ));
    let mut marker = db.clone();
    marker.set_extension("marker");
    let _ = std::fs::remove_dir_all(&db);
    let _ = std::fs::remove_file(&marker);
    (db, marker)
}

/// Hard-kill the child: SIGKILL on Unix, taskkill /F on Windows (the
/// crash_kill.rs d05 pattern — best-effort; the asserts after the kill fail
/// loudly if the kill did not happen).
fn hard_kill(child: &std::process::Child) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &child.id().to_string()])
            .status();
    }
}

fn wait_for_marker(marker: &std::path::Path, child: &mut std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if marker.exists() {
            return;
        }
        if Instant::now() > deadline {
            let died = child.try_wait().ok().flatten();
            hard_kill(child);
            let _ = child.wait();
            panic!(
                "txn_crasher never reached the park window (marker {:?} absent); child status: {died:?}",
                marker
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Reopen the store after a hard kill. Windows may release the dir lock a
/// beat after taskkill returns — retry until the open succeeds.
fn reopen_after_kill(path: &std::path::Path) -> Kernel {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(engine) = aikoql_storage_v2::AikoqlStorageEngineV2::open(path) {
            if let Ok(k) = Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xBEEF) {
                return k;
            }
        }
        assert!(
            Instant::now() <= deadline,
            "store must reopen after a hard kill (fail-safe)"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kill the child parked at `stage`, then run the common assertions.
fn kill_at_stage(name: &str, stage: &str) -> (Kernel, PathBuf, PathBuf) {
    let (db, marker) = tmp_db(name);
    let mut child = std::process::Command::new(txn_crasher_exe())
        .arg(&db)
        .arg("crash-txn")
        .env("AIKOQL_TXN_PARK", stage)
        .env("AIKOQL_TXN_PARK_MARKER", &marker)
        .spawn()
        .expect("spawn txn_crasher");
    wait_for_marker(&marker, &mut child);
    hard_kill(&child);
    let killed = child.wait().expect("reap child");
    assert!(
        !killed.success(),
        "child should have been killed, not exited"
    );
    let k = reopen_after_kill(&db);
    (k, db, marker)
}

#[test]
fn tx006a_crash_before_commit_applies_nothing() {
    let (k, db, marker) = kill_at_stage("txn_pre", "pre_commit");

    // The park fires before the engine batch: the commit never landed.
    assert_eq!(node_count(&k), 0);

    // The client retries the same txn id: the write applies cleanly.
    let mut t = k
        .begin_transaction(Subject::new("alice"), "crash-txn")
        .unwrap();
    t.stage(create_req("alice", "Node", "i", 7)).unwrap();
    let (r, _) = t.commit().unwrap();
    assert_eq!(r[0].version, 1);
    assert_eq!(node_count(&k), 1);

    // And a further re-apply with the same id and the SAME body is the
    // recorded no-op (P5-M20: retry identity is id + body).
    let seq = k.journal_head().unwrap().0;
    let mut t = k
        .begin_transaction(Subject::new("alice"), "crash-txn")
        .unwrap();
    t.stage(create_req("alice", "Node", "i", 7)).unwrap();
    assert_eq!(t.commit().unwrap().0, r);
    assert_eq!(node_count(&k), 1);
    assert_eq!(k.journal_head().unwrap().0, seq);

    let _ = std::fs::remove_dir_all(&db);
    let _ = std::fs::remove_file(&marker);
}

#[test]
fn tx006b_crash_after_commit_keeps_the_commit_and_dedupes() {
    let (k, db, marker) = kill_at_stage("txn_post", "post_commit");

    // The park fires after the engine batch: the commit IS durable.
    assert_eq!(node_count(&k), 1);
    let koids = k.type_koids("Node").unwrap();
    let head = k.get(alice(), &koids[0]).unwrap();
    assert_eq!(head.properties.get("i"), Some(&Value::Int(7)));

    // The retry with the same id and the SAME body re-applies nothing and
    // returns the recorded outcome (P5-M20: retry identity is id + body).
    let seq = k.journal_head().unwrap().0;
    let mut t = k
        .begin_transaction(Subject::new("alice"), "crash-txn")
        .unwrap();
    t.stage(create_req("alice", "Node", "i", 7)).unwrap();
    let (r, _) = t.commit().unwrap();
    assert_eq!(r[0].version, 1);
    assert_eq!(r[0].koid, koids[0]);
    assert_eq!(node_count(&k), 1);
    assert_eq!(k.journal_head().unwrap().0, seq);
    let head = k.get(alice(), &koids[0]).unwrap();
    assert_eq!(head.properties.get("i"), Some(&Value::Int(7)));

    let _ = std::fs::remove_dir_all(&db);
    let _ = std::fs::remove_file(&marker);
}

// --- tx007 — idempotent retry -----------------------------------------------------

#[test]
fn tx007_idempotent_retry_is_a_recorded_noop() {
    let (k, engine) = mk_shared();
    let mut t = k.begin_transaction(Subject::new("alice"), "idem").unwrap();
    t.stage(create_req("alice", "Node", "i", 1)).unwrap();
    let (r1, _) = t.commit().unwrap();
    assert_eq!(r1[0].version, 1);

    // Same id again: the staged op (the SAME body — P5-M20: retry identity
    // is id + body) is ignored and the recorded outcome returns.
    let seq = k.journal_head().unwrap().0;
    let mut t2 = k.begin_transaction(Subject::new("alice"), "idem").unwrap();
    t2.stage(create_req("alice", "Node", "i", 1)).unwrap();
    assert_eq!(t2.commit().unwrap().0, r1);
    assert_eq!(node_count(&k), 1);
    assert_eq!(k.journal_head().unwrap().0, seq);

    let m = k.transaction_metrics();
    assert_eq!(m.committed, 1);
    assert_eq!(m.deduped_retries, 1);

    // The record survives a reopen — a retry after restart is still a no-op.
    let k2 = reopen(&engine).unwrap();
    let mut t3 = k2.begin_transaction(Subject::new("alice"), "idem").unwrap();
    t3.stage(create_req("alice", "Node", "i", 1)).unwrap();
    assert_eq!(t3.commit().unwrap().0, r1);
    assert_eq!(node_count(&k2), 1);
}

// --- tx008 — staged writes are invisible to the transaction's own reads -------

/// The documented contract: `txn.get` reads the SNAPSHOT — a transaction
/// never sees its own staged writes. Pinned here so a "read-your-writes
/// convenience" cannot silently change the semantics.
#[test]
fn tx008_staged_write_is_invisible_to_the_transactions_own_reads() {
    let k = mk();
    let koid = k
        .remember(create_req("alice", "Node", "i", 1))
        .unwrap()
        .koid;

    let mut t = k.begin_transaction(Subject::new("alice"), "tx8").unwrap();
    let mut u = RememberRequest::update(alice(), koid, meta("Node"));
    u.properties.insert("i".into(), Value::Int(2));
    t.stage(u).unwrap();

    // Staged write is NOT visible to the transaction itself (contract).
    assert_eq!(
        t.get(&koid).unwrap().properties.get("i"),
        Some(&Value::Int(1)),
        "a transaction must not read its own staged writes"
    );

    let (r, _) = t.commit().unwrap();
    assert_eq!(r[0].version, 2);
    assert_eq!(
        k.get(alice(), &koid).unwrap().properties.get("i"),
        Some(&Value::Int(2))
    );
}

// --- tx009 — retry identity: same id + different body fails closed ------------

/// P5-M20 (PR6 P0-03): the recorded-retry check keys on the txn id alone —
/// the SAME id with a DIFFERENT body silently replays the original outcome.
/// A retry must fail closed instead: only the identical body is a retry.
#[test]
fn tx009_same_txn_id_with_a_different_body_fails_closed() {
    let k = mk();
    let mut t = k.begin_transaction(Subject::new("alice"), "spent").unwrap();
    t.stage(create_req("alice", "Node", "i", 1)).unwrap();
    let (r1, _) = t.commit().unwrap();
    assert_eq!(r1[0].version, 1);

    // Same id, DIFFERENT body: never replay the recorded outcome.
    let mut t2 = k.begin_transaction(Subject::new("alice"), "spent").unwrap();
    t2.stage(create_req("alice", "Node", "i", 99)).unwrap();
    assert!(
        t2.commit().is_err(),
        "same txn_id with a different body must fail closed, not replay the recorded outcome"
    );
    assert_eq!(node_count(&k), 1, "the different body must not apply");
    assert_eq!(k.transaction_metrics().committed, 1);
}

// --- tx010 — retry identity: same id + same body is the recorded retry --------

/// The matching half of the retry identity: an IDENTICAL body re-submit is
/// the recorded no-op (deduped=true, original outcome, deduped_retries).
#[test]
fn tx010_same_txn_id_with_the_same_body_is_a_recorded_retry() {
    let k = mk();
    let mut t = k.begin_transaction(Subject::new("alice"), "retry").unwrap();
    t.stage(create_req("alice", "Node", "i", 1)).unwrap();
    let (r1, deduped) = t.commit().unwrap();
    assert!(!deduped);
    assert_eq!(r1[0].version, 1);

    let mut t2 = k.begin_transaction(Subject::new("alice"), "retry").unwrap();
    t2.stage(create_req("alice", "Node", "i", 1)).unwrap();
    let (r2, deduped) = t2.commit().unwrap();
    assert!(deduped, "an identical body re-submit is the recorded retry");
    assert_eq!(r2, r1, "the retry returns the original outcome");
    assert_eq!(node_count(&k), 1);
    assert_eq!(k.transaction_metrics().deduped_retries, 1);
}
