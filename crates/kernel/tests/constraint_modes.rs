//! P3-M5 — constraint engine: enforcement modes, severity → ViolationEvent
//! catalog, relationship cardinality, temporal windows, incremental counters
//! (MRFC-0060 §16/24/30–32/36–37; docs/IMPLEMENTATION-PLAN-PHASE3.md §64–66).
//!
//! REDs cst001–007. Written RED-first against the intended API surface:
//! `EnforcementMode`, `ViolationSeverity::Info`, `ViolationEvent`,
//! `CardinalityConstraint`, `TemporalConstraint`, `Schema::has_enabled_constraints`,
//! `ConstraintEvaluator::stats`, `Kernel::{constraint_stats, violation_events}`.

use aikoql_kernel::lifecycle::constraint::{ConstraintEvalStats, ConstraintEvaluator};
use aikoql_kernel::*;
use std::collections::HashSet;
use std::sync::Arc;

fn mk() -> Kernel {
    Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap()
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn alice() -> Subject {
    Subject::new("alice")
}

/// `age >= n` check predicate.
fn age_ge(n: i64) -> CheckExpression {
    CheckExpression::Compare {
        op: CompareOp::Gte,
        left: Box::new(CheckExpression::Property("age".into())),
        right: Box::new(CheckExpression::Literal(Value::Int(n))),
    }
}

/// Always-false predicate — a tripwire: if it is ever evaluated it fails.
fn impossible() -> CheckExpression {
    CheckExpression::Compare {
        op: CompareOp::Eq,
        left: Box::new(CheckExpression::Literal(Value::Int(1))),
        right: Box::new(CheckExpression::Literal(Value::Int(2))),
    }
}

fn props(pairs: &[(&str, Value)]) -> PropertyMap {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn ck(
    name: &str,
    predicate: CheckExpression,
    mode: EnforcementMode,
    severity: ViolationSeverity,
) -> CheckConstraint {
    CheckConstraint {
        name: name.into(),
        predicate,
        timing: ConstraintTiming::Immediate,
        mode,
        severity,
    }
}

fn rel(t: &str, target: KOID) -> RelationshipRef {
    RelationshipRef {
        rel_type: t.into(),
        target,
        direction: Direction::Outbound,
    }
}

fn team(k: &Kernel, name: &str) -> KOID {
    let mut r = RememberRequest::create(alice(), meta("Team"));
    r.properties.insert("name".into(), Value::Text(name.into()));
    k.remember(r).unwrap().koid
}

// ---------------------------------------------------------------------------
// cst001 — mode matrix: ENFORCED/ADVISORY block, record, DISABLED skips
// ---------------------------------------------------------------------------

#[test]
fn cst001_mode_matrix_block_record_log_skip() {
    // All four modes on the same predicate, violating properties.
    let ev = ConstraintEvaluator::new();
    let schema = Schema::new("Person", 1)
        .check("ck_enforced", age_ge(18)) // builder default = Enforced / Error
        .check_constraint(ck(
            "ck_validated",
            age_ge(18),
            EnforcementMode::Validated,
            ViolationSeverity::Error,
        ))
        .check_constraint(ck(
            "ck_advisory",
            age_ge(18),
            EnforcementMode::Advisory,
            ViolationSeverity::Warning,
        ))
        .check_constraint(ck(
            "ck_disabled",
            impossible(),
            EnforcementMode::Disabled,
            ViolationSeverity::Error,
        ));
    let bad = props(&[("age", Value::Int(15))]);
    let r = ev.evaluate_full(&schema, &bad, None, Some(KOID::ZERO), None);

    // Block: Enforced + Validated fail the write.
    assert!(!r.valid);
    let names: Vec<&str> = r
        .violations
        .iter()
        .map(|v| v.constraint_name.as_str())
        .collect();
    assert!(
        names.contains(&"ck_enforced"),
        "enforced must block: {:?}",
        names
    );
    assert!(
        names.contains(&"ck_validated"),
        "validated must block: {:?}",
        names
    );

    // Record: Advisory lands in warnings, write stays valid on its own.
    let wnames: Vec<&str> = r
        .warnings
        .iter()
        .map(|v| v.constraint_name.as_str())
        .collect();
    assert!(
        wnames.contains(&"ck_advisory"),
        "advisory must record: {:?}",
        wnames
    );

    // Skip: Disabled absent everywhere and its impossible predicate never ran.
    assert!(!names.contains(&"ck_disabled"));
    assert!(!wnames.contains(&"ck_disabled"));

    let s = ev.stats();
    assert_eq!(s.evaluated, 3, "enforced+validated+advisory evaluated");
    assert_eq!(s.skipped_disabled, 1);
    assert_eq!(s.skipped_unaffected, 0);

    // Advisory-only schema: the write proceeds, violation is recorded as an event.
    let ev2 = ConstraintEvaluator::new();
    let adv = Schema::new("Person", 1).check_constraint(ck(
        "ck_a",
        age_ge(18),
        EnforcementMode::Advisory,
        ViolationSeverity::Info,
    ));
    let r2 = ev2.evaluate_full(&adv, &bad, None, Some(KOID::ZERO), None);
    assert!(r2.valid, "advisory alone must not block");
    assert_eq!(r2.warnings.len(), 1);
    assert_eq!(r2.warnings[0].mode, EnforcementMode::Advisory);
    assert_eq!(r2.warnings[0].severity, ViolationSeverity::Info);
    assert_eq!(r2.warnings[0].koid, Some(KOID::ZERO));

    // Disabled-only schema: nothing runs, nothing recorded.
    let ev3 = ConstraintEvaluator::new();
    let dead = Schema::new("Person", 1).check_constraint(ck(
        "ck_dead",
        impossible(),
        EnforcementMode::Disabled,
        ViolationSeverity::Error,
    ));
    assert!(!dead.has_enabled_constraints());
    let r3 = ev3.evaluate_full(&dead, &bad, None, Some(KOID::ZERO), None);
    assert!(r3.valid);
    assert!(r3.violations.is_empty() && r3.warnings.is_empty());
    assert_eq!(ev3.stats().evaluated, 0);
    assert_eq!(ev3.stats().skipped_disabled, 1);
}

// ---------------------------------------------------------------------------
// cst002 — severity → ViolationEvent catalog (ERROR/WARNING/INFO stamped)
// ---------------------------------------------------------------------------

#[test]
fn cst002_severity_to_violation_event_catalog() {
    let ev = ConstraintEvaluator::new();
    let schema = Schema::new("Person", 1)
        .check("ck_err", age_ge(18)) // default severity Error
        .check_constraint(ck(
            "ck_warn",
            age_ge(18),
            EnforcementMode::Enforced,
            ViolationSeverity::Warning,
        ))
        .check_constraint(ck(
            "ck_info",
            age_ge(18),
            EnforcementMode::Advisory,
            ViolationSeverity::Info,
        ));
    let bad = props(&[("age", Value::Int(15))]);
    let r = ev.evaluate_full(&schema, &bad, None, None, None);

    // Every recorded event carries its declared (constraint, mode, severity).
    let events: Vec<ViolationEvent> = r
        .violations
        .iter()
        .chain(r.warnings.iter())
        .cloned()
        .collect();
    assert_eq!(events.len(), 3);
    let by_name = |n: &str| events.iter().find(|e| e.constraint_name == n).unwrap();
    assert_eq!(by_name("ck_err").severity, ViolationSeverity::Error);
    assert_eq!(by_name("ck_err").mode, EnforcementMode::Enforced);
    assert_eq!(by_name("ck_warn").severity, ViolationSeverity::Warning);
    assert_eq!(by_name("ck_info").severity, ViolationSeverity::Info);
    assert_eq!(by_name("ck_info").mode, EnforcementMode::Advisory);
    assert!(by_name("ck_err").timestamp == 0 && by_name("ck_err").koid.is_none());

    // Info-severity builder exists and defaults to Enforced mode.
    let built = ConstraintViolation::info("x", "y");
    assert_eq!(built.severity, ViolationSeverity::Info);
    assert_eq!(built.mode, EnforcementMode::Enforced);
    assert_eq!(built.timestamp, 0);
}

// ---------------------------------------------------------------------------
// cst003 — cardinality: KO exceeding N outbound rels of the type violates
// ---------------------------------------------------------------------------

#[test]
fn cst003_cardinality_bounds_outbound_relationship_count() {
    let k = mk();
    k.register_schema(Schema::new("Person", 1).cardinality("c_member", "member_of", None, Some(1)))
        .unwrap();
    let t1 = team(&k, "alpha");
    let t2 = team(&k, "beta");

    // Two outbound member_of → violates max 1 (Enforced blocks the write).
    let mut r = RememberRequest::create(alice(), meta("Person"));
    r.relationships = vec![rel("member_of", t1), rel("member_of", t2)];
    let err = k.remember(r).unwrap_err();
    assert!(
        err.to_string().contains("c_member"),
        "cardinality violation must name the constraint: {}",
        err
    );

    // Below the minimum → also blocks.
    k.register_schema(Schema::new("Org", 1).cardinality("c_dept", "has_dept", Some(1), None))
        .unwrap();
    let r2 = RememberRequest::create(alice(), meta("Org"));
    let err2 = k.remember(r2).unwrap_err();
    assert!(
        err2.to_string().contains("c_dept"),
        "below-minimum must block: {}",
        err2
    );

    // Other relationship types don't count toward the bound.
    k.register_schema(Schema::new("Animal", 1).cardinality("c_friend", "friend_of", None, Some(0)))
        .unwrap();
    let mut r3 = RememberRequest::create(alice(), meta("Animal"));
    r3.relationships = vec![rel("eats", t1)];
    k.remember(r3).unwrap();

    // Advisory cardinality records instead of blocking.
    k.register_schema(
        Schema::new("Driver", 1).cardinality_constraint(CardinalityConstraint {
            name: "c_ride".into(),
            relationship_type: "rides".into(),
            min_outbound: None,
            max_outbound: Some(1),
            mode: EnforcementMode::Advisory,
            severity: ViolationSeverity::Warning,
        }),
    )
    .unwrap();
    let mut r4 = RememberRequest::create(alice(), meta("Driver"));
    r4.relationships = vec![rel("rides", t1), rel("rides", t2)];
    k.remember(r4).unwrap();
    let events = k.violation_events();
    let evt = events
        .iter()
        .find(|e| e.constraint_name == "c_ride")
        .expect("advisory cardinality violation must be recorded");
    assert_eq!(evt.mode, EnforcementMode::Advisory);
    assert_eq!(evt.severity, ViolationSeverity::Warning);
}

// ---------------------------------------------------------------------------
// cst004 — cross-type: the constraint binds its schema's type, not others
// ---------------------------------------------------------------------------

#[test]
fn cst004_cardinality_is_cross_type_discriminated() {
    let k = mk();
    k.register_schema(Schema::new("Person", 1).cardinality("c_member", "member_of", None, Some(1)))
        .unwrap();
    let t1 = team(&k, "alpha");
    let t2 = team(&k, "beta");

    // Person: 2 member_of → blocked.
    let mut r = RememberRequest::create(alice(), meta("Person"));
    r.relationships = vec![rel("member_of", t1), rel("member_of", t2)];
    assert!(k.remember(r).is_err());

    // Team: identical relationship shape, no Team constraint → allowed.
    let mut r2 = RememberRequest::create(alice(), meta("Team"));
    r2.relationships = vec![rel("member_of", t1), rel("member_of", t2)];
    k.remember(r2).unwrap();
}

// ---------------------------------------------------------------------------
// cst005 — temporal window: start <= end ordering, mode-aware, write-set filtered
// ---------------------------------------------------------------------------

#[test]
fn cst005_temporal_window_ordering() {
    let ev = ConstraintEvaluator::new();
    let schema = Schema::new("Event", 1).temporal("t_valid", "valid_from", "valid_to");

    // Reversed window → violates (Enforced blocks).
    let bad = props(&[
        ("valid_from", Value::Int(200)),
        ("valid_to", Value::Int(100)),
    ]);
    let r = ev.evaluate_full(&schema, &bad, None, Some(KOID::ZERO), None);
    assert!(!r.valid);
    assert_eq!(r.violations[0].constraint_name, "t_valid");

    // Ordered window → valid.
    let good = props(&[
        ("valid_from", Value::Int(100)),
        ("valid_to", Value::Int(200)),
    ]);
    let ev2 = ConstraintEvaluator::new();
    assert!(ev2.evaluate_full(&schema, &good, None, None, None).valid);

    // ISO-8601 text windows order lexicographically.
    let tbad = props(&[
        ("valid_from", Value::Text("2026-09-09T12:00:00Z".into())),
        ("valid_to", Value::Text("2026-09-08T12:00:00Z".into())),
    ]);
    let ev3 = ConstraintEvaluator::new();
    assert!(!ev3.evaluate_full(&schema, &tbad, None, None, None).valid);

    // One bound missing → nothing to compare, not a violation.
    let half = props(&[("valid_from", Value::Int(200))]);
    let ev4 = ConstraintEvaluator::new();
    assert!(ev4.evaluate_full(&schema, &half, None, None, None).valid);

    // Advisory temporal records, does not block.
    let adv = Schema::new("Event", 1).temporal_constraint(TemporalConstraint {
        name: "t_a".into(),
        start_property: "valid_from".into(),
        end_property: "valid_to".into(),
        mode: EnforcementMode::Advisory,
        severity: ViolationSeverity::Warning,
    });
    let ev5 = ConstraintEvaluator::new();
    let ra = ev5.evaluate_full(&adv, &bad, None, Some(KOID::ZERO), None);
    assert!(ra.valid);
    assert_eq!(ra.warnings.len(), 1);
    assert_eq!(ra.warnings[0].constraint_name, "t_a");

    // Write-set filter: neither bound touched → constraint skipped.
    let ev6 = ConstraintEvaluator::new();
    let ws_other: HashSet<String> = ["other".to_string()].into_iter().collect();
    let r6 = ev6.evaluate_full(&schema, &bad, Some(&ws_other), None, None);
    assert!(r6.valid);
    assert_eq!(ev6.stats().evaluated, 0);
    assert_eq!(ev6.stats().skipped_unaffected, 1);
}

// ---------------------------------------------------------------------------
// cst006 — incremental re-eval: only affected constraints/objects evaluated
// ---------------------------------------------------------------------------

#[test]
fn cst006_incremental_only_affected_evaluated() {
    // Evaluator level: write-set {name} → age-check skipped even though violating.
    let ev = ConstraintEvaluator::new();
    let schema = Schema::new("Person", 1).check("ck_age", age_ge(18)).check(
        "ck_name",
        CheckExpression::Compare {
            op: CompareOp::Neq,
            left: Box::new(CheckExpression::Property("name".into())),
            right: Box::new(CheckExpression::Literal(Value::Text("".into()))),
        },
    );
    let ws_name: HashSet<String> = ["name".to_string()].into_iter().collect();
    let both = props(&[("age", Value::Int(15)), ("name", Value::Text("ann".into()))]);
    let r = ev.evaluate_full(&schema, &both, Some(&ws_name), Some(KOID::ZERO), None);
    assert!(r.valid, "age untouched by the write must not be evaluated");
    assert_eq!(ev.stats().evaluated, 1);
    assert_eq!(ev.stats().skipped_unaffected, 1);

    // Empty write-set: skim skips everything.
    let ev2 = ConstraintEvaluator::new();
    let empty_ws: HashSet<String> = HashSet::new();
    let r2 = ev2.evaluate_full(&schema, &both, Some(&empty_ws), Some(KOID::ZERO), None);
    assert!(r2.valid);
    assert_eq!(ev2.stats().evaluated, 0);

    // Kernel level: a second object whose write touches no constrained property
    // must not move the evaluated counter.
    let k = mk();
    k.register_schema(Schema::new("Person", 1).check("ck_age", age_ge(18)))
        .unwrap();
    let mut r3 = RememberRequest::create(alice(), meta("Person"));
    r3.properties.insert("age".into(), Value::Int(15));
    assert!(k.remember(r3).is_err(), "violating write must still block");
    let before = k.constraint_stats();
    assert!(before.evaluated >= 1);

    let mut r4 = RememberRequest::create(alice(), meta("Person"));
    r4.properties
        .insert("name".into(), Value::Text("ann".into()));
    k.remember(r4).unwrap();
    let after = k.constraint_stats();
    assert_eq!(
        after.evaluated, before.evaluated,
        "write of an unaffected property must not re-evaluate"
    );
    let _: ConstraintEvalStats = before; // type is Copy-exported
}

// ---------------------------------------------------------------------------
// cst007 — DISABLED = zero-overhead: kernel never invokes the evaluator
// ---------------------------------------------------------------------------

#[test]
fn cst007_disabled_zero_overhead_no_evaluator_invoked() {
    let k = mk();
    let schema = Schema::new("Person", 1)
        .check_constraint(ck(
            "ck_dead",
            impossible(),
            EnforcementMode::Disabled,
            ViolationSeverity::Error,
        ))
        .unique_constraint(UniqueConstraint {
            properties: vec!["email".into()],
            scope: UniquenessScope::Type,
            timing: ConstraintTiming::Immediate,
            mode: EnforcementMode::Disabled,
            severity: ViolationSeverity::Error,
        });
    assert!(!schema.has_enabled_constraints());
    k.register_schema(schema).unwrap();

    // Both writes would violate the (disabled) constraints. The first's
    // impossible predicate would fail if evaluated; the second duplicates
    // the unique email. Both succeed and the evaluator counter never moves.
    let mut r1 = RememberRequest::create(alice(), meta("Person"));
    r1.properties
        .insert("email".into(), Value::Text("a@b".into()));
    k.remember(r1).unwrap();
    let mut r2 = RememberRequest::create(alice(), meta("Person"));
    r2.properties
        .insert("email".into(), Value::Text("a@b".into()));
    k.remember(r2).unwrap();

    assert_eq!(k.constraint_stats().evaluated, 0, "evaluator must not run");
    assert_eq!(k.constraint_stats().skipped_disabled, 0);
    assert!(k.violation_events().is_empty());
}
