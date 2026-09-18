//! P5-M22 — index DDL lifecycle + checkpoint/resume (PR6 P1-07, P1-14,
//! P1-15, P1-16).
//!
//! The suite pins the two M22 contracts before the fix:
//! - idx4-001: the catalog DDL row is a stateful lifecycle — a crash between
//!   the durable row and the registry push converges to ONE state at reopen
//!   (a declared row is completed, a dropping row never enters the registry),
//!   and the convergence must not depend on the maintainer's replay.
//! - idx4-002: restart at a checkpoint resumes at the water — the journal
//!   replay applies only the events after the checkpoint, and the resume
//!   path seeds the in-memory property indexes the checkpoint cannot carry.
//! - idx4-003: the HNSW checkpoint pair carries a generation manifest
//!   published atomically last — a torn pair fails closed at load.

use aikoql_kernel::*;
use aikoql_scheduler::IndexMaintainer;
use aikoql_vector::{HnswVectorIndex, TantivyTextIndex};
use std::sync::Arc;
use std::time::Duration;

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn create(k: &Kernel, subj: &Subject, type_name: &str, body: &str) -> KOID {
    let mut props = PropertyMap::new();
    props.insert("body".into(), Value::Text(body.into()));
    k.remember(RememberRequest {
        context: subj.into(),
        koid: None,
        expected_version: Some(0),
        idempotency_key: None,
        metadata: meta(type_name),
        properties: props,
        semantic: None,
        relationships: vec![],
        security: None,
        extensions: ExtensionMap::new(),
        origin: Origin::Human,
        note: None,
        referential_policy: ReferentialPolicy::default(),
    })
    .unwrap()
    .koid
}

// --- idx4-001 — crash between DDL persist and registry push (PR6 P1-07) ---------

/// The DDL row was durably persisted but the registry push and the
/// synchronous make-good rebuild never ran (the crash window). Reopen must
/// converge: the declaration completes — the index exists, answers the
/// committed rows, and is usable — WITHOUT a maintainer replay. Today the
/// row loads with empty contents and nothing ever seeds it (the replay is
/// the only filler, and a resumed restart does not replay).
#[test]
fn idx4_001_declared_crash_row_converges_at_reopen() {
    let engine = Arc::new(MemoryEngine::new());
    let clock = Arc::new(ManualClock::new(20_000));
    let k = Kernel::open(engine.clone(), clock.clone(), 0xCAFE).unwrap();
    let a = Subject::new("alice");

    // Manufacture the crash window: the catalog row only — no registry push,
    // no rebuild.
    let mut props = PropertyMap::new();
    props.insert("type_name".into(), Value::Text("note".into()));
    props.insert(
        "properties".into(),
        Value::List(vec![Value::Text("body".into())]),
    );
    props.insert("state".into(), Value::Text("declared".into()));
    k.catalog_create_entry("index", "by_body", props).unwrap();
    let first = create(&k, &a, "note", "committed before the crash");
    let second = create(&k, &a, "note", "also committed before the crash");
    drop(k);

    // Reopen — no maintainer: the convergence is the open's duty.
    let k2 = Kernel::open(engine, clock, 0x1D3C).unwrap();
    assert_eq!(
        k2.scan_index(
            "by_body",
            &[Value::Text("committed before the crash".into())]
        )
        .unwrap(),
        vec![first],
        "the crashed declaration is completed at reopen: the row answers"
    );
    assert_eq!(
        k2.scan_index(
            "by_body",
            &[Value::Text("also committed before the crash".into())]
        )
        .unwrap(),
        vec![second],
        "every committed row is seeded"
    );
}

/// The inverse window: a DROPPING row at reopen (the crash between the
/// durable drop mark and the registry removal) must never enter the live
/// registry. Today every index row loads, so the dropped index comes back.
#[test]
fn idx4_001b_dropping_row_never_enters_the_registry() {
    let engine = Arc::new(MemoryEngine::new());
    let clock = Arc::new(ManualClock::new(20_000));
    let k = Kernel::open(engine.clone(), clock.clone(), 0xCAFE).unwrap();

    let mut props = PropertyMap::new();
    props.insert("type_name".into(), Value::Text("note".into()));
    props.insert(
        "properties".into(),
        Value::List(vec![Value::Text("body".into())]),
    );
    props.insert("state".into(), Value::Text("dropping".into()));
    k.catalog_create_entry("index", "ghost", props).unwrap();
    drop(k);

    let k2 = Kernel::open(engine, clock, 0x1D3C).unwrap();
    let names: Vec<String> = k2
        .property_indexes()
        .unwrap()
        .iter()
        .map(|i| i.name().to_string())
        .collect();
    assert!(
        !names.contains(&"ghost".to_string()),
        "a dropping row never enters the registry: {names:?}"
    );
}

// --- idx4-002 — checkpoint/resume (PR6 P1-15) ------------------------------------

/// Checkpoint at water H, commit N more, restart from the checkpoint: the
/// resume must apply the N events after H. The vector/text slots come back
/// from the checkpoint files; the property indexes come back from their own
/// checkpoint files (P5-M26, per-index — a missing file reseeds from the
/// committed heads). The observable pin: rows committed before the
/// checkpoint still answer, and the N events after H apply on resume.
#[test]
fn idx4_002_restart_replays_only_events_after_the_checkpoint() {
    let engine = Arc::new(MemoryEngine::new());
    let clock = Arc::new(ManualClock::new(30_000));
    let k = Kernel::open(engine.clone(), clock.clone(), 0x5EED).unwrap();
    let a = Subject::new("alice");
    k.catalog_create_index("by_body", "note", &["body"])
        .unwrap();
    for i in 0..70 {
        create(&k, &a, "note", &format!("pre-checkpoint {i}"));
    }
    let vectors: Arc<dyn VectorIndex> = Arc::new(HnswVectorIndex::new(0, 10_000));
    let text: Arc<dyn TextIndex> = Arc::new(TantivyTextIndex::new().unwrap());
    let m = IndexMaintainer::start(&k, vectors.clone(), text.clone()).unwrap();
    m.wait_caught_up(&k, Duration::from_secs(30)).unwrap();
    let water = m.water();
    // The checkpoint pair must be loadable for the RED to pin the resume
    // semantics — an HNSW checkpoint with no vectors has no payload section.
    vectors.upsert(KOID::from_bytes([3u8; KOID_LEN]), "m", &[0.1, 0.2]);
    let dir = std::env::temp_dir().join(format!("idx4_002_ckpt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    m.checkpoint(&k, &dir).unwrap();
    assert!(dir.join("COMPLETE").exists());

    for i in 0..130 {
        create(&k, &a, "note", &format!("post-checkpoint {i}"));
    }
    m.wait_caught_up(&k, Duration::from_secs(30)).unwrap();
    m.shutdown();
    drop(k);

    // Restart: the checkpoint pair loads, the maintainer resumes at the water.
    let k2 = Kernel::open(engine, clock, 0x7A17).unwrap();
    let vectors2: Arc<dyn VectorIndex> =
        Arc::new(HnswVectorIndex::load(&dir.join("vectors")).unwrap());
    let text2: Arc<dyn TextIndex> = Arc::new(TantivyTextIndex::load(&dir.join("text")).unwrap());
    let m2 = IndexMaintainer::start_at(&k2, vectors2, text2, Some(water), Some(&dir)).unwrap();
    // Give the live loop its chance — today nothing ever arrives (the tail
    // events predate the subscription and the resume skipped the replay).
    std::thread::sleep(Duration::from_millis(300));

    assert_eq!(
        k2.scan_index("by_body", &[Value::Text("pre-checkpoint 0".into())])
            .unwrap()
            .len(),
        1,
        "rows committed before the checkpoint still answer after a resume restart"
    );
    assert_eq!(
        k2.scan_index("by_body", &[Value::Text("post-checkpoint 129".into())])
            .unwrap()
            .len(),
        1,
        "events after the checkpoint apply live"
    );
    m2.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A resume water beyond the journal head means the checkpoint belongs to a
/// different (newer) database — the loaded index files would serve foreign
/// data, so the resume must fail closed and the host rebuilds fresh. Today
/// the water is trusted blindly: every event seq <= water is discarded
/// forever, including all future ones.
#[test]
fn idx4_002b_resume_water_past_the_head_fails_closed() {
    let clock = Arc::new(ManualClock::new(30_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock, 0x5EED).unwrap();
    let vectors: Arc<dyn VectorIndex> = Arc::new(HnswVectorIndex::new(0, 10_000));
    let text: Arc<dyn TextIndex> = Arc::new(TantivyTextIndex::new().unwrap());
    assert!(
        IndexMaintainer::start_at(&k, vectors, text, Some(1_000_000), None).is_err(),
        "a resume water beyond the journal head must fail closed"
    );
}

// --- idx4-003 — the HNSW checkpoint pair's generation manifest (PR6 P1-14) -------

/// The graph and its meta must come from ONE generation. The manifest is
/// published atomically last; the loader gates on it — a pair torn across
/// two checkpoints (or missing the manifest) fails closed at load. Today
/// the loader reads the two files independently and accepts any mix.
#[test]
fn idx4_003_torn_checkpoint_pair_fails_closed() {
    let idx = HnswVectorIndex::new(2, 100);
    let kid = KOID::from_bytes([7u8; KOID_LEN]);
    idx.upsert(kid, "m", &[1.0, 0.0]);
    let dir = std::env::temp_dir().join(format!("idx4_003_ckpt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    idx.checkpoint(&dir).unwrap();

    // A second checkpoint yields another generation; splicing its meta over
    // the first pair tears it (graph from generation 1, meta from 2).
    let other = dir.join("other");
    idx.checkpoint(&other).unwrap();
    std::fs::copy(other.join("meta.json"), dir.join("meta.json")).unwrap();

    assert!(
        HnswVectorIndex::load(&dir).is_err(),
        "a pair torn across generations fails closed at load"
    );

    // The manifest is the gate — a checkpoint pair without one fails too.
    std::fs::remove_file(dir.join("manifest.json")).unwrap();
    assert!(
        HnswVectorIndex::load(&dir).is_err(),
        "a checkpoint without its generation manifest fails closed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- idx4-005 — property-index cost cells (PR6 rule 11, measurement-first) --------

fn idx4_ko(koid: KOID, title: &str) -> KnowledgeObject {
    let mut ko = KnowledgeObject::new(
        koid,
        Metadata {
            type_name: "note".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
        SecurityDescriptor {
            owner: "alice".into(),
            acl: vec![],
            classification: None,
        },
    );
    ko.properties
        .insert("title".into(), Value::Text(title.into()));
    ko
}

fn idx4_koid(i: usize) -> KOID {
    let b = (i as u64).to_le_bytes();
    KOID::from_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[0], b[1], b[2], b[3], b[4], b[5], b[6],
        b[7],
    ])
}

/// The cost cells behind the rule-11 re-key decision: upsert at HIGH
/// cardinality (the per-upsert all-bucket sweep — O(buckets) every call) and
/// scan_eq at LOW cardinality (the per-scan bucket sort). Correctness-pinned
/// at every size. 1k always runs; the scale cells run via
/// AIKOQL_PROPIDX_CELLS=<n> in release (debug-mode n² sweeps are slow).
#[test]
fn idx4_005_property_index_cost_cells() {
    let sizes: Vec<usize> = match std::env::var("AIKOQL_PROPIDX_CELLS") {
        Ok(v) => vec![1000, v.parse().unwrap()],
        Err(_) => vec![1000],
    };
    for n in sizes {
        // High cardinality: n distinct titles — every upsert sweeps the
        // whole map looking for the koid's old key.
        let hi = PropertyIndex::new("hi", "note", &["title"]);
        let t0 = std::time::Instant::now();
        for i in 0..n {
            hi.upsert(idx4_koid(i), &idx4_ko(idx4_koid(i), &format!("t{i}")))
                .unwrap();
        }
        let hi_upsert = t0.elapsed();
        assert_eq!(hi.len(), n, "no drop at {n}");
        let one = hi.scan_eq(&[Value::Text("t7".into())]).unwrap();
        assert_eq!(one, vec![idx4_koid(7)], "exact answer at {n}");

        // Low cardinality: every row shares one title — scan_eq sorts n.
        let lo = PropertyIndex::new("lo", "note", &["title"]);
        for i in 0..n {
            lo.upsert(idx4_koid(i), &idx4_ko(idx4_koid(i), "same"))
                .unwrap();
        }
        assert_eq!(lo.len(), n);
        let t0 = std::time::Instant::now();
        let all = lo.scan_eq(&[Value::Text("same".into())]).unwrap();
        let lo_scan = t0.elapsed();
        assert_eq!(all.len(), n, "every row answers at {n}");

        println!(
            "idx4-005 cell n={n}: upsert(hi-card) {:.3}s, scan_eq(lo-card) {:.3}s",
            hi_upsert.as_secs_f64(),
            lo_scan.as_secs_f64()
        );
    }
}

/// The rule-11 decision support: the cells are RECORDED in the testing
/// plan's M24 row before the reverse-map re-key is judged. Pre-impl the row
/// is ⬜ — this needle fails until the cells ran and the numbers landed.
#[test]
fn idx4_005_cells_recorded_in_the_testing_plan() {
    let plan = std::fs::read_to_string(format!(
        "{}/../../docs/TESTING-PLAN-PHASE5.md",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let m24 = plan
        .lines()
        .find(|l| l.starts_with("| P5-M24"))
        .expect("P5-M24 row");
    assert!(
        m24.contains("1k") && m24.contains("100k") && m24.contains("1M"),
        "the idx4-005 cells must be recorded in the M24 row: {m24}"
    );
}
