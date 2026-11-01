//! AG-23 (embedding model choice) — RED: switching models must never serve
//! mixed-dimension results. Two models with different dims coexist in the
//! vector index; model-scoped and model-less queries each answer from one
//! dim only.

use aikoql_kernel::*;
use aikoql_scheduler::IndexMaintainer;
use aikoql_vector::{ModelAwareVectorIndex, TokenTextIndex};
use std::sync::Arc;
use std::time::Duration;

fn mk() -> (Kernel, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(20_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0x1D5).unwrap();
    (k, clock)
}

fn alice() -> Subject {
    Subject::new("alice")
}

fn remember_emb(k: &Kernel, model: &str, emb: Vec<f32>) -> KOID {
    let mut req = RememberRequest::create(
        alice(),
        Metadata {
            type_name: "Memory".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("body".into(), Value::Text(model.into()));
    req.semantic = Some(SemanticBlock {
        embedding_model: Some(model.into()),
        embedding: Some(emb),
        confidence: None,
        source: None,
        summary: None,
    });
    k.remember(req).unwrap().koid
}

fn query(model: Option<&str>, vector: Vec<f32>) -> SimilarityQuery {
    SimilarityQuery {
        context: alice().into(),
        filter: None,
        text: None,
        vector: Some(vector),
        embedding_model: model.map(String::from),
        k: 10,
        fusion: Fusion::VectorOnly,
    }
}

/// The AG-23 gate: a model-scoped query answers from its model's dim, a
/// model-less query from the dim of the query vector — never a mix.
#[test]
fn ag23_switching_models_never_serves_mixed_dimensions() {
    let (k, _c) = mk();
    let small: Vec<KOID> = (0..50u32)
        .map(|i| {
            let a = (i as f32 * 0.05).to_radians();
            remember_emb(&k, "small", vec![a.cos(), a.sin()])
        })
        .collect();
    let large: Vec<KOID> = (0..30u32)
        .map(|i| {
            let a = (i as f32 * 0.05).to_radians();
            remember_emb(&k, "large", vec![a.cos(), a.sin(), 1.0, 0.5])
        })
        .collect();
    let m = IndexMaintainer::start(
        &k,
        Arc::new(ModelAwareVectorIndex::new(10_000)),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m.clone());
    m.wait_caught_up(&k, Duration::from_secs(10)).unwrap();

    let scoped = k.find_similar(query(Some("small"), vec![1.0, 0.0])).unwrap();
    assert_eq!(scoped.len(), 10);
    assert!(
        scoped.iter().all(|h| small.contains(&h.ko.koid)),
        "a small-model query must not rank large-model KOs"
    );

    let scoped = k
        .find_similar(query(Some("large"), vec![1.0, 0.0, 1.0, 0.5]))
        .unwrap();
    assert_eq!(scoped.len(), 10);
    assert!(
        scoped.iter().all(|h| large.contains(&h.ko.koid)),
        "a large-model query must not rank small-model KOs"
    );

    let dim_picked = k.find_similar(query(None, vec![1.0, 0.0])).unwrap();
    assert_eq!(dim_picked.len(), 10);
    assert!(
        dim_picked.iter().all(|h| small.contains(&h.ko.koid)),
        "a model-less 2-d query answers from the 2-d model only — never mixed dims"
    );
}
