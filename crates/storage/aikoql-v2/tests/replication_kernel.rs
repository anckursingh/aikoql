//! MRFC-0080 R2 — promotion through the kernel: a follower's storage,
//! opened as a kernel, continues the leader's knowledge state (journal
//! head, objects) and never mints an id the leader already committed —
//! even when its clock reads the same millisecond as (or earlier than)
//! the leader's last commit.

mod common;

use aikoql_kernel::*;
use aikoql_storage_v2::engine::StorageAdminApi;
use aikoql_storage_v2::snapshot::restore_from;
use aikoql_storage_v2::AikoqlStorageEngineV2;
use common::dir;
use std::path::Path;
use std::sync::Arc;

const SEED: u64 = 0xA9C9; // the MCP server's seed: every node shares it
const T: u64 = 1_800_000_000_000;

fn meta() -> Metadata {
    Metadata {
        type_name: "doc".into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn tester() -> Subject {
    Subject::with_roles("tester", &["admin"])
}

type Stack = (Arc<Kernel>, Arc<AikoqlStorageEngineV2>, Arc<ManualClock>);

fn stack(d: &Path, clock_ms: u64) -> Stack {
    let engine = Arc::new(AikoqlStorageEngineV2::open(d).expect("open v2"));
    let store: Arc<dyn aikoql_kernel::storage::store::StorageEngine> = engine.clone();
    let clock = Arc::new(ManualClock::new(clock_ms));
    let k = Arc::new(Kernel::open(store, clock.clone(), SEED).expect("open kernel"));
    (k, engine, clock)
}

fn create(k: &Kernel, v: i64) -> KOID {
    let mut req = RememberRequest::create(tester(), meta());
    req.properties.insert("v".into(), Value::Int(v));
    k.remember(req).unwrap().koid
}

/// Promote a follower seeded from the leader at clock `follower_ms`. The
/// leader's history spans `follower_ms` and T, as a real one would.
fn promoted_follower_mints_fresh_ids(tag: &str, follower_ms: u64) {
    let (leader, engine, clock) = stack(&dir(&format!("{tag}-lead")), follower_ms);
    let mut koids = vec![create(&leader, 0)];
    clock.set(T);
    koids.extend((1..3).map(|i| create(&leader, i)));
    let head = leader.journal_head().unwrap();

    let snap = dir(&format!("{tag}-snap"));
    std::fs::remove_dir_all(&snap).ok();
    engine.snapshot_to(&snap).unwrap();
    let fdir = dir(&format!("{tag}-follow"));
    std::fs::remove_dir_all(&fdir).ok();
    drop(restore_from(&snap, fdir.clone()).unwrap());

    let (follower, _fe, _fc) = stack(&fdir, follower_ms);
    assert_eq!(
        follower.journal_head().unwrap(),
        head,
        "the hash chain continues"
    );
    for (i, koid) in koids.iter().enumerate() {
        assert_eq!(
            follower.get(tester(), koid).unwrap().properties.get("v"),
            Some(&Value::Int(i as i64))
        );
    }
    let fresh = create(&follower, 99);
    assert!(
        !koids.contains(&fresh),
        "promoted follower minted a KOID the leader already committed"
    );
    assert_eq!(
        follower
            .get(tester(), &koids[0])
            .unwrap()
            .properties
            .get("v"),
        Some(&Value::Int(0)),
        "the leader's object is untouched"
    );
}

#[test]
fn promotion_in_the_same_millisecond_mints_fresh_ids() {
    promoted_follower_mints_fresh_ids("r2-same-ms", T);
}

#[test]
fn promotion_with_a_lagging_clock_mints_fresh_ids() {
    promoted_follower_mints_fresh_ids("r2-lagging", T - 60_000);
}
