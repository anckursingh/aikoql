//! crash_writer — crash-fault injection helper (used by tests/durability.rs
//! and tests/crash_kill.rs).
//!
//! Commits N knowledge objects with well-known explicit KOIDs to a redb file,
//! prints the committed journal head, then terminates ABRUPTLY via
//! `std::process::exit(0)` — no destructors run, simulating a power-loss /
//! kill -9 at the commit boundary. If `crash_after` is supplied, the process
//! exits after exactly that many commits, testing prefix-atomicity.
//!
//! Loop mode (DB-002 kill-during-write harness): `crash_writer <path> <n>
//! loop <progress>` commits the N records in a cycle, rewriting <progress>
//! with the total commit count after every commit, until it is killed by
//! the test — a real mid-write SIGKILL/taskkill, not a cooperative exit.

use aikoql_kernel::*;
use std::sync::Arc;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: crash_writer <path> <n> [crash_after]");
    let n: u8 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let crash_after: Option<u8> = std::env::args().nth(3).and_then(|s| s.parse().ok());

    let engine = RedbEngine::open(&path).expect("open engine");
    let k = Kernel::open(Arc::new(engine), Arc::new(SystemClock), 7).expect("open kernel");

    let subject = Subject::new("crasher");
    let meta = Metadata {
        type_name: "fact".into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    };

    if crash_after == Some(0) {
        println!("COMMITTED_SEQ=0");
        std::process::exit(0);
    }

    // DB-002 loop mode: commit in a cycle until killed. Progress is the
    // total commit count, written after each commit returns — anything the
    // parent observed as committed must survive a hard kill.
    if std::env::args().nth(3).as_deref() == Some("loop") {
        let progress = std::env::args().nth(4).expect("loop mode needs <progress>");
        // create() pins expected_version=0 (insert-only); first commit of each
        // KOID is a create, repeats take the update path (expected_version
        // None => current head).
        let mut seen = [false; 256];
        let mut total: u64 = 0;
        loop {
            for i in 0..n {
                let mut req = RememberRequest::create(subject.clone(), meta.clone());
                req.koid = Some(KOID::from_bytes([i; KOID_LEN]));
                req.properties.insert("i".into(), Value::Int(i as i64));
                if seen[i as usize] {
                    req.expected_version = None;
                } else {
                    seen[i as usize] = true;
                }
                k.remember(req).expect("commit");
                total += 1;
                std::fs::write(&progress, total.to_string()).expect("write progress");
            }
        }
    }

    for i in 0..n {
        let mut req = RememberRequest::create(subject.clone(), meta.clone());
        req.koid = Some(KOID::from_bytes([i; KOID_LEN]));
        req.properties.insert("i".into(), Value::Int(i as i64));
        k.remember(req).expect("commit");
        if crash_after == Some(i + 1) {
            println!("COMMITTED_SEQ={}", i + 1);
            std::process::exit(0);
        }
    }

    let (seq, _) = k.journal_head().expect("journal head");
    println!("COMMITTED_SEQ={}", seq);

    // Abrupt termination: skips ALL destructors (Database, Kernel, locks).
    std::process::exit(0);
}
