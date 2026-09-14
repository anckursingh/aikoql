//! P5-M9 (ND-08) — statistics collection. cbo_a01–a05.
//!
//! The statistics subsystem feeds the cost-based optimizer: `analyze` walks
//! the canonical heads (the same reconciliation walk the M8 verify/rebuild
//! use), computes the roadmap's statistics list — type cardinality, property
//! cardinality (distinct counts), selectivity, relationship degree/fanout,
//! vector candidate density, temporal density, tenant distribution — and
//! persists them as ordinary catalog rows (kind `statistics`, name = type
//! name; M7 made statistics one of the entities M8/M9 consume
//! transactionally). A watermark (the journal length at capture) drives
//! staleness: any later journal event makes the row stale, and a stale row
//! must never silently drive a plan (cbo008, runtime).
//!
//! Observable contract pinned here:
//! - cbo_a01 analyze computes every statistic exactly
//! - cbo_a02 statistics persist across restart; re-analyze updates in place
//! - cbo_a03 the watermark tracks the journal (fresh at capture, stale after
//!   any later event, refreshed by re-analysis)
//! - cbo_a04 a corrupt statistics row fails the read closed (ct003 pattern)
//! - cbo_a05 never-analyzed types read back None; an empty type analyzes to
//!   row_count 0

use aikoql_kernel::transaction::kernel::{KnowledgeContext, RememberRequest, Subject};
use aikoql_kernel::*;
use std::sync::Arc;

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

/// Remember an Employee named `name` (dept, salary); returns its KOID.
fn emp(k: &Kernel, name: &str, dept: &str, salary: i64) -> KOID {
    let mut req = RememberRequest::create(alice(), meta("Employee"));
    req.properties
        .insert("name".into(), Value::Text(name.into()));
    req.properties
        .insert("dept".into(), Value::Text(dept.into()));
    req.properties.insert("salary".into(), Value::Int(salary));
    k.remember(req).unwrap().koid
}

// --- cbo_a01 — analyze computes the roadmap statistics --------------------------

#[test]
fn cbo_a01_analyze_computes_every_statistic_exactly() {
    let k = mk();
    let dept = {
        let mut req = RememberRequest::create(alice(), meta("Department"));
        req.properties.insert("id".into(), Value::Int(1));
        k.remember(req).unwrap().koid
    };
    // Two of three employees have an outbound "works-in" edge.
    let mut edges = 0usize;
    for (n, d, s) in [("A", "Eng", 100), ("B", "Eng", 120), ("C", "Ops", 90)] {
        let id = emp(&k, n, d, s);
        if edges < 2 {
            let mut req = RememberRequest::update(alice(), id, meta("Employee"));
            // Kernel update semantics REPLACE the property map (only
            // extensions carry forward), so the update restates them.
            req.properties.insert("name".into(), Value::Text(n.into()));
            req.properties.insert("dept".into(), Value::Text(d.into()));
            req.properties.insert("salary".into(), Value::Int(s));
            req.relationships.push(aikoql_kernel::RelationshipRef {
                rel_type: "works-in".into(),
                target: dept,
                direction: aikoql_kernel::Direction::Outbound,
            });
            k.remember(req).unwrap();
            edges += 1;
        }
    }
    // One row carries temporal bounds (extensions — the kom-level storage).
    {
        let mut req = RememberRequest::create(alice(), meta("Employee"));
        req.properties
            .insert("name".into(), Value::Text("D".into()));
        req.extensions
            .insert("valid_from".into(), Value::Int(10_000));
        k.remember(req).unwrap();
    }
    // One row carries a vector embedding.
    {
        let mut req = RememberRequest::create(alice(), meta("Employee"));
        req.properties
            .insert("name".into(), Value::Text("E".into()));
        req.semantic = Some(SemanticBlock {
            embedding_model: Some("bge-m3".into()),
            embedding: Some(vec![0.1, 0.2]),
            confidence: None,
            source: None,
            summary: None,
        });
        k.remember(req).unwrap();
    }

    let stats = k.analyze("Employee").unwrap();
    assert_eq!(stats.row_count, 5, "type cardinality");
    assert_eq!(
        stats.cardinality("name").unwrap(),
        5,
        "property cardinality = distinct values"
    );
    assert_eq!(stats.cardinality("dept").unwrap(), 2);
    assert!(
        (stats.selectivity("dept") - 0.5).abs() < 1e-9,
        "selectivity = match fraction 1/distinct (uniform)"
    );
    assert!(
        (stats.fanout - 2.0 / 5.0).abs() < 1e-9,
        "avg outbound degree"
    );
    assert!((stats.vector_density - 1.0 / 5.0).abs() < 1e-9);
    assert!((stats.temporal_density - 1.0 / 5.0).abs() < 1e-9);
    assert_eq!(stats.tenant_count, 1, "one distinct tenant (absent)");
    // Fresh at capture: the stats row's own create event sits at watermark-1,
    // so the journal at capture is NOT stale.
    assert!(!stats.is_stale(k.journal().unwrap().len() as u64));
}

// --- cbo_a02 — persistence + re-analysis ---------------------------------------

#[test]
fn cbo_a02_statistics_persist_and_reanalyze_updates_in_place() {
    let (k, engine) = mk_shared();
    emp(&k, "A", "Eng", 100);
    emp(&k, "B", "Ops", 90);
    k.analyze("Employee").unwrap();

    let k2 = reopen(&engine).unwrap();
    let stats = k2.statistics("Employee").unwrap().expect("persisted row");
    assert_eq!(stats.row_count, 2);
    assert_eq!(stats.cardinality("dept").unwrap(), 2);

    // Re-analysis after more writes updates the SAME (kind, name) row.
    emp(&k2, "C", "Eng", 110);
    let stats2 = k2.analyze("Employee").unwrap();
    assert_eq!(stats2.row_count, 3);
    let k3 = reopen(&engine).unwrap();
    assert_eq!(k3.statistics("Employee").unwrap().unwrap().row_count, 3);
}

// --- cbo_a03 — staleness tracks the journal -------------------------------------

#[test]
fn cbo_a03_watermark_tracks_the_journal() {
    let k = mk();
    emp(&k, "A", "Eng", 100);
    k.analyze("Employee").unwrap();
    let fresh_len = k.journal().unwrap().len() as u64;
    assert!(
        !k.statistics("Employee")
            .unwrap()
            .unwrap()
            .is_stale(fresh_len),
        "fresh at capture"
    );
    emp(&k, "B", "Ops", 90); // any later event ⇒ stale
    let now_len = k.journal().unwrap().len() as u64;
    assert!(
        k.statistics("Employee").unwrap().unwrap().is_stale(now_len),
        "a later journal event makes the row stale"
    );
    // Re-analysis refreshes the watermark.
    let fresh = k.analyze("Employee").unwrap();
    assert!(!fresh.is_stale(k.journal().unwrap().len() as u64));
}

// --- cbo_a04 — corruption fails the read closed ---------------------------------

#[test]
fn cbo_a04_corrupt_statistics_row_fails_closed() {
    let k = mk();
    // A foreign writer plants a statistics row whose row_count is not an Int.
    let mut req = RememberRequest::create(
        KnowledgeContext::new(Subject::new("aikoql:system").in_tenant("aikoql:catalog")),
        Metadata {
            type_name: "aikoql:catalog".into(),
            tenant: Some("aikoql:catalog".into()),
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("kind".into(), Value::Text("statistics".into()));
    req.properties
        .insert("name".into(), Value::Text("Employee".into()));
    req.properties
        .insert("row_count".into(), Value::Text("garbage".into()));
    k.remember(req).unwrap();

    let err = match k.statistics("Employee") {
        Ok(_) => panic!("a corrupt statistics row must fail the read"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(msg.contains("statistics"), "the error names the row: {msg}");
}

// --- cbo_a05 — absent vs empty --------------------------------------------------

#[test]
fn cbo_a05_never_analyzed_reads_none_and_empty_analyzes_to_zero() {
    let k = mk();
    assert!(k.statistics("Employee").unwrap().is_none());
    let stats = k.analyze("Employee").unwrap();
    assert_eq!(stats.row_count, 0, "an empty type analyzes to zero rows");
    assert!(
        stats.selectivity("dept") == 0.0,
        "no distincts ⇒ no selectivity"
    );
    assert!(stats.fanout == 0.0 && stats.vector_density == 0.0);
    assert!(!stats.is_stale(k.journal().unwrap().len() as u64));
}
