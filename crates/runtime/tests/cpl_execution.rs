//! P3-M4 compiler completion — execution REDs.
//!
//! cpl002: `TRAVERSE rel DEPTH 3` returns the 3-hop closure on a diamond.
//! cpl004: `TRAVERSE rel RETURN name` projects fields after traversal.

use aikoql_compiler::parser;
use aikoql_graph::{GraphEngineApi, RelateRequest};
use aikoql_kernel::ir::IrOp;
use aikoql_kernel::transaction::kernel::{KnowledgeContext, Subject};
use aikoql_kernel::*;
use aikoql_runtime::{Interpreter, RowSet};
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

fn ctx() -> KnowledgeContext {
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

/// Remember a Person with a `name` property; return its KOID.
fn person(k: &Kernel, name: &str) -> KOID {
    let mut req = RememberRequest::create(ctx(), meta("Person"));
    req.properties
        .insert("name".into(), Value::Text(name.into()));
    k.remember(req).unwrap().koid
}

/// Diamond: a -knows-> b, a -knows-> c, b -knows-> d, c -knows-> d, d -knows-> e.
/// Returns [a, b, c, d, e].
fn diamond(k: &Kernel) -> Vec<KOID> {
    let a = person(k, "Alice");
    let b = person(k, "Bob");
    let c = person(k, "Carol");
    let d = person(k, "Dave");
    let e = person(k, "Eve");
    for (src, tgt) in [(&a, &b), (&a, &c), (&b, &d), (&c, &d), (&d, &e)] {
        k.relate(RelateRequest::new(ctx(), *src, *tgt, "knows"))
            .unwrap();
    }
    vec![a, b, c, d, e]
}

#[test]
fn cpl002_traverse_depth_3_returns_three_hop_closure() {
    let k = mk();
    let ids = diamond(&k);
    let plan = parser::compile_with_subject(
        r#"MATCH Person WHERE name == "Alice" TRAVERSE knows DEPTH 3 RETURN *"#,
        "alice",
    )
    .unwrap();
    let result = Interpreter::execute(&k, &plan).unwrap();
    let hits: HashSet<KOID> = match result {
        RowSet::Traversal(t) => t.into_iter().map(|(koid, _, _)| koid).collect(),
        other => panic!("expected Traversal, got {:?}", other),
    };
    // 3-hop closure from a = {b, c, d, e}; a itself never appears.
    let expected: HashSet<KOID> = ids[1..].iter().copied().collect();
    assert_eq!(
        hits, expected,
        "3-hop closure must include every reachable node"
    );

    // Sanity: default depth 1 reaches only the direct neighbors {b, c}.
    let plan1 = parser::compile_with_subject(
        r#"MATCH Person WHERE name == "Alice" TRAVERSE knows RETURN *"#,
        "alice",
    )
    .unwrap();
    match Interpreter::execute(&k, &plan1).unwrap() {
        RowSet::Traversal(t) => {
            let hits1: HashSet<KOID> = t.into_iter().map(|(koid, _, _)| koid).collect();
            let expected1: HashSet<KOID> = ids[1..3].iter().copied().collect();
            assert_eq!(hits1, expected1, "depth 1 reaches only direct neighbors");
        }
        other => panic!("expected Traversal, got {:?}", other),
    }
}

#[test]
fn cpl004_projection_applied_after_traverse() {
    let k = mk();
    diamond(&k);
    let plan = parser::compile_with_subject(
        r#"MATCH Person WHERE name == "Alice" TRAVERSE knows RETURN name"#,
        "alice",
    )
    .unwrap();
    assert!(
        matches!(
            plan.operators.last(),
            Some(IrOp::Project { fields }) if fields == &vec!["name".to_string()]
        ),
        "Project must follow Traverse: {:?}",
        plan.operators
    );
    let result = Interpreter::execute(&k, &plan).unwrap();
    let kos = match result {
        RowSet::Objects(kos) => kos,
        other => panic!("expected Objects after projection, got {:?}", other),
    };
    assert_eq!(kos.len(), 2, "default depth 1: direct neighbors b, c");
    for ko in &kos {
        assert_eq!(
            ko.properties.len(),
            1,
            "projection keeps only the listed fields"
        );
        match ko.properties.get("name") {
            Some(Value::Text(n)) => assert!(["Bob", "Carol"].contains(&n.as_str())),
            other => panic!("expected name text, got {:?}", other),
        }
    }
}
