//! P5-M15 — CBO on the default KOQL query path. cbo_default_001–004.
//!
//! The default `Interpreter::execute` path (every caller: MCP tool, shell,
//! SDK, certification) cost-optimizes before executing, guarded by M9's
//! gates — only a covering, verified-clean, fresh-stats property index over
//! the first Eq filter is selected, and only when strictly cheaper. Every
//! other plan (incl. any kernel without declared indexes or stats) executes
//! byte-identical to the pre-M15 path.
//!
//! Detection power: the index-assisted path is result-identical to the full
//! scan by construction (Eq never matches a missing property; a clean index
//! holds exactly the committed truth), so the plan decision is observed via
//! the `execute_with_report` seam — this suite fails to COMPILE before the
//! M15 surface exists (E0599, the missing-seam RED). 002/003 are guard pins
//! that fail a BROKEN wiring, not the pre-impl state: an execute() that
//! skipped the verify/staleness gates would drop the unseen row (002), and
//! an optimizer that rewrote a no-index kernel would change the plan (003).
//! 004 pins gate 6 through the default path once `run_costed`'s baseline is
//! the rule physicalization.

use aikoql_compiler::parser;
use aikoql_kernel::ir::{IrPlan, PhysicalPlan, Strategy};
use aikoql_kernel::transaction::kernel::{KnowledgeContext, RememberRequest, Subject};
use aikoql_kernel::*;
use aikoql_runtime::plan_oracle::{run_costed, OracleEntry};
use aikoql_runtime::Interpreter;
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

fn person(k: &Kernel, name: &str, dept: &str) -> KOID {
    let mut req = RememberRequest::create(alice(), meta("Person"));
    req.properties
        .insert("name".into(), Value::Text(name.into()));
    req.properties
        .insert("dept".into(), Value::Text(dept.into()));
    k.remember(req).unwrap().koid
}

/// Feed every live row of `Person` into every property index — the
/// maintainer catch-up (the runtime tests carry no scheduler; idx2-008).
fn catch_up_indexes(k: &Kernel) {
    for (id, _v, ts, state) in k.scan_heads().unwrap() {
        if state == LifecycleState::Deleted {
            continue;
        }
        if let Some(ko) = k.raw_object_at(&id, ts).unwrap() {
            if ko.metadata.type_name == "Person" {
                for idx in k.property_indexes().unwrap() {
                    idx.upsert(id, &ko).unwrap();
                }
            }
        }
    }
}

/// koids of the executed rows, for row-for-row (order-preserving) equality.
fn exec_ids(rows: &aikoql_runtime::RowSet) -> Vec<KOID> {
    match rows {
        aikoql_runtime::RowSet::Objects(kos) => kos.iter().map(|ko| ko.koid).collect(),
        other => panic!("expected Objects, got {other:?}"),
    }
}

fn plan_of(_k: &Kernel, query: &str) -> IrPlan {
    parser::compile_with_subject(query, "alice").unwrap()
}

/// The pre-M15 path, verbatim: physicalize by the rules, execute directly.
fn rule_based(k: &Kernel, plan: &IrPlan) -> Vec<KOID> {
    exec_ids(
        &Interpreter::execute_physical(k, &PhysicalPlan::from_ops(plan.operators.clone())).unwrap(),
    )
}

// --- cbo_default_001 — the default path selects the index and stays equivalent ---

#[test]
fn cbo_default_001_indexed_fresh_kernel_uses_the_index_on_the_default_path() {
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..100 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k);
    k.analyze("Person").unwrap();

    let query = "MATCH Person WHERE name == \"P042\" RETURN *";
    let plan = plan_of(&k, query);
    let (rows, report) = Interpreter::execute_with_report(&k, &plan).unwrap();
    assert!(report.stats_used, "fresh stats drive the decision");
    assert_eq!(
        report.plan.operators[0].strategy,
        Strategy::PropertyIndex,
        "the default path executes the index-assisted plan"
    );
    assert_eq!(report.index_used.as_deref(), Some("by_name"));
    // Row-for-row identical to the rule path — same rows, same order.
    assert_eq!(exec_ids(&rows), rule_based(&k, &plan));
}

// --- cbo_default_002 — the guards hold on the default path ----------------------

#[test]
fn cbo_default_002_lagging_or_stale_index_falls_back_on_the_default_path() {
    // An index that has not caught up (stats fresh, index lagging): the
    // default path must NOT use it — the unseen row answers the truth.
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..10 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k);
    let unseen = person(&k, "Unseen", "Eng"); // created after catch-up
    k.analyze("Person").unwrap();

    let query = "MATCH Person WHERE name == \"Unseen\" RETURN *";
    let plan = plan_of(&k, query);
    let (rows, report) = Interpreter::execute_with_report(&k, &plan).unwrap();
    assert!(
        report.stats_used,
        "stats fresh — the index is the rejected piece"
    );
    assert_eq!(report.plan.operators[0].strategy, Strategy::FullScan);
    assert_eq!(exec_ids(&rows), vec![unseen], "not silently wrong");

    // Stale stats (a write after analyze): same fallback on the default path.
    let k2 = mk();
    k2.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..100 {
        person(&k2, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k2);
    k2.analyze("Person").unwrap();
    let late = person(&k2, "Late", "Eng");
    let query = "MATCH Person WHERE name == \"P042\" RETURN *";
    let plan = plan_of(&k2, query);
    let (rows, report) = Interpreter::execute_with_report(&k2, &plan).unwrap();
    assert!(report.stats_stale, "staleness is detected");
    assert!(!report.stats_used, "stale stats never drive the plan");
    assert_eq!(report.plan.operators[0].strategy, Strategy::FullScan);
    let ids = exec_ids(&rows);
    assert!(
        ids.iter().all(|id| *id != late),
        "the stale path returns the committed truth, not index artifacts"
    );
    assert_eq!(ids.len(), 1, "exactly P042 — the rule-based answer");
    let _ = late;
}

// --- cbo_default_003 — no declared indexes: byte-identical to pre-M15 -----------

#[test]
fn cbo_default_003_no_declared_indexes_executes_byte_identical_to_pre_m15() {
    let k = mk();
    for i in 0..20 {
        person(
            &k,
            &format!("P{i:03}"),
            if i % 2 == 0 { "Eng" } else { "Ops" },
        );
    }
    let query = "MATCH Person WHERE dept == \"Eng\" RETURN *";
    let plan = plan_of(&k, query);
    let (rows, report) = Interpreter::execute_with_report(&k, &plan).unwrap();
    // The plan is byte-identical to the pre-M15 physicalization.
    let rule_plan = PhysicalPlan::from_ops(plan.operators.clone());
    assert_eq!(
        report.plan, rule_plan,
        "the optimizer is a no-op without indexes"
    );
    assert!(
        report
            .plan
            .operators
            .iter()
            .all(|po| po.strategy != Strategy::PropertyIndex),
        "no index was selected"
    );
    assert!(
        !report.stats_used && !report.stats_stale,
        "no stats, not stale"
    );
    assert_eq!(exec_ids(&rows), rule_based(&k, &plan));
}

// --- cbo_default_004 — gate-6 oracle through the default path -------------------

#[test]
fn cbo_default_004_gate6_oracle_divergence_zero_through_the_default_path() {
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..50 {
        person(
            &k,
            &format!("P{i:03}"),
            if i % 2 == 0 { "Eng" } else { "Ops" },
        );
    }
    catch_up_indexes(&k);
    k.analyze("Person").unwrap();

    let q = |s: &str| OracleEntry {
        id: s.into(),
        baseline: plan_of(&k, s),
        candidate: plan_of(&k, s),
    };
    let entries = vec![
        q("MATCH Person WHERE name == \"P042\" RETURN *"), // index-assisted
        q("MATCH Person WHERE dept == \"Eng\" RETURN *"),  // full scan
        q("MATCH Person RETURN *"),                        // no filter
        q("MATCH Person WHERE dept == \"Eng\" RETURN name, dept ORDER BY name LIMIT 10"),
        q("MATCH Person WHERE dept == \"Eng\" RETURN dept GROUP BY dept"),
        q("MATCH Person AS_OF 15000 RETURN *"),
    ];
    let report = run_costed(&k, &entries);
    assert_eq!(
        report.divergences,
        Vec::<String>::new(),
        "the default path never changes results (gate-6)"
    );
}
