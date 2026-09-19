//! KOQL v1 remainder (P5-M2, roadmap ND-02) — kq001–kq012.
//!
//! The roadmap's ND-02 TDD RED list verbatim: syntax (kq001), precedence
//! (kq002), invalid constructs (kq003), temporal expressions (kq004), graph
//! traversal (kq005), semantic search (kq006), provenance/evidence (kq007),
//! pagination/ordering (kq008) — plus the phase-5 additions: invalid
//! security references fail closed (kq009), numeric literals lower to
//! `Value::Int` when integral (kq010), stable-AST versioning + serde
//! round-trip (kq011), precise semantic errors on the new clauses (kq012).

use aikoql_compiler::parser::{self, ast::*};
// Explicit import wins over the globs: AggFunc exists in both ast and kernel::ir.
use aikoql_compiler::parser::ast::AggFunc;
use aikoql_compiler::semantic::SemanticAnalyzer;
use aikoql_kernel::ir::*;
use aikoql_kernel::lifecycle::schema::SchemaRegistry;
use aikoql_kernel::Value;

fn as_match(stmt: Statement) -> MatchStatement {
    match stmt {
        Statement::Match(m) => m,
        other => panic!("expected Match, got {:?}", other),
    }
}

fn compiled(source: &str) -> IrPlan {
    parser::compile(source).unwrap_or_else(|e| panic!("compile failed: {}", e))
}

// ---------------------------------------------------------------------------
// kq001 — syntax: the new constructs parse
// ---------------------------------------------------------------------------

#[test]
fn kq001_order_by_parses() {
    let m = as_match(parser::parse("MATCH Person ORDER BY name RETURN *").unwrap());
    let ob = m.order_by.as_ref().expect("order_by clause");
    assert_eq!(ob.keys.len(), 1);
    assert_eq!(ob.keys[0].field, "name");
    assert!(!ob.keys[0].desc);
}

#[test]
fn kq001_order_by_multi_with_direction() {
    let m =
        as_match(parser::parse("MATCH Fact ORDER BY severity DESC, created ASC RETURN *").unwrap());
    let ob = m.order_by.as_ref().expect("order_by clause");
    assert_eq!(ob.keys.len(), 2);
    assert_eq!(ob.keys[0].field, "severity");
    assert!(ob.keys[0].desc);
    assert_eq!(ob.keys[1].field, "created");
    assert!(!ob.keys[1].desc);
}

#[test]
fn kq001_group_by_keys_and_all_aggregates() {
    let m = as_match(
        parser::parse(
            "MATCH Fact GROUP BY kind, COUNT(*), AVG(temp), SUM(temp), MIN(temp), MAX(temp) RETURN *",
        )
        .unwrap(),
    );
    let gb = m.group_by.as_ref().expect("group_by clause");
    assert_eq!(gb.keys, vec!["kind".to_string()]);
    assert_eq!(gb.aggs.len(), 5);
    assert_eq!(gb.aggs[0].func, AggFunc::Count);
    assert_eq!(gb.aggs[0].field, None); // COUNT(*)
    assert_eq!(gb.aggs[1].func, AggFunc::Avg);
    assert_eq!(gb.aggs[1].field.as_deref(), Some("temp"));
    assert_eq!(gb.aggs[2].func, AggFunc::Sum);
    assert_eq!(gb.aggs[3].func, AggFunc::Min);
    assert_eq!(gb.aggs[4].func, AggFunc::Max);
}

#[test]
fn kq001_group_by_aggregate_with_field() {
    let m = as_match(parser::parse("MATCH Fact GROUP BY COUNT(koid) RETURN *").unwrap());
    let gb = m.group_by.as_ref().expect("group_by clause");
    assert!(gb.keys.is_empty());
    assert_eq!(gb.aggs[0].field.as_deref(), Some("koid"));
}

#[test]
fn kq001_join_parses() {
    let m =
        as_match(parser::parse("MATCH Employee JOIN Department ON id == dept RETURN *").unwrap());
    let j = m.join.as_ref().expect("join clause");
    assert_eq!(j.right_type, "Department");
    assert_eq!(j.on.left, "id");
    assert_eq!(j.on.right, "dept");
}

// ---------------------------------------------------------------------------
// kq002 — precedence
// ---------------------------------------------------------------------------

#[test]
fn kq002_order_direction_binds_to_nearest_key() {
    // DESC applies to `b` only — direction binds to the key it follows.
    let m = as_match(parser::parse("MATCH Fact ORDER BY a, b DESC RETURN *").unwrap());
    let ob = m.order_by.as_ref().unwrap();
    assert!(!ob.keys[0].desc);
    assert!(ob.keys[1].desc);
}

#[test]
fn kq002_where_terminates_before_order_by() {
    // The WHERE predicate ends at the ORDER keyword — no greedy predicate parse.
    let m = as_match(parser::parse("MATCH X WHERE a == 1 AND b == 2 ORDER BY a RETURN *").unwrap());
    assert_eq!(m.predicates.len(), 2);
    assert_eq!(m.order_by.as_ref().unwrap().keys[0].field, "a");
}

// ---------------------------------------------------------------------------
// kq003 — invalid constructs fail with precise parser errors
// ---------------------------------------------------------------------------

#[test]
fn kq003_order_by_requires_field() {
    assert!(parser::parse("MATCH X ORDER BY RETURN *").is_err());
}

#[test]
fn kq003_order_by_unknown_direction_is_rejected() {
    assert!(parser::parse("MATCH X ORDER BY a SIDEWAYS RETURN *").is_err());
}

#[test]
fn kq003_group_by_requires_item() {
    assert!(parser::parse("MATCH X GROUP BY RETURN *").is_err());
}

#[test]
fn kq003_aggregate_requires_argument() {
    assert!(parser::parse("MATCH X GROUP BY COUNT() RETURN *").is_err());
}

#[test]
fn kq003_unknown_aggregate_function_is_rejected() {
    assert!(parser::parse("MATCH X GROUP BY MEDIAN(temp) RETURN *").is_err());
}

#[test]
fn kq003_join_requires_on() {
    assert!(parser::parse("MATCH A JOIN B RETURN *").is_err());
}

#[test]
fn kq003_join_on_requires_two_fields() {
    assert!(parser::parse("MATCH A JOIN B ON x == RETURN *").is_err());
}

#[test]
fn kq003_duplicate_order_by_is_rejected() {
    assert!(parser::parse("MATCH X ORDER BY a ORDER BY b RETURN *").is_err());
}

#[test]
fn kq003_duplicate_group_by_is_rejected() {
    assert!(parser::parse("MATCH X GROUP BY a GROUP BY b RETURN *").is_err());
}

#[test]
fn kq003_duplicate_join_is_rejected() {
    assert!(parser::parse("MATCH A JOIN B ON x == y JOIN C ON z == w RETURN *").is_err());
}

// ---------------------------------------------------------------------------
// kq004–kq007 — the shipped clauses compose with the new ones
// ---------------------------------------------------------------------------

#[test]
fn kq004_temporal_composes_with_order_by() {
    let plan = compiled("MATCH Fact AS_OF 1000 ORDER BY severity RETURN *");
    assert!(matches!(plan.operators[1], IrOp::Temporal { .. }));
    let sort_pos = plan
        .operators
        .iter()
        .position(|op| matches!(op, IrOp::Sort { .. }))
        .expect("Sort in plan");
    assert!(sort_pos > 1, "Sort lands after Temporal");
}

#[test]
fn kq005_traverse_composes_with_order_by() {
    let m = as_match(
        parser::parse("MATCH Person TRAVERSE knows DEPTH 2 ORDER BY name RETURN *").unwrap(),
    );
    assert!(m.traverse.is_some());
    assert!(m.order_by.is_some());
}

#[test]
fn kq006_semantic_search_composes_with_order_by() {
    let m =
        as_match(parser::parse("MATCH Doc SIMILAR TO \"q\" ORDER BY created RETURN *").unwrap());
    assert!(m.similarity.is_some());
    assert!(m.order_by.is_some());
}

#[test]
fn kq007_provenance_composes_with_order_by() {
    let m = as_match(parser::parse("MATCH Fact SOURCE \"a.md\" ORDER BY ts RETURN *").unwrap());
    assert_eq!(m.provenance.as_deref(), Some("a.md"));
    assert!(m.order_by.is_some());
}

// ---------------------------------------------------------------------------
// kq008 — pagination/ordering: Sort lands before Limit
// ---------------------------------------------------------------------------

#[test]
fn kq008_sort_lands_before_limit() {
    let plan = compiled("MATCH Fact ORDER BY ts DESC LIMIT 5 OFFSET 2 RETURN *");
    let sort_pos = plan
        .operators
        .iter()
        .position(|op| matches!(op, IrOp::Sort { .. }))
        .expect("Sort in plan");
    let limit_pos = plan
        .operators
        .iter()
        .position(|op| matches!(op, IrOp::Limit { .. }))
        .expect("Limit in plan");
    assert!(sort_pos < limit_pos, "ordering runs before pagination");
    match &plan.operators[limit_pos] {
        IrOp::Limit { limit, offset } => {
            assert_eq!(*limit, 5);
            assert_eq!(*offset, 2);
        }
        _ => unreachable!(),
    }
}

#[test]
fn kq008_sort_lands_after_project_before_limit() {
    let plan = compiled("MATCH Fact ORDER BY ts LIMIT 3 RETURN koid");
    assert!(matches!(plan.operators[1], IrOp::Project { .. }));
    assert!(matches!(plan.operators[2], IrOp::Sort { .. }));
    assert!(matches!(plan.operators[3], IrOp::Limit { .. }));
}

#[test]
fn kq008_group_by_pipeline_positions() {
    // Filter → Aggregate → Sort → Limit (no Project for RETURN *).
    let plan = compiled(
        "MATCH Fact WHERE kind == \"hot\" GROUP BY kind, COUNT(*) ORDER BY kind LIMIT 3 RETURN *",
    );
    assert!(matches!(plan.operators[0], IrOp::Scan { .. }));
    assert!(matches!(plan.operators[1], IrOp::Filter { .. }));
    assert!(matches!(plan.operators[2], IrOp::Aggregate { .. }));
    assert!(matches!(plan.operators[3], IrOp::Sort { .. }));
    assert!(matches!(plan.operators[4], IrOp::Limit { .. }));
}

#[test]
fn kq008_join_pipeline_position() {
    // Join lands after Filter (the left side is filtered first).
    let plan =
        compiled("MATCH Employee WHERE dept == \"Eng\" JOIN Department ON id == dept RETURN *");
    assert!(matches!(plan.operators[0], IrOp::Scan { .. }));
    assert!(matches!(plan.operators[1], IrOp::Filter { .. }));
    match &plan.operators[2] {
        IrOp::Join {
            right_type,
            on_left,
            on_right,
            kind,
        } => {
            assert_eq!(right_type, "Department");
            assert_eq!(on_left, "id");
            assert_eq!(on_right, "dept");
            assert_eq!(*kind, JoinKind::Inner, "bare JOIN lowers to Inner");
        }
        other => panic!("expected Join, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// kq009 — invalid security references fail closed
// ---------------------------------------------------------------------------

#[test]
fn kq009_match_role_fails_closed() {
    let err = parser::compile("MATCH aikoql:role RETURN *").unwrap_err();
    assert!(err.contains("AIKOQL1035"), "got: {}", err);
}

#[test]
fn kq009_match_policy_fails_closed_on_scoped_path() {
    // The MCP path is compile_scoped — the guard must hold there too.
    let err = parser::compile_scoped(
        "MATCH aikoql:policy RETURN *",
        "alice",
        &["admin".into()],
        Some("t1"),
    )
    .unwrap_err();
    assert!(err.contains("AIKOQL1035"), "got: {}", err);
}

#[test]
fn kq009_join_onto_security_type_fails_closed() {
    let err = parser::compile("MATCH Employee JOIN aikoql:role ON x == y RETURN *").unwrap_err();
    assert!(err.contains("AIKOQL1035"), "got: {}", err);
}

#[test]
fn kq009_semantic_analysis_reports_security_violation() {
    let stmt = parser::parse("MATCH aikoql:role RETURN *").unwrap();
    let reg = SchemaRegistry::new();
    let err = SemanticAnalyzer::new(&reg).analyze(&stmt).unwrap_err();
    assert_eq!(err.code, parser::diagnostics::Code::SecurityViolation);
}

#[test]
fn kq009_document_type_stays_queryable() {
    // The knowledge payload types stay queryable — only the security objects
    // (role/policy) fail closed.
    assert!(parser::compile("MATCH aikoql:document RETURN *").is_ok());
}

// ---------------------------------------------------------------------------
// kq010 — numeric literals lower to Value::Int when integral
// ---------------------------------------------------------------------------

#[test]
fn kq010_integral_literal_lowers_to_int() {
    let plan = compiled("MATCH fact WHERE temp == 35 RETURN *");
    match &plan.operators[1] {
        IrOp::Filter { predicates } => {
            assert_eq!(predicates[0].value, Value::Int(35));
        }
        _ => panic!("expected Filter"),
    }
}

#[test]
fn kq010_fractional_literal_stays_float() {
    let plan = compiled("MATCH fact WHERE temp == 35.5 RETURN *");
    match &plan.operators[1] {
        IrOp::Filter { predicates } => {
            assert_eq!(predicates[0].value, Value::Float(35.5));
        }
        _ => panic!("expected Filter"),
    }
}

#[test]
fn kq010_out_of_i64_range_stays_float() {
    let plan = compiled("MATCH fact WHERE big == 9223372036854775808.0 RETURN *");
    match &plan.operators[1] {
        IrOp::Filter { predicates } => {
            assert_eq!(
                predicates[0].value,
                Value::Float(9_223_372_036_854_775_808.0)
            );
        }
        _ => panic!("expected Filter"),
    }
}

// ---------------------------------------------------------------------------
// kq011 — stable AST: version stamp + serde round-trip
// ---------------------------------------------------------------------------

#[test]
fn kq011_parse_versioned_stamps_the_ast_version() {
    let v = parser::parse_versioned("MATCH Person RETURN *").unwrap();
    assert_eq!(v.version, AST_VERSION);
    assert!(matches!(v.statement, Statement::Match(_)));
}

#[test]
fn kq011_serde_round_trip_preserves_the_ast() {
    let v = parser::parse_versioned(
        "MATCH Fact GROUP BY kind, COUNT(*) ORDER BY kind DESC LIMIT 3 RETURN *",
    )
    .unwrap();
    let json = serde_json::to_string(&v).unwrap();
    assert!(json.contains("\"version\":1"), "serialized form: {}", json);
    let back: VersionedStatement = serde_json::from_str(&json).unwrap();
    assert_eq!(back, v);
}

// ---------------------------------------------------------------------------
// kq012 — precise semantic errors on the new clauses
// ---------------------------------------------------------------------------

fn registry_with_closed_schemas() -> SchemaRegistry {
    use aikoql_kernel::knowledge::kom::Schema;
    let mut r = SchemaRegistry::new();
    let mut person = Schema::new("Person", 1);
    person.allowed_properties = Some(["name".into(), "company".into()].into_iter().collect());
    let mut dept = Schema::new("Department", 1);
    dept.allowed_properties = Some(["name".into()].into_iter().collect());
    let mut emp = Schema::new("Employee", 1);
    emp.allowed_properties = Some(["id".into(), "dept".into()].into_iter().collect());
    r.register(person);
    r.register(dept);
    r.register(emp);
    r
}

#[test]
fn kq012_order_by_unknown_field_is_unknown_property() {
    let r = registry_with_closed_schemas();
    let a = SemanticAnalyzer::new(&r);
    let stmt = parser::parse("MATCH Person ORDER BY bogus RETURN *").unwrap();
    let err = a.analyze(&stmt).unwrap_err();
    assert_eq!(err.code, parser::diagnostics::Code::UnknownProperty);
    assert!(err.format().starts_with("AIKOQL1031"));
}

#[test]
fn kq012_group_by_unknown_field_is_unknown_property() {
    let r = registry_with_closed_schemas();
    let a = SemanticAnalyzer::new(&r);
    let stmt = parser::parse("MATCH Person GROUP BY bogus, COUNT(*) RETURN *").unwrap();
    let err = a.analyze(&stmt).unwrap_err();
    assert_eq!(err.code, parser::diagnostics::Code::UnknownProperty);
}

#[test]
fn kq012_join_unknown_right_type_is_unknown_type() {
    let r = registry_with_closed_schemas();
    let a = SemanticAnalyzer::new(&r);
    let stmt = parser::parse("MATCH Employee JOIN Nope ON x == y RETURN *").unwrap();
    let err = a.analyze(&stmt).unwrap_err();
    assert_eq!(err.code, parser::diagnostics::Code::UnknownType);
    assert!(err.format().starts_with("AIKOQL1030"));
}

#[test]
fn kq012_join_unknown_on_field_is_unknown_property() {
    let r = registry_with_closed_schemas();
    let a = SemanticAnalyzer::new(&r);
    let stmt = parser::parse("MATCH Employee JOIN Department ON id == bogus RETURN *").unwrap();
    let err = a.analyze(&stmt).unwrap_err();
    assert_eq!(err.code, parser::diagnostics::Code::UnknownProperty);
}

#[test]
fn kq012_valid_order_group_join_passes_semantic_analysis() {
    let r = registry_with_closed_schemas();
    let a = SemanticAnalyzer::new(&r);
    let stmt = parser::parse(
        "MATCH Employee WHERE dept == \"Eng\" JOIN Department ON dept == name GROUP BY dept, COUNT(*) ORDER BY dept RETURN *",
    )
    .unwrap();
    assert!(a.analyze(&stmt).is_ok());
}
