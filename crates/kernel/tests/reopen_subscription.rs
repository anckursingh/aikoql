//! Subscription replay across a real engine reopen.
//!
//! Launch S-02: moved out of the kernel lib tests — a lib-test target
//! cannot open aikoql-storage-v2 directly (the kernel → v2 → kernel
//! dev-dep cycle compiles the kernel twice, test and non-test, and the
//! `StorageEngine` traits don't unify). Integration tests link the normal
//! kernel lib, where the adapter impl applies.

use aikoql_kernel::*;
use std::sync::Arc;

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

#[test]
fn durable_subscription_survives_reopen() {
    let dir = std::env::temp_dir();
    // The path is pid-only: a killed run's corpse is never removed by
    // a different pid's start-remove — sweep stale siblings (>1 day,
    // so a concurrent live run is untouched) instead.
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let stale = name.starts_with("aikoql_sub_reopen_")
                && e.metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .is_some_and(|t| t < cutoff);
            if stale {
                let _ = std::fs::remove_file(e.path());
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let path = dir.join(format!("aikoql_sub_reopen_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);

    let clock = Arc::new(ManualClock::new(1_000));
    let engine = Arc::new(aikoql_storage_v2::engine::AikoqlStorageEngineV2::open(&path).unwrap());
    let k = Kernel::open(engine.clone(), clock.clone(), 42).unwrap();
    let alice = Subject::new("alice");

    let _rx = k.subscribe("s1".into(), EventFilter::default()).unwrap();
    let r = k
        .remember(RememberRequest::create(alice.clone(), meta("fact")))
        .unwrap();
    // do not ack — subscription must replay after reopen
    drop(k);

    let k2 = Kernel::open(engine, clock, 42).unwrap();
    let replay = k2.replay("s1").unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].koid, r.koid);

    drop(k2); // v2 holds a live dir lock — release before cleanup
    let _ = std::fs::remove_dir_all(&path);
}
