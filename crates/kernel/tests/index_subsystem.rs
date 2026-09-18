//! P5-M8 (ND-07) — the unified index subsystem, kernel side. idx2-001..010.
//!
//! The `Index` trait is the one database-level index abstraction over
//! catalog-registered indexes (P5-M7 rows of kind "index"): property and
//! composite hash indexes ship here (the vector/text engines become
//! implementations too — exercised through the scheduler's maintainer).
//! These tests pin the kernel half: catalog create/drop/list sugar, the
//! live registry, equality scans, online rebuild, the verify checker
//! (stale entries vs the store), and fail-closed corruption.
//!
//! Honest-ledger: relationship/temporal/provenance index types stay
//! catalog-schema-only (the pre-declared non-goal row).

use aikoql_kernel::{
    ForgetMode, Index, Kernel, KnowledgeContext, ManualClock, MemoryEngine, Metadata, PropertyMap,
    RememberRequest, Subject, Value, KOID, KOID_LEN,
};
use std::sync::Arc;

// --- helpers ------------------------------------------------------------------

fn mk() -> Kernel {
    Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0x1D3C,
    )
    .unwrap()
}

fn alice() -> Subject {
    Subject::new("alice")
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

/// Remember a note with the given `body`; returns its KOID.
fn note(k: &Kernel, body: &str) -> KOID {
    let mut req = RememberRequest::create(alice(), meta("note"));
    req.properties
        .insert("body".into(), Value::Text(body.into()));
    k.remember(req).unwrap().koid
}

/// The live registry entry for `name` (populated from the catalog at open).
fn find(k: &Kernel, name: &str) -> Arc<dyn Index> {
    k.property_indexes()
        .unwrap()
        .into_iter()
        .find(|i| i.name() == name)
        .expect("index registered in the live registry")
}

// --- idx2-001 — create ----------------------------------------------------------

#[test]
fn idx2_001_index_create_lists_and_dedupes() {
    let k = mk();
    k.catalog_create_index("by_body", "note", &["body"])
        .unwrap();
    // composite: a two-column key
    k.catalog_create_index("by_author_year", "note", &["author", "year"])
        .unwrap();

    let mut names: Vec<String> = k
        .catalog_list_indexes()
        .unwrap()
        .into_iter()
        .map(|d| d.name)
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["by_author_year".to_string(), "by_body".to_string()],
        "every catalog-registered index is listed"
    );

    // the decl payload round-trips
    let decl = k
        .catalog_list_indexes()
        .unwrap()
        .into_iter()
        .find(|d| d.name == "by_author_year")
        .unwrap();
    assert_eq!(decl.type_name, "note");
    assert_eq!(
        decl.properties,
        vec!["author".to_string(), "year".to_string()]
    );

    // duplicate (kind, name) fails closed
    assert!(
        k.catalog_create_index("by_body", "note", &["body"])
            .is_err(),
        "a duplicate index name must fail closed"
    );
}

// --- idx2-002 — drop ------------------------------------------------------------

#[test]
fn idx2_002_index_drop_removes_registry_and_scans_fail_closed() {
    let k = mk();
    k.catalog_create_index("by_body", "note", &["body"])
        .unwrap();
    k.catalog_drop_index("by_body").unwrap();

    assert!(
        k.catalog_list_indexes().unwrap().is_empty(),
        "a dropped index leaves the catalog"
    );
    assert!(
        k.catalog_drop_index("by_body").is_err(),
        "dropping an unknown index fails closed"
    );
    assert!(
        k.scan_index("by_body", &[Value::Text("x".into())]).is_err(),
        "the live registry no longer answers a dropped index"
    );
}

// --- composite scan (property + composite index types) ---------------------------

#[test]
fn idx2_composite_scan_matches_the_full_key() {
    let k = mk();
    k.catalog_create_index("by_author_year", "note", &["author", "year"])
        .unwrap();

    let mut req = RememberRequest::create(alice(), meta("note"));
    req.properties
        .insert("author".into(), Value::Text("alice".into()));
    req.properties.insert("year".into(), Value::Int(2026));
    let id = k.remember(req).unwrap().koid;

    let ko = k.get(alice(), &id).unwrap();
    let idx = find(&k, "by_author_year");
    idx.upsert(id, &ko).unwrap();

    let got = k
        .scan_index(
            "by_author_year",
            &[Value::Text("alice".into()), Value::Int(2026)],
        )
        .unwrap();
    assert_eq!(got, vec![id], "the full composite key matches");

    let wrong = k
        .scan_index(
            "by_author_year",
            &[Value::Text("alice".into()), Value::Int(2027)],
        )
        .unwrap();
    assert!(
        wrong.is_empty(),
        "a partial key never matches a composite index"
    );

    // a key with the wrong arity fails closed
    assert!(
        k.scan_index("by_author_year", &[Value::Text("alice".into())])
            .is_err(),
        "key arity mismatches must fail closed"
    );
}

// --- idx2-006 — online rebuild ----------------------------------------------------

#[test]
fn idx2_006_online_rebuild_reconciles_from_the_store() {
    let k = mk();
    k.catalog_create_index("by_body", "note", &["body"])
        .unwrap();
    let a = note(&k, "cats");
    let idx = find(&k, "by_body");

    // seed one real entry plus a stale one the store does not back
    let ko = k.get(alice(), &a).unwrap();
    idx.upsert(a, &ko).unwrap();
    idx.upsert(KOID([0xEE; KOID_LEN]), &ko).unwrap();
    assert_eq!(idx.len(), 2, "real + garbage entries before rebuild");

    idx.rebuild(&k).unwrap();
    assert_eq!(
        idx.len(),
        1,
        "rebuild drops entries the store does not back"
    );
    let got = k
        .scan_index("by_body", &[Value::Text("cats".into())])
        .unwrap();
    assert_eq!(got, vec![a], "rebuild answers exactly the stored rows");
}

// --- idx2-009 — stale entries + consistency verification ---------------------------

#[test]
fn idx2_009_verify_reports_stale_and_missing_then_rebuild_heals() {
    let k = mk();
    k.catalog_create_index("by_body", "note", &["body"])
        .unwrap();
    let a = note(&k, "cats");
    let b = note(&k, "dogs");
    let c = note(&k, "birds");
    let idx = find(&k, "by_body");

    // a is indexed; b never was (missing); a garbage koid nobody backs
    // (stale); c is indexed then tombstoned (stale by deletion)
    let ko_a = k.get(alice(), &a).unwrap();
    idx.upsert(a, &ko_a).unwrap();
    idx.upsert(KOID([0xEE; KOID_LEN]), &ko_a).unwrap();
    let ko_c = k.get(alice(), &c).unwrap();
    idx.upsert(c, &ko_c).unwrap();
    k.forget(alice(), &c, ForgetMode::Tombstone, None, None)
        .unwrap();

    let rep = idx.verify(&k).unwrap();
    assert!(rep.verified, "the report is a real store reconciliation");
    assert!(rep.missing.contains(&b), "an unindexed live row is missing");
    assert!(
        rep.stale.contains(&KOID([0xEE; KOID_LEN])),
        "an entry without a live head is stale"
    );
    assert!(
        rep.stale.contains(&c),
        "a tombstoned head left in the index is stale"
    );
    assert!(
        !rep.stale.contains(&a),
        "the good entry is neither stale nor missing"
    );
    assert!(
        !rep.missing.contains(&a),
        "the good entry is neither stale nor missing"
    );

    // rebuild heals both directions
    idx.rebuild(&k).unwrap();
    let rep = idx.verify(&k).unwrap();
    assert!(rep.missing.is_empty(), "rebuild closes the missing gap");
    assert!(rep.stale.is_empty(), "rebuild closes the stale gap");
}

// --- idx2-010 — corruption ---------------------------------------------------------

#[test]
fn idx2_010_corrupt_index_row_fails_open_closed() {
    let engine = Arc::new(MemoryEngine::new());
    let clock = Arc::new(ManualClock::new(10_000));
    let k1 = Kernel::open(engine.clone(), clock.clone(), 0x1D3C).unwrap();

    // a catalog index row with a garbage payload — written as the system
    // principal (the row owner, so the default ACL lets the write through)
    let mut props = PropertyMap::new();
    props.insert("kind".into(), Value::Text("index".into()));
    props.insert("name".into(), Value::Text("bad".into()));
    props.insert("properties".into(), Value::Text("not-a-list".into()));
    let mut req = RememberRequest::create(
        KnowledgeContext::new(Subject::new("aikoql:system").in_tenant("aikoql:catalog")),
        Metadata {
            type_name: "aikoql:catalog".into(),
            tenant: Some("aikoql:catalog".into()),
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties = props;
    k1.remember(req).unwrap();
    drop(k1);

    // the corrupt row must fail the NEXT open closed — never silently ignored
    let k2 = Kernel::open(engine, clock, 0x1D3C);
    assert!(k2.is_err(), "a corrupt index row must fail the open closed");
}

// --- P5-M26 — idx5-001 --------------------------------------------------------------
//
// rebuild is O(committed heads). The SDK re-declares the same-shape index on
// every connect (the M17b contract), so at 1M rows every open pays the full
// reseed. A caught-up rebuild must be a no-op; a stale index still rebuilds.

/// True when the park hook fired before the deadline (the hook stamps
/// INDEX_REBUILD_PARK_AT first thing inside the rebuild).
fn idx5_parked(deadline_secs: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(deadline_secs);
    while std::env::var_os("INDEX_REBUILD_PARK_AT").is_none() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    true
}

#[test]
fn idx5_001_caught_up_rebuild_is_a_noop_stale_still_rebuilds() {
    let k = mk();
    // a name unique to this test: the park hook is name-keyed, and env vars
    // are process-global under parallel test binaries.
    k.catalog_create_index("by_body_idx5", "note", &["body"])
        .unwrap();
    note(&k, "hello");
    k.rebuild_index("by_body_idx5").unwrap(); // fresh: stamp == head
    assert_eq!(
        k.index_applied_seq("by_body_idx5").unwrap(),
        k.journal_head().unwrap().0,
        "precondition: the index is caught up"
    );

    std::env::remove_var("INDEX_REBUILD_PARK_AT");
    std::env::set_var("INDEX_REBUILD_PARK", "by_body_idx5");
    k.rebuild_index("by_body_idx5").unwrap();
    assert!(
        !idx5_parked(2),
        "a caught-up rebuild_index must not re-run the O(heads) rebuild"
    );
    std::env::remove_var("INDEX_REBUILD_PARK");

    // A stale index (a commit landed with no apply) still rebuilds.
    note(&k, "world");
    std::env::remove_var("INDEX_REBUILD_PARK_AT");
    std::env::set_var("INDEX_REBUILD_PARK", "by_body_idx5");
    k.rebuild_index("by_body_idx5").unwrap();
    assert!(idx5_parked(2), "a stale index must still rebuild");
    std::env::remove_var("INDEX_REBUILD_PARK");
    std::env::remove_var("INDEX_REBUILD_PARK_AT");
    assert_eq!(
        k.scan_index("by_body_idx5", &[Value::Text("world".into())])
            .unwrap()
            .len(),
        1,
        "the stale rebuild heals the index"
    );
}
