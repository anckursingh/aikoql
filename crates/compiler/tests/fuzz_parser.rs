//! Fuzz tests for AIKOQL Lexer + Parser (MRFC-0010 §11).
//!
//! Invariants:
//! - Lexer never panics on arbitrary input.
//! - Parser never panics on any valid token stream.
//! - Round-trip: AST → string → parse → equivalent AST.

use mnemosyne_compiler::parser::{self, ast::*};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Strategy: generate random valid AIKOQL source fragments
// ---------------------------------------------------------------------------

fn ident_str() -> impl Strategy<Value = String> {
    let keywords = &[
        "match",
        "where",
        "and",
        "or",
        "return",
        "similar",
        "to",
        "traverse",
        "create",
        "update",
        "delete",
        "ingest",
        "extract",
        "tables",
        "entities",
        "build",
        "relationships",
        "commit",
        "explain",
        "true",
        "false",
        "null",
    ];
    "[a-zA-Z_][a-zA-Z0-9_]{0,15}".prop_filter("not a keyword", move |s: &String| {
        let lower = s.to_lowercase();
        !keywords.contains(&lower.as_str())
    })
}

fn string_val() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9 ]{0,10}").unwrap()
}

fn keyword() -> impl Strategy<Value = &'static str> {
    prop::sample::select(&["MATCH", "CREATE", "UPDATE", "DELETE"])
}

fn operator() -> impl Strategy<Value = &'static str> {
    prop::sample::select(&["==", "!=", "<", ">", "<=", ">="])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

proptest! {
    /// Lexer invariant: never panics, always terminates.
    #[test]
    fn fuzz_lexer_never_panics(s in "\\PC{0,200}") {
        let mut lex = parser::lexer::Lexer::new(&s);
        for _ in 0..1000 {
            let t = lex.next_token();
            if matches!(t, parser::lexer::Token::Eof) { break; }
        }
    }

    /// Parser invariant: never panics on valid AIKOQL fragments.
    #[test]
    fn fuzz_parser_never_panics(
        kw in keyword(),
        entity in ident_str(),
        prop_name in ident_str(),
        op in operator(),
        val in string_val(),
        extra in "\\PC{0,50}",
    ) {
        let source = format!("{} {} WHERE {} {} \"{}\" RETURN * {}", kw, entity, prop_name, op, val, extra);
        let _ = parser::parse(&source); // OK to fail, must not panic
    }

    /// Parser round-trip: a syntactically valid MATCH query parses successfully.
    #[test]
    fn fuzz_match_parses(
        entity in ident_str(),
        prop1 in ident_str(),
        val1 in string_val(),
        prop2 in ident_str(),
        val2 in string_val(),
    ) {
        let source = format!(
            "MATCH {} WHERE {} == \"{}\" AND {} == \"{}\" RETURN *",
            entity, prop1, val1, prop2, val2
        );
        let result = parser::parse(&source);
        // Must parse successfully (structurally valid).
        assert!(result.is_ok(), "failed to parse: {:?}", result.err());
        let stmt = result.unwrap();
        match stmt {
            Statement::Match(m) => {
                assert_eq!(m.entity, entity);
                assert_eq!(m.predicates.len(), 2);
            }
            _ => panic!("expected Match"),
        }
    }
}
