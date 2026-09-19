//! PR6-011 — storage semantics through the kernel boundary (review §13):
//! Kernel → StorageEngine → AikoqlStorageEngineV2 → Db, real production APIs.
//!
//! Four rows: CRUD survives reopen; relationships survive compaction +
//! checkpoint (driven through the §22 StorageAdminApi maintenance surface);
//! snapshot/restore preserves directory state; the Class-B job
//! acknowledgement (the persisted admission + result records) survives
//! restart and re-issues identically.

mod common;

use aikoql_kernel::*;
use aikoql_storage_v2::db::Config;
use aikoql_storage_v2::engine::StorageAdminApi;
use aikoql_storage_v2::AikoqlStorageEngineV2;
use common::dir;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SEED: u64 = 0xBEEF;

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

/// The production caller identity (admin so reads cross contexts).
fn tester() -> Subject {
    Subject::with_roles("tester", &["admin"])
}

/// The production stack over `d`: engine + kernel. Drop both (kernel first,
/// then engine) to release the directory before reopening.
fn stack(d: &Path) -> (Arc<Kernel>, Arc<AikoqlStorageEngineV2>) {
    let engine = Arc::new(AikoqlStorageEngineV2::open(d).expect("open v2"));
    let store: Arc<dyn aikoql_kernel::storage::store::StorageEngine> = engine.clone();
    let k = Arc::new(Kernel::open(store, Arc::new(SystemClock), SEED).expect("open kernel"));
    (k, engine)
}

/// row 1 — CRUD through the kernel survives a v2 reopen: the update and the
/// tombstone are durable, and the deleted koid stays invisible.
#[test]
fn kernel_crud_survives_v2_reopen() {
    let d = dir("kb-crud-live");
    let (k, engine) = stack(&d);
    let created = k
        .remember(RememberRequest::create(tester(), meta("doc")))
        .unwrap()
        .koid;
    let mut up = RememberRequest::update(tester(), created, meta("doc"));
    up.properties.insert("v".into(), Value::Int(2));
    k.remember(up).unwrap();
    let doomed = k
        .remember(RememberRequest::create(tester(), meta("doc")))
        .unwrap()
        .koid;
    k.forget(tester(), &doomed, ForgetMode::Tombstone, None, None)
        .unwrap();
    drop(k);
    drop(engine);

    let (k2, _engine2) = stack(&d);
    let ko = k2.get(tester(), &created).unwrap();
    assert_eq!(
        ko.properties.get("v"),
        Some(&Value::Int(2)),
        "the update survived the reopen"
    );
    assert_eq!(
        k2.get(tester(), &doomed).unwrap().lifecycle.state,
        LifecycleState::Deleted,
        "the tombstone survived the reopen"
    );
    let docs: Vec<KOID> = k2
        .scan_by_type(&tester(), "doc")
        .unwrap()
        .into_iter()
        .map(|ko| ko.koid)
        .collect();
    assert_eq!(docs, vec![created], "the delete is durable");
}

/// row 2 — relationships survive compaction and checkpoint: both maintenance
/// ops run through the StorageAdminApi surface on the live production stack,
/// and the edges (plus every row) answer identically after a reopen.
#[test]
fn kernel_relationships_survive_compaction_and_checkpoint() {
    let d = dir("kb-rel-live");
    let mut cfg = Config::new(d.clone());
    cfg.memtable_bytes = 512;
    cfg.l0_compact_trigger = 0;
    let engine = Arc::new(AikoqlStorageEngineV2::open_with_config(cfg).unwrap());
    let store: Arc<dyn aikoql_kernel::storage::store::StorageEngine> = engine.clone();
    let k = Arc::new(Kernel::open(store, Arc::new(SystemClock), SEED).unwrap());

    let a = k
        .remember(RememberRequest::create(tester(), meta("node")))
        .unwrap()
        .koid;
    let b = k
        .remember(RememberRequest::create(tester(), meta("node")))
        .unwrap()
        .koid;
    let c = k
        .remember(RememberRequest::create(tester(), meta("node")))
        .unwrap()
        .koid;
    // bulk rows so the checkpoint flush spans several segments
    for _ in 0..60 {
        k.remember(RememberRequest::create(tester(), meta("bulk")))
            .unwrap();
    }
    let mut req = RememberRequest::update(tester(), a, meta("node"));
    req.relationships.push(RelationshipRef {
        rel_type: "linked".into(),
        target: b,
        direction: Direction::Outbound,
    });
    k.remember(req).unwrap();
    let mut req = RememberRequest::update(tester(), b, meta("node"));
    req.relationships.push(RelationshipRef {
        rel_type: "linked".into(),
        target: c,
        direction: Direction::Outbound,
    });
    k.remember(req).unwrap();

    engine.storage_checkpoint().unwrap();
    let cs = engine.storage_compact().unwrap();
    assert!(
        cs.segments_in > cs.segments_out,
        "the compaction did real work ({:?} → {:?} segments)",
        cs.segments_in,
        cs.segments_out
    );

    drop(k);
    drop(engine);
    let (k2, _engine2) = stack(&d);
    assert_eq!(
        k2.outbound_edges(&a, Some("linked")).unwrap(),
        vec![("linked".into(), b)],
        "the a→b edge survived compaction+checkpoint+reopen"
    );
    assert_eq!(
        k2.outbound_edges(&b, Some("linked")).unwrap(),
        vec![("linked".into(), c)]
    );
    assert_eq!(
        k2.inbound_edges(&b, Some("linked")).unwrap(),
        vec![("linked".into(), a)]
    );
    assert_eq!(k2.scan_by_type(&tester(), "node").unwrap().len(), 3);
    assert_eq!(k2.scan_by_type(&tester(), "bulk").unwrap().len(), 60);
}

/// row 3 — the engine-native snapshot/restore round trip preserves the
/// directory state the kernel sees: types, versions, and edges answer
/// identically from the restored directory, and the restore itself is
/// durable (the restored directory survives its own reopen).
#[test]
fn kernel_snapshot_restore_preserves_directory_state() {
    let live = dir("kb-snap-live");
    let (k1, e1) = stack(&live);
    let a = k1
        .remember(RememberRequest::create(tester(), meta("person")))
        .unwrap()
        .koid;
    let b = k1
        .remember(RememberRequest::create(tester(), meta("person")))
        .unwrap()
        .koid;
    let mut req = RememberRequest::update(tester(), a, meta("person"));
    req.properties.insert("age".into(), Value::Int(41));
    req.relationships.push(RelationshipRef {
        rel_type: "knows".into(),
        target: b,
        direction: Direction::Outbound,
    });
    k1.remember(req).unwrap();

    let snap = dir("kb-snap-dir");
    StorageAdminApi::snapshot_to(e1.as_ref(), &snap).unwrap();
    drop(k1);
    drop(e1);

    let target = dir("kb-snap-target");
    let e2 = Arc::new(AikoqlStorageEngineV2::open(&target).unwrap());
    StorageAdminApi::restore_from(e2.as_ref(), &snap).unwrap();
    let store2: Arc<dyn aikoql_kernel::storage::store::StorageEngine> = e2.clone();
    let k2 = Arc::new(Kernel::open(store2, Arc::new(SystemClock), SEED).unwrap());

    let ko = k2.get(tester(), &a).unwrap();
    assert_eq!(
        ko.properties.get("age"),
        Some(&Value::Int(41)),
        "the version restored"
    );
    assert_eq!(
        k2.outbound_edges(&a, Some("knows")).unwrap(),
        vec![("knows".into(), b)],
        "the edge restored"
    );
    assert_eq!(k2.scan_by_type(&tester(), "person").unwrap().len(), 2);
    drop(k2);
    drop(e2);

    // the restored directory is itself a durable production directory
    let (k3, _e3) = stack(&target);
    assert_eq!(
        k3.get(tester(), &a).unwrap().properties.get("age"),
        Some(&Value::Int(41)),
        "the restored state survives its own reopen"
    );
    assert_eq!(
        k3.outbound_edges(&a, Some("knows")).unwrap(),
        vec![("knows".into(), b)]
    );
}

/// row 4 — the Class-B job acknowledgement survives restart: the persisted
/// admission record keeps its status and result, and an identical resubmit
/// re-issues the same acknowledgement (the retry-identity property).
#[test]
fn transaction_acknowledgement_survives_restart() {
    let d = dir("kb-ack-live");
    let (k, engine) = stack(&d);
    let props: PropertyMap = [("zone".to_string(), Value::Text("a".into()))]
        .into_iter()
        .collect();
    let mut seed_req = RememberRequest::create(tester(), meta("sensor"));
    seed_req.properties = props.clone();
    k.remember(seed_req).unwrap();
    let handle = k.reason(&tester(), "sensor", props).unwrap();
    let st = wait_status(
        &k,
        handle.job_id,
        JobStatus::Completed,
        Duration::from_secs(10),
    );
    assert_eq!(st, JobStatus::Completed, "the job must complete");
    let before = k.job_result(handle.job_id).unwrap();
    assert_eq!(before.len(), 1, "the acknowledged result has one claim");
    drop(k);
    drop(engine);

    let (k2, _engine2) = stack(&d);
    assert_eq!(
        k2.job_status(handle.job_id).unwrap(),
        JobStatus::Completed,
        "the acknowledgement survives the restart"
    );
    let after = k2.job_result(handle.job_id).unwrap();
    assert_eq!(after.len(), before.len());
    assert_eq!(
        after[0].koid, before[0].koid,
        "the acknowledged result survives verbatim"
    );
    // P5-M20 retry identity: the admission table was rebuilt from the
    // persisted record, so the identical input re-issues the same ack.
    let again = k2
        .reason(
            &tester(),
            "sensor",
            [("zone".to_string(), Value::Text("a".into()))]
                .into_iter()
                .collect(),
        )
        .unwrap();
    assert_eq!(
        again.job_id, handle.job_id,
        "identical retry dedupes to the same acknowledgement"
    );
}

fn wait_status(k: &Kernel, job_id: u64, want: JobStatus, deadline: Duration) -> JobStatus {
    let start = Instant::now();
    loop {
        let s = k.job_status(job_id).unwrap();
        if s == want || Instant::now() - start > deadline {
            return s;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
