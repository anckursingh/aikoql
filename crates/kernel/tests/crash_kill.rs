//! DB-002 kill-during-write harness (TP-2a, docs/TESTING-PLAN.md).
//!
//! The suite asks: "Kill process during write; verify recovery/fail-safe
//! behavior." durability.rs already proves abrupt *exit* at the commit
//! boundary (d04, no destructors); this test proves a real mid-write hard
//! kill: the crash_writer example commits in a loop while the parent
//! SIGKILLs / taskkills it, then the store is reopened and checked for
//! silent loss or corruption.

use aikoql_kernel::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn tmp_db(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "aikoql_kill_{}_{}_{}.redb",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn writer_exe() -> PathBuf {
    let mut exe = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    exe.push("../../target/debug/examples/crash_writer");
    #[cfg(windows)]
    exe.set_extension("exe");
    assert!(
        exe.exists(),
        "crash_writer example not built at {:?}; run `cargo build --examples` first",
        exe
    );
    exe
}

/// Hard-kill the child: SIGKILL on Unix, taskkill /F on Windows. Best-effort:
/// an already-dead child fails the kill tool but is exactly the state we want
/// — the asserts after the kill (reopen, seq >= observed) fail loudly instead.
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

/// The writer's progress file: total commits observed after each commit.
/// Same-host page-cache coherence makes polling reliable without fsync.
fn read_progress(p: &std::path::Path) -> u64 {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

#[test]
fn d05_kill_during_write_preserves_committed_prefix() {
    let path = tmp_db("kill");
    let progress = path.with_extension("progress");
    let target: u64 = 200;

    // Writer child: commits KOIDs 0..N in a cycle until killed.
    let mut child = std::process::Command::new(writer_exe())
        .arg(&path)
        .arg("8")
        .arg("loop")
        .arg(&progress)
        .spawn()
        .expect("spawn crash_writer");

    // Wait until ≥ target commits were observed committed, then hard-kill
    // mid-stream.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if read_progress(&progress) >= target {
            break;
        }
        if Instant::now() > deadline {
            let died = child.try_wait().ok().flatten();
            hard_kill(&child);
            let _ = child.wait(); // reap before the panic unwinds
            panic!(
                "writer never reached {target} commits (progress={}); child status: {died:?}",
                read_progress(&progress)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    hard_kill(&child);

    // Reap: wait() returns once the OS has released the child's handles.
    let killed = child.wait().expect("reap child");
    assert!(
        !killed.success(),
        "child should have been killed, not exited"
    );

    // Windows may release the file handle a beat after taskkill returns.
    let observed = read_progress(&progress);
    let mut k = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while k.is_none() && Instant::now() <= deadline {
        if let Ok(engine) = RedbEngine::open(&path) {
            k = Kernel::open(Arc::new(engine), Arc::new(SystemClock), 7).ok();
        }
        if k.is_none() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let k = k.expect("store must reopen after a hard kill (fail-safe)");

    // No silent loss: every commit the writer reported is on disk.
    let (seq, _) = k.journal_head().unwrap();
    assert!(
        seq >= observed,
        "journal head {seq} < {observed} observed commits — committed data lost"
    );

    // No corruption: every KOID the writer could have committed is readable
    // with its exact value, and the audit chain validates end to end.
    let crasher = Subject::new("crasher");
    for i in 0..8u8 {
        let id = KOID::from_bytes([i; KOID_LEN]);
        let ko = k
            .get(&crasher, &id)
            .unwrap_or_else(|e| panic!("missing KO {}: {}", i, e));
        assert_eq!(ko.properties.get("i"), Some(&Value::Int(i as i64)));
        assert!(
            k.prove(&crasher, &id).unwrap().chain_valid,
            "audit chain broken after kill for KO {i}"
        );
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&progress);
}
