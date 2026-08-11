//! Golden AST snapshot tests — MRFC-0010 §11.
//!
//! Each canonical query is parsed and its AST structure verified. When the
//! AST changes, these tests break intentionally — forcing a conscious review.
//!
//! The `golden_match_simple` test uses exact Debug-format matching as a
//! full-snapshot canary. The rest use structural assertions.

use aikoql_compiler::parser;
use aikoql_compiler::parser::ast::*;

// Helper: extract a MatchStatement from a parsed Statement.
fn as_match(stmt: &Statement) -> &MatchStatement {
    match stmt {
        Statement::Match(m) => m,
        other => panic!("expected Match, got {:?}", other),
    }
}

#[test]
fn golden_match_simple() {
    let stmt = parser::parse("MATCH Person RETURN *").unwrap();
    let snap = format!("{:#?}", stmt);
    let expected = "\
Match(
    MatchStatement {
        entity: \"Person\",
        predicates: [],
        similarity: None,
        traverse: None,
        projection: Star,
    },
)";
    assert_eq!(snap.trim(), expected.trim());
}

#[test]
fn golden_match_with_filter() {
    let stmt = parser::parse(r#"MATCH Person WHERE company == "Visa" RETURN *"#).unwrap();
    let m = as_match(&stmt);
    assert_eq!(m.entity, "Person");
    assert_eq!(m.predicates.len(), 1);
    assert_eq!(m.projection, Projection::Star);
    match &m.predicates[0] {
        Predicate::Eq { property, value } => {
            assert_eq!(property, "company");
            assert_eq!(*value, Expr::String("Visa".into()));
        }
        _ => panic!("expected Eq predicate"),
    }
}

#[test]
fn golden_match_hybrid() {
    let stmt = parser::parse(
        r#"MATCH Person SIMILAR TO "John" TRAVERSE managed_by WHERE company == "Visa" RETURN explain"#,
    )
    .unwrap();
    let m = as_match(&stmt);
    assert_eq!(m.entity, "Person");
    assert!(m.similarity.is_some());
    assert_eq!(m.similarity.as_ref().unwrap().query, "John");
    assert!(m.traverse.is_some());
    assert_eq!(m.traverse.as_ref().unwrap().relation, "managed_by");
    assert_eq!(m.predicates.len(), 1);
    assert_eq!(m.projection, Projection::Explain);
}

#[test]
fn golden_match_multi_filter() {
    let stmt =
        parser::parse(r#"MATCH Person WHERE company == "Visa" AND city == "Amsterdam" RETURN *"#)
            .unwrap();
    let m = as_match(&stmt);
    assert_eq!(m.predicates.len(), 2);
    // AND flattens into two Eq predicates.
    for pred in &m.predicates {
        match pred {
            Predicate::Eq { property, .. } => {
                assert!(property == "company" || property == "city");
            }
            _ => panic!("expected Eq"),
        }
    }
}

#[test]
fn golden_create() {
    let stmt = parser::parse(r#"CREATE Person name == "Alice", age == 30"#).unwrap();
    match stmt {
        Statement::Create(c) => {
            assert_eq!(c.entity, "Person");
            assert_eq!(c.properties.len(), 2);
            assert_eq!(c.properties[0].0, "name");
            assert_eq!(c.properties[0].1, Expr::String("Alice".into()));
            assert_eq!(c.properties[1].0, "age");
            assert_eq!(c.properties[1].1, Expr::Number(30.0));
        }
        other => panic!("expected Create, got {:?}", other),
    }
}

#[test]
fn golden_update() {
    let stmt = parser::parse(r#"UPDATE Person "abc123" name == "Bob""#).unwrap();
    match stmt {
        Statement::Update(u) => {
            assert_eq!(u.entity, "Person");
            assert_eq!(u.koid, "abc123");
            assert_eq!(u.properties.len(), 1);
            assert_eq!(u.properties[0].0, "name");
            assert_eq!(u.properties[0].1, Expr::String("Bob".into()));
        }
        other => panic!("expected Update, got {:?}", other),
    }
}

#[test]
fn golden_delete() {
    let stmt = parser::parse(r#"DELETE Person "abc123""#).unwrap();
    match stmt {
        Statement::Delete(d) => {
            assert_eq!(d.entity, "Person");
            assert_eq!(d.koid, "abc123");
        }
        other => panic!("expected Delete, got {:?}", other),
    }
}

#[test]
fn golden_ingest() {
    let stmt = parser::parse(
        r#"INGEST "invoice.pdf" EXTRACT tables EXTRACT entities BUILD relationships COMMIT"#,
    )
    .unwrap();
    match stmt {
        Statement::Ingest(i) => {
            assert_eq!(i.source, "invoice.pdf");
            assert!(i.extract_tables);
            assert!(i.extract_entities);
            assert!(i.build_relationships);
        }
        other => panic!("expected Ingest, got {:?}", other),
    }
}

#[test]
fn golden_match_field_projection() {
    let stmt = parser::parse(r#"MATCH Person RETURN name, company"#).unwrap();
    let m = as_match(&stmt);
    assert_eq!(
        m.projection,
        Projection::Fields(vec!["name".into(), "company".into()])
    );
}

#[test]
fn golden_all_operators() {
    let cases = vec![
        (
            "MATCH Person WHERE a == \"x\" RETURN *",
            Predicate::Eq {
                property: "a".into(),
                value: Expr::String("x".into()),
            },
        ),
        (
            "MATCH Person WHERE a != \"x\" RETURN *",
            Predicate::Neq {
                property: "a".into(),
                value: Expr::String("x".into()),
            },
        ),
        (
            "MATCH Person WHERE a > 5 RETURN *",
            Predicate::Gt {
                property: "a".into(),
                value: Expr::Number(5.0),
            },
        ),
        (
            "MATCH Person WHERE a < 5 RETURN *",
            Predicate::Lt {
                property: "a".into(),
                value: Expr::Number(5.0),
            },
        ),
        (
            "MATCH Person WHERE a >= 5 RETURN *",
            Predicate::Gte {
                property: "a".into(),
                value: Expr::Number(5.0),
            },
        ),
        (
            "MATCH Person WHERE a <= 5 RETURN *",
            Predicate::Lte {
                property: "a".into(),
                value: Expr::Number(5.0),
            },
        ),
    ];
    for (src, expected) in cases {
        let stmt = parser::parse(src).unwrap();
        let m = as_match(&stmt);
        assert_eq!(m.predicates.len(), 1);
        assert_eq!(m.predicates[0], expected);
    }
}
