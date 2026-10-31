//! AG-13 — memory consolidation: near-duplicate KOs are merged by a
//! background job so the surface stays small without losing provenance.
//! The merge derives a new KO with `derived_from` edges to every source
//! (AG-13a: lineage kept) and deletes nothing — sources stay readable and
//! the journal records no Forgotten (AG-13c: only `forget` deletes).
//!
//! RED: `aikoql_scheduler::consolidate` and `ConsolidationJob` do not exist.

use aikoql_kernel::transaction::kernel::{KnowledgeContext, RememberRequest, Subject};
use aikoql_kernel::*;
use aikoql_scheduler::{consolidate, ConsolidationJob};
use std::sync::Arc;
use std::time::Duration;

/// Property-similarity gate for near-duplicates (0.8 = conservative).
const THRESHOLD: f64 = 0.8;

/// The job's subject: an unscoped admin — the only principal the kernel
/// lets read across tenants (auth T-55).
fn subject() -> Subject {
    Subject::with_roles("consolidation", &["admin"])
}

fn open(tag: &str) -> Kernel {
    let dir = std::env::temp_dir().join(format!("ag13-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let engine =
        Arc::new(aikoql_storage_v2::AikoqlStorageEngineV2::open(dir.to_str().unwrap()).unwrap());
    Kernel::open(engine, Arc::new(SystemClock), 0xA113).unwrap()
}

fn note(k: &Kernel, body: &str, tag: &str) -> KOID {
    let mut req = RememberRequest::create(
        KnowledgeContext::new(Subject::new("ag13")),
        Metadata {
            type_name: "note".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("body".into(), Value::Text(body.into()));
    req.properties.insert("tag".into(), Value::Text(tag.into()));
    k.remember(req).unwrap().koid
}

#[test]
fn ag13a_lineage_kept() {
    let k = open("lineage");
    let a = note(&k, "sally works at acme corp as staff engineer", "who");
    let b = note(&k, "sally works at acme corp as a staff engineer", "who");
    let c = note(&k, "the office moves to the harbor in june", "where");
    let merged = consolidate(&k, &subject(), THRESHOLD).unwrap();
    assert_eq!(
        merged.len(),
        1,
        "one near-duplicate pair, one merge: {merged:?}"
    );
    // The merged KO carries the sources as inbound derived_from edges
    // (the index direction is source → derived).
    let inn = k
        .inbound_edges(&merged[0], Some(DERIVED_FROM))
        .unwrap_or_else(|e| panic!("no derived_from edges on the merged KO: {e}"));
    let mut sources: Vec<String> = inn.into_iter().map(|(_, id)| id.to_hex()).collect();
    sources.sort();
    assert_eq!(
        sources,
        vec![a.to_hex(), b.to_hex()],
        "the merged KO traces to both sources"
    );
    assert!(
        !sources.contains(&c.to_hex()),
        "the distinct note must not be a source"
    );
    // NewestWins: the merged properties carry the newest body.
    let merged_ko = k.get(KnowledgeContext::new(subject()), &merged[0]).unwrap();
    assert_eq!(
        merged_ko.properties.get("body"),
        Some(&Value::Text(
            "sally works at acme corp as a staff engineer".into()
        ))
    );
}

#[test]
fn ag13c_nothing_deleted() {
    let k = open("undelete");
    let a = note(&k, "sally works at acme corp as staff engineer", "who");
    let b = note(&k, "sally works at acme corp as a staff engineer", "who");
    let c = note(&k, "the office moves to the harbor in june", "where");
    let merged = consolidate(&k, &subject(), THRESHOLD).unwrap();
    assert_eq!(merged.len(), 1);
    let ctx = || KnowledgeContext::new(subject());
    for id in [a, b, c] {
        k.get(ctx(), &id)
            .unwrap_or_else(|e| panic!("{id} deleted by consolidation: {e}"));
    }
    let forgotten = k
        .journal()
        .unwrap()
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Forgotten))
        .count();
    assert_eq!(forgotten, 0, "only `forget` deletes");
    assert_eq!(
        k.scan_by_type(&subject(), "note").unwrap().len(),
        4,
        "3 sources + 1 merged — nothing removed"
    );
    // A second run must not merge the same pair again: the sources are
    // already derived_from sources (no duplicate merges, no growth).
    assert_eq!(
        consolidate(&k, &subject(), THRESHOLD).unwrap().len(),
        0,
        "consolidation is idempotent"
    );
}

#[test]
fn ag13_job_consolidates_on_cadence() {
    let k = open("job");
    let job = ConsolidationJob::start(&k, Duration::from_millis(50)).unwrap();
    note(&k, "sally works at acme corp as staff engineer", "who");
    note(&k, "sally works at acme corp as a staff engineer", "who");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while k.scan_by_type(&subject(), "note").unwrap().len() != 3
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    job.shutdown();
    assert_eq!(
        k.scan_by_type(&subject(), "note").unwrap().len(),
        3,
        "the job merged the near-duplicates on its cadence"
    );
}
