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

/// Deterministic one-hot embedding table for the N1 similarity pins
/// (same hand-computable scheme as hybrid_h_suite).
struct OneHot;

impl EmbeddingProvider for OneHot {
    fn embed(&self, text: &str, _model: Option<&str>) -> KResult<Vec<f32>> {
        Ok(match text {
            "cats" => vec![1.0, 0.0],
            _ => vec![0.70710677, 0.70710677],
        })
    }
}

/// Seeded notes: `body` drives text scoring, `device_id` is the projection
/// target, the one-hot embedding drives vector scoring. Returns the koids.
fn seeded_notes(k: &Kernel) -> Vec<KOID> {
    let mut out = Vec::new();
    for (body, device_id, emb) in [
        ("cats", "dev_a", vec![1.0f32, 0.0]),
        ("dogs", "dev_b", vec![0.70710677, 0.70710677]),
        ("fish", "dev_c", vec![0.0, 1.0]),
    ] {
        let mut req = RememberRequest::create(ctx(), meta("note"));
        req.properties
            .insert("body".into(), Value::Text(body.into()));
        req.properties
            .insert("device_id".into(), Value::Text(device_id.into()));
        req.semantic = Some(SemanticBlock {
            embedding: Some(emb),
            embedding_model: None,
            summary: None,
            confidence: None,
            source: None,
        });
        out.push(k.remember(req).unwrap().koid);
    }
    out
}

fn objects_of(rows: RowSet) -> Vec<KnowledgeObject> {
    match rows {
        RowSet::Objects(kos) => kos,
        other => panic!("expected Objects, got {:?}", other),
    }
}

#[test]
fn cpl005_projection_applied_after_similarity() {
    // Device-eval N1: similarity legs produce Scored rows; projection must
    // load the KO each row refers to instead of failing.
    let k = mk().with_embedding_provider(Arc::new(OneHot));
    let ids = seeded_notes(&k);

    // Text path: Jaccard scores every note (k=10), rank order survives
    // projection — the best text match first.
    let plan =
        parser::compile_with_subject(r#"MATCH note SIMILAR TO "cats" RETURN device_id"#, "alice")
            .unwrap();
    let kos = objects_of(Interpreter::execute(&k, &plan).unwrap());
    assert_eq!(kos.len(), 3, "Jaccard scores every seeded note");
    for ko in &kos {
        assert_eq!(
            ko.properties.len(),
            1,
            "projection keeps only the listed fields"
        );
        match ko.properties.get("device_id") {
            Some(Value::Text(d)) => assert!(["dev_a", "dev_b", "dev_c"].contains(&d.as_str())),
            other => panic!("expected device_id text, got {:?}", other),
        }
    }
    assert_eq!(
        kos[0].properties.get("device_id"),
        Some(&Value::Text("dev_a".into())),
        "rank order survives projection"
    );

    // Embedding path: USING EMBEDDING fuses ANN + text via RRF — only
    // score>0 rows survive (cats, dogs); projection works the same.
    let plan = parser::compile_with_subject(
        r#"MATCH note SIMILAR TO "cats" USING EMBEDDING RETURN device_id"#,
        "alice",
    )
    .unwrap();
    let kos = objects_of(Interpreter::execute(&k, &plan).unwrap());
    assert_eq!(kos.len(), 2, "RRF drops the zero-score fish");
    assert_eq!(
        kos[0].properties.get("device_id"),
        Some(&Value::Text("dev_a".into()))
    );
    assert_eq!(
        kos[1].properties.get("device_id"),
        Some(&Value::Text("dev_b".into()))
    );

    // RETURN koid: KO identity survives projection even though koid is
    // not a property.
    let plan = parser::compile_with_subject(r#"MATCH note SIMILAR TO "cats" RETURN koid"#, "alice")
        .unwrap();
    let kos = objects_of(Interpreter::execute(&k, &plan).unwrap());
    assert_eq!(kos.len(), 3);
    let got: HashSet<KOID> = kos.iter().map(|ko| ko.koid).collect();
    let want: HashSet<KOID> = ids.iter().copied().collect();
    assert_eq!(got, want, "koid rows carry the seeded KO identities");
}
