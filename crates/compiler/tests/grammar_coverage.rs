//! Grammar coverage audit — MRFC-0010 §11.
//!
//! Every EBNF production rule from MRFC-0010 §8 must have at least one test.
//! This file is the canonical coverage map. If a rule is added to the grammar,
//! a test must be added here.

use aikoql_compiler::parser;
use aikoql_compiler::parser::ast::*;

// ---------------------------------------------------------------------------
// query = match | create | update | delete | ingest
// ---------------------------------------------------------------------------

#[test]
fn cover_query_match() {
    assert!(parser::parse("MATCH Person RETURN *").is_ok());
}

#[test]
fn cover_query_create() {
    assert!(parser::parse(r#"CREATE Person name == "Alice""#).is_ok());
}

#[test]
fn cover_query_update() {
    assert!(parser::parse(r#"UPDATE Person "abc" name == "Bob""#).is_ok());
}

#[test]
fn cover_query_delete() {
    assert!(parser::parse(r#"DELETE Person "abc""#).is_ok());
}

#[test]
fn cover_query_ingest() {
    assert!(parser::parse(r#"INGEST "file.pdf" COMMIT"#).is_ok());
}

// ---------------------------------------------------------------------------
// match = "MATCH" entity ["WHERE" predicate] ["SIMILAR" similarity]
//         ["TRAVERSE" relation] "RETURN" projection
// ---------------------------------------------------------------------------

#[test]
fn cover_match_bare() {
    let m = match parser::parse("MATCH Person RETURN *").unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(m.entity, "Person");
}

#[test]
fn cover_match_where() {
    let m = match parser::parse(r#"MATCH Person WHERE x == "y" RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(m.predicates.len(), 1);
}

#[test]
fn cover_match_similar() {
    let m = match parser::parse(r#"MATCH Person SIMILAR TO "query" RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert!(m.similarity.is_some());
}

#[test]
fn cover_match_traverse() {
    let m = match parser::parse(r#"MATCH Person TRAVERSE knows RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert!(m.traverse.is_some());
}

#[test]
fn cover_match_source() {
    let m = match parser::parse(r#"MATCH Fact SOURCE "x.pdf" RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(m.provenance.as_deref(), Some("x.pdf"));
}

#[test]
fn cover_match_limit_offset() {
    let m = match parser::parse(r#"MATCH Fact LIMIT 10 OFFSET 3 RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(m.limit, Some(10));
    assert_eq!(m.offset, Some(3));
}

#[test]
fn cover_match_all_clauses() {
    let m = match parser::parse(
        r#"MATCH Person WHERE x == "y" SIMILAR TO "q" TRAVERSE knows RETURN explain"#,
    )
    .unwrap()
    {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(m.predicates.len(), 1);
    assert!(m.similarity.is_some());
    assert!(m.traverse.is_some());
    assert_eq!(m.projection, Projection::Explain);
}

// ---------------------------------------------------------------------------
// projection = "*" | "explain" | identifier ("," identifier)*
// ---------------------------------------------------------------------------

#[test]
fn cover_projection_star() {
    let m = match parser::parse("MATCH Person RETURN *").unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(m.projection, Projection::Star);
}

#[test]
fn cover_projection_explain() {
    let m = match parser::parse("MATCH Person RETURN explain").unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(m.projection, Projection::Explain);
}

#[test]
fn cover_projection_single_field() {
    let m = match parser::parse("MATCH Person RETURN name").unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(m.projection, Projection::Fields(vec!["name".into()]));
}

#[test]
fn cover_projection_multi_field() {
    let m = match parser::parse("MATCH Person RETURN name, age, city").unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(
        m.projection,
        Projection::Fields(vec!["name".into(), "age".into(), "city".into()])
    );
}

// ---------------------------------------------------------------------------
// predicate = property operator value | predicate AND predicate | predicate OR predicate
// ---------------------------------------------------------------------------

#[test]
fn cover_predicate_simple() {
    let m = match parser::parse(r#"MATCH Person WHERE x == "y" RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(
        m.predicates[0],
        Predicate::Eq {
            property: "x".into(),
            value: Expr::String("y".into()),
        }
    );
}

#[test]
fn cover_predicate_and() {
    let m = match parser::parse(r#"MATCH Person WHERE a == "1" AND b == "2" RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(m.predicates.len(), 2);
}

#[test]
fn cover_predicate_or() {
    let m = match parser::parse(r#"MATCH Person WHERE a == "1" OR b == "2" RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert_eq!(m.predicates.len(), 2);
}

#[test]
fn cover_predicate_nested_and_or() {
    // WHERE a == "1" AND b == "2" OR c == "3"
    // Parses as: (a==1 AND b==2) OR c==3
    let m = match parser::parse(r#"MATCH Person WHERE a == "1" AND b == "2" OR c == "3" RETURN *"#)
        .unwrap()
    {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert!(m.predicates.len() >= 2);
}

// ---------------------------------------------------------------------------
// operator = "==" | "!=" | "<" | ">" | "<=" | ">="
// ---------------------------------------------------------------------------

#[test]
fn cover_operator_eq() {
    let m = match parser::parse(r#"MATCH Person WHERE a == "x" RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert!(matches!(m.predicates[0], Predicate::Eq { .. }));
}

#[test]
fn cover_operator_neq() {
    let m = match parser::parse(r#"MATCH Person WHERE a != "x" RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert!(matches!(m.predicates[0], Predicate::Neq { .. }));
}

#[test]
fn cover_operator_gt() {
    let m = match parser::parse(r#"MATCH Person WHERE a > 5 RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert!(matches!(m.predicates[0], Predicate::Gt { .. }));
}

#[test]
fn cover_operator_lt() {
    let m = match parser::parse(r#"MATCH Person WHERE a < 5 RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert!(matches!(m.predicates[0], Predicate::Lt { .. }));
}

#[test]
fn cover_operator_gte() {
    let m = match parser::parse(r#"MATCH Person WHERE a >= 5 RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert!(matches!(m.predicates[0], Predicate::Gte { .. }));
}

#[test]
fn cover_operator_lte() {
    let m = match parser::parse(r#"MATCH Person WHERE a <= 5 RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    assert!(matches!(m.predicates[0], Predicate::Lte { .. }));
}

// ---------------------------------------------------------------------------
// value = string | number | "true" | "false" | "null"
// ---------------------------------------------------------------------------

#[test]
fn cover_value_string() {
    let m = match parser::parse(r#"MATCH Person WHERE a == "hello" RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    match &m.predicates[0] {
        Predicate::Eq { value, .. } => assert_eq!(*value, Expr::String("hello".into())),
        _ => panic!(),
    }
}

#[test]
fn cover_value_number() {
    let m = match parser::parse(r#"MATCH Person WHERE a == 42 RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    match &m.predicates[0] {
        Predicate::Eq { value, .. } => assert_eq!(*value, Expr::Number(42.0)),
        _ => panic!(),
    }
}

#[test]
fn cover_value_bool() {
    let m = match parser::parse(r#"MATCH Person WHERE a == true RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    match &m.predicates[0] {
        Predicate::Eq { value, .. } => assert_eq!(*value, Expr::Bool(true)),
        _ => panic!(),
    }
}

#[test]
fn cover_value_null() {
    let m = match parser::parse(r#"MATCH Person WHERE a == null RETURN *"#).unwrap() {
        Statement::Match(m) => m,
        _ => panic!(),
    };
    match &m.predicates[0] {
        Predicate::Eq { value, .. } => assert_eq!(*value, Expr::Null),
        _ => panic!(),
    }
}

// ---------------------------------------------------------------------------
// create = "CREATE" entity property_assignment ("," property_assignment)*
// update = "UPDATE" entity koid property_assignment ("," property_assignment)*
// ---------------------------------------------------------------------------

#[test]
fn cover_create_single_prop() {
    let c = match parser::parse(r#"CREATE Person name == "Alice""#).unwrap() {
        Statement::Create(c) => c,
        _ => panic!(),
    };
    assert_eq!(c.properties.len(), 1);
}

#[test]
fn cover_create_multi_prop() {
    let c = match parser::parse(r#"CREATE Person name == "Alice", age == 30"#).unwrap() {
        Statement::Create(c) => c,
        _ => panic!(),
    };
    assert_eq!(c.properties.len(), 2);
}

#[test]
fn cover_update_single_prop() {
    let u = match parser::parse(r#"UPDATE Person "abc" name == "Bob""#).unwrap() {
        Statement::Update(u) => u,
        _ => panic!(),
    };
    assert_eq!(u.properties.len(), 1);
    assert_eq!(u.koid, "abc");
}

#[test]
fn cover_update_multi_prop() {
    let u = match parser::parse(r#"UPDATE Person "abc" name == "Bob", age == 25"#).unwrap() {
        Statement::Update(u) => u,
        _ => panic!(),
    };
    assert_eq!(u.properties.len(), 2);
}

// ---------------------------------------------------------------------------
// ingest = "INGEST" string extract_clause* build_clause? "COMMIT"
// ---------------------------------------------------------------------------

#[test]
fn cover_ingest_minimal() {
    let i = match parser::parse(r#"INGEST "file.pdf" COMMIT"#).unwrap() {
        Statement::Ingest(i) => i,
        _ => panic!(),
    };
    assert_eq!(i.source, "file.pdf");
    assert!(!i.extract_tables && !i.extract_entities && !i.build_relationships);
}

#[test]
fn cover_ingest_full() {
    let i = match parser::parse(
        r#"INGEST "file.pdf" EXTRACT tables EXTRACT entities BUILD relationships COMMIT"#,
    )
    .unwrap()
    {
        Statement::Ingest(i) => i,
        _ => panic!(),
    };
    assert!(i.extract_tables);
    assert!(i.extract_entities);
    assert!(i.build_relationships);
}

// ---------------------------------------------------------------------------
// Error path coverage: every error code has a test
// ---------------------------------------------------------------------------

#[test]
fn cover_error_unexpected_token() {
    let e = parser::parse("BOGUS").unwrap_err();
    assert!(e.contains("AIKOQL1010"));
}

#[test]
fn cover_error_invalid_operator() {
    let e = parser::parse(r#"MATCH Person WHERE name = "x" RETURN *"#).unwrap_err();
    assert!(e.contains("AIKOQL1013"));
}

#[test]
fn cover_error_unexpected_eof() {
    // "MATCH Person" with no RETURN clause hits the EOF branch in parse_match.
    let e = parser::parse("MATCH Person").unwrap_err();
    assert!(e.contains("AIKOQL1012"), "got: {}", e);
}
