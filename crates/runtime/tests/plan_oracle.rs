//! P5-M0 — plan-equivalence oracle harness (gate 6).
//!
//! gd002: the oracle must show divergence 0 for plans that are equivalent by
//! construction — the planner's `optimize` over compiled queries (identity
//! today), and the dedup rewrite vs a hand-built reference plan.
//! gd003: detection power — an evil rewrite (constant flip in a predicate)
//! must be caught with divergence > 0.
//!
//! The oracle itself lives in `aikoql_runtime::plan_oracle`; this suite is the
//! consumer pin that P5-M1+ milestones keep extending (one oracle row per
//! milestone whose RED list touches the planner — TESTING-PLAN-PHASE5 rule 8).

use std::sync::Arc;

use aikoql_compiler::parser::compile_with_subject;
use aikoql_compiler::planner::Planner;
use aikoql_kernel::ir::{IrOp, IrPlan, Predicate};
use aikoql_kernel::{
    ExtensionMap, Kernel, ManualClock, MemoryEngine, Metadata, Origin, PropertyMap,
    ReferentialPolicy, RememberRequest, Subject, Value,
};
use aikoql_runtime::plan_oracle::{run, OracleEntry};

fn mk() -> Kernel {
    let clock = Arc::new(ManualClock::new(20_000));
    Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xCAFE).unwrap()
}

fn create_ko(k: &Kernel, subj: &Subject, type_name: &str, props: PropertyMap) {
    k.remember(RememberRequest {
        context: subj.into(),
        koid: None,
        expected_version: Some(0),
        idempotency_key: None,
        metadata: Metadata {
            type_name: type_name.into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
        properties: props,
        semantic: None,
        relationships: vec![],
        security: None,
        extensions: ExtensionMap::new(),
        origin: Origin::Human,
        note: None,
        referential_policy: ReferentialPolicy::default(),
    })
    .unwrap();
}

/// Two `fact`s with kind "hot", one "cold", one `event` "hot" — so a constant
/// flip in the predicate changes the row count (2 vs 1) while a type-level
/// query (`event`) stays structurally distinct.
///
/// Text predicates: numeric literals compile to `Value::Float`
/// (parser/mod.rs), which the runtime cannot compare against `Int`-seeded
/// properties — a pre-existing compiler defect, recorded for P5-M2.
fn seeded() -> Kernel {
    let k = mk();
    let alice = Subject::new("alice");
    let kind = |k: &str| -> PropertyMap {
        PropertyMap::from([("kind".to_string(), Value::Text(k.into()))])
    };
    create_ko(&k, &alice, "fact", kind("hot"));
    create_ko(&k, &alice, "fact", kind("hot"));
    create_ko(&k, &alice, "fact", kind("cold"));
    create_ko(&k, &alice, "event", kind("hot"));
    k
}

fn scan(type_name: &str, subject: &str) -> IrOp {
    IrOp::Scan {
        type_name: type_name.into(),
        subject: subject.into(),
        roles: vec![],
        tenant: None,
    }
}

#[test]
fn gd002_optimize_is_equivalence_preserving() {
    // Gate 6 invariant: the planner never changes the result set. Compiled
    // queries emit exactly one Scan, so `optimize` is identity today — the
    // oracle pins the invariant for when it stops being identity.
    let k = seeded();
    let queries = [
        "MATCH fact WHERE kind == \"hot\" RETURN *",
        "MATCH event RETURN *",
    ];
    let entries: Vec<OracleEntry> = queries
        .iter()
        .map(|q| {
            let raw = compile_with_subject(q, "alice").unwrap();
            OracleEntry {
                id: (*q).to_string(),
                baseline: raw.clone(),
                candidate: Planner::optimize(&raw),
            }
        })
        .collect();
    let report = run(&k, &entries);
    assert_eq!(
        report.divergences,
        Vec::<String>::new(),
        "optimized plan must be equivalent to the raw plan (gate 6)"
    );
}

#[test]
fn gd002b_dedup_scans_matches_reference() {
    // The planner's real rewrite: two consecutive identical Scans collapse to
    // one. The candidate (deduped two-scan plan) must match the hand-built
    // single-scan reference exactly.
    let k = seeded();
    let dup = IrPlan::new(vec![scan("fact", "alice"), scan("fact", "alice")]);
    let reference = IrPlan::new(vec![scan("fact", "alice")]);
    let report = run(
        &k,
        &[OracleEntry {
            id: "dedup-two-identical-scans".into(),
            baseline: reference,
            candidate: Planner::optimize(&dup),
        }],
    );
    assert_eq!(
        report.divergences,
        Vec::<String>::new(),
        "deduped plan must match the reference plan"
    );
}

#[test]
fn gd003_evil_rewrite_diverges() {
    // Detection power: flip a constant in the predicate — an executable,
    // wrong "optimization" of the same intent. The oracle must report it.
    let k = seeded();
    let baseline =
        compile_with_subject("MATCH fact WHERE kind == \"hot\" RETURN *", "alice").unwrap();
    let evil = compile_with_subject("MATCH fact WHERE kind == \"cold\" RETURN *", "alice").unwrap();
    let report = run(
        &k,
        &[OracleEntry {
            id: "evil-constant-flip".into(),
            baseline,
            candidate: evil,
        }],
    );
    assert_eq!(
        report.divergences.len(),
        1,
        "oracle must detect a semantic rewrite (report: {report:?})"
    );
    assert_eq!(report.entries, 1);
}

#[test]
fn gd003b_evil_predicate_drop_diverges() {
    // A second wrong rewrite shape: dropping the predicate entirely returns
    // all rows — divergence must fire here too (regression shape for future
    // planner rules that might drop ops).
    let k = seeded();
    let baseline =
        compile_with_subject("MATCH fact WHERE kind == \"hot\" RETURN *", "alice").unwrap();
    let evil = compile_with_subject("MATCH fact RETURN *", "alice").unwrap();
    let report = run(
        &k,
        &[OracleEntry {
            id: "evil-predicate-drop".into(),
            baseline,
            candidate: evil,
        }],
    );
    assert_eq!(
        report.divergences.len(),
        1,
        "oracle must detect a dropped predicate (report: {report:?})"
    );
}

// Keep Predicate imported even though gd003 compiles its evil plans from
// query text — M1's corpus will build evil plans structurally (subject-swap,
// tenant-swap) and needs the same import surface.
#[allow(dead_code)]
fn _m1_surface(_p: Predicate) {}
