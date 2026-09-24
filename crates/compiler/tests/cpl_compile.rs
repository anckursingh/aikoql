//! P3-M4 compiler completion — compile-level REDs (cpl001, cpl003, cpl005,
//! cpl006). Execution REDs (cpl002, cpl004) live in crates/runtime/tests/
//! cpl_execution.rs; cpl007 is the golden_snapshots + grammar_coverage +
//! fuzz_parser extension.

use aikoql_compiler::parser;
use aikoql_compiler::Compiler;
use aikoql_kernel::ir::IrOp;

#[test]
fn cpl001_ingest_lowers_to_ingest_op() {
    let plan = parser::compile(r#"INGEST "sec-filing.pdf" COMMIT"#).unwrap();
    assert_eq!(plan.operators.len(), 1);
    match &plan.operators[0] {
        IrOp::Ingest { artifact_ref } => assert_eq!(artifact_ref, "sec-filing.pdf"),
        other => panic!("expected IngestOp, got {:?}", other),
    }
}

#[test]
fn cpl003_absent_depth_defaults_to_one() {
    let plan = parser::compile("MATCH Person TRAVERSE knows RETURN *").unwrap();
    match &plan.operators[1] {
        IrOp::Traverse { depth, .. } => assert_eq!(*depth, 1),
        other => panic!("expected Traverse, got {:?}", other),
    }
}

#[test]
fn cpl005_depth_zero_is_a_semantic_error() {
    let err = parser::compile("MATCH Person TRAVERSE knows DEPTH 0 RETURN *").unwrap_err();
    assert!(
        err.contains("AIKOQL1034"),
        "expected semantic depth error, got: {}",
        err
    );
    // Negative depth never lexes as a count — rejected before lowering.
    assert!(parser::compile("MATCH Person TRAVERSE knows DEPTH -1 RETURN *").is_err());
}

#[test]
fn cpl006_json_frontend_depth_parity() {
    // Text frontend: TRAVERSE knows DEPTH 3 → depth 3.
    let text = parser::compile("MATCH Person TRAVERSE knows DEPTH 3 RETURN *").unwrap();
    match &text.operators[1] {
        IrOp::Traverse { depth, .. } => assert_eq!(*depth, 3),
        other => panic!("expected Traverse, got {:?}", other),
    }
    // JSON frontend: explicit depth 3 and default depth 1 carry through
    // identically to the text frontend.
    let json3 = Compiler::compile(
        r#"{"traverse": {"start": "abcdef1234567890abcdef1234567890", "rel_type": "knows", "depth": 3}}"#,
    )
    .unwrap();
    match &json3.operators[0] {
        IrOp::Traverse { depth, .. } => assert_eq!(*depth, 3),
        other => panic!("expected Traverse, got {:?}", other),
    }
    let json_default = Compiler::compile(
        r#"{"traverse": {"start": "abcdef1234567890abcdef1234567890", "rel_type": "knows"}}"#,
    )
    .unwrap();
    match &json_default.operators[0] {
        IrOp::Traverse { depth, .. } => assert_eq!(*depth, 1),
        other => panic!("expected Traverse, got {:?}", other),
    }
}
