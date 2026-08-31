//! MVP-QA-002 Suite I — derived-state consistency (QA2-DER-003).
//!
//! DER-001/002/004/005 are already covered (i09/i10/i11 index rebuild
//! parity, `mvp_ko_003` graph projection drops Deleted endpoints, t35
//! stale-cache pin). DER-003 closes the vector-projection gap: embeddings
//! must reference the correct KO version — a stale embedding can never be
//! current after an update.

use aikoql_kernel::*;
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

fn sem(model: &str, embedding: Vec<f32>) -> SemanticBlock {
    SemanticBlock {
        embedding_model: Some(model.into()),
        embedding: Some(embedding),
        confidence: Some(0.9),
        source: Some("qa2-der-003".into()),
        summary: None,
    }
}

#[test]
fn w2_der_003_stale_embedding_never_current_after_update() {
    let k = mk();

    // v1: the KO and its embedding arrive together.
    let mut seed = RememberRequest::create(alice(), meta("doc"));
    seed.properties.insert("n".into(), Value::Int(0));
    seed.semantic = Some(sem("m1", vec![1.0, 2.0, 3.0]));
    let id = k.remember(seed).unwrap().koid;
    let v1_ts = k.get(alice(), &id).unwrap().commit_ts;

    // v2: content changes — the embedding must change WITH the version.
    let mut upd = RememberRequest::update(alice(), id, meta("doc"));
    upd.properties.insert("n".into(), Value::Int(1));
    upd.semantic = Some(sem("m2", vec![9.0, 8.0, 7.0]));
    k.remember(upd).unwrap();
    let v2_ts = k.get(alice(), &id).unwrap().commit_ts;

    // v3: the semantic block is dropped entirely.
    let mut upd = RememberRequest::update(alice(), id, meta("doc"));
    upd.properties.insert("n".into(), Value::Int(2));
    upd.semantic = None;
    k.remember(upd).unwrap();
    let v3_ts = k.get(alice(), &id).unwrap().commit_ts;

    // Current state: the head carries NO stale embedding — v1's [1,2,3]
    // must not surface as current.
    let head = k.get(alice(), &id).unwrap();
    assert_eq!(head.version, 3);
    assert!(
        head.semantic.is_none(),
        "stale v1/v2 embedding must not be current"
    );

    // Each historical version keeps exactly ITS OWN embedding.
    let v1 = k.raw_object_at(&id, v1_ts).unwrap().expect("v1 snapshot");
    assert_eq!(v1.version, 1);
    let s1 = v1.semantic.as_ref().expect("v1 has an embedding");
    assert_eq!(s1.embedding_model.as_deref(), Some("m1"));
    assert_eq!(s1.embedding.as_deref(), Some(&[1.0, 2.0, 3.0][..]));

    let v2 = k.raw_object_at(&id, v2_ts).unwrap().expect("v2 snapshot");
    assert_eq!(v2.version, 2);
    let s2 = v2.semantic.as_ref().expect("v2 has an embedding");
    assert_eq!(s2.embedding_model.as_deref(), Some("m2"));
    assert_eq!(s2.embedding.as_deref(), Some(&[9.0, 8.0, 7.0][..]));

    let v3 = k.raw_object_at(&id, v3_ts).unwrap().expect("v3 snapshot");
    assert_eq!(v3.version, 3);
    assert!(v3.semantic.is_none(), "v3 dropped the semantic block");

    // The audit chain stays valid across all three versions.
    assert!(k.prove(alice(), &id).unwrap().chain_valid);
}
