//! AG-20b (filtered ANN) — RED suite: a vector query narrowed by a
//! selective type filter must score a sublinear number of candidates, not
//! widen the ANN pool until it covers the whole index. Gate: results match
//! the exact oracle over the filtered set, and the distance-computation
//! counter stays sublinear in the object count.

use aikoql_kernel::*;
use aikoql_scheduler::IndexMaintainer;
use aikoql_vector::{HnswVectorIndex, TokenTextIndex};
use std::sync::Arc;
use std::time::Duration;

fn mk() -> (Kernel, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(20_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0x1D4).unwrap();
    (k, clock)
}

fn alice() -> Subject {
    Subject::new("alice")
}

fn remember_emb(k: &Kernel, ty: &str, emb: Vec<f32>) -> KOID {
    let mut req = RememberRequest::create(
        alice(),
        Metadata {
            type_name: ty.into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties.insert("body".into(), Value::Text(ty.into()));
    req.semantic = Some(SemanticBlock {
        embedding_model: None,
        embedding: Some(emb),
        confidence: None,
        source: None,
        summary: None,
    });
    k.remember(req).unwrap().koid
}

fn seed(k: &Kernel) -> Vec<KOID> {
    let mut rare = Vec::new();
    for i in 0..5_000u32 {
        let a = (i as f32 * 0.013).to_radians();
        remember_emb(k, "Bulk", vec![a.cos(), a.sin()]);
    }
    // Spread the Rare vectors across the whole angle range: clustered at the
    // query's near end, the first ANN round would already hold all of them
    // and the widening loop would never be exercised.
    for i in 0..20u32 {
        let a = (i as f32 * 3.2).to_radians();
        rare.push(remember_emb(k, "Rare", vec![a.cos(), a.sin()]));
    }
    rare
}

fn filtered_query(k: u32) -> SimilarityQuery {
    SimilarityQuery {
        context: alice().into(),
        filter: Some(PropertyFilter {
            type_name: Some("Rare".into()),
            required: vec![],
        }),
        text: None,
        vector: Some(vec![1.0, 0.0]),
        embedding_model: None,
        k: k as usize,
        fusion: Fusion::VectorOnly,
    }
}

/// RED pre-fix: a 0.4% type filter widens the candidate pool until it
/// covers the whole 5020-object index (~7.5K scored candidates). The
/// filtered set itself is 20 objects — exact over it is both sublinear
/// and the oracle.
#[test]
fn ag20b_selective_type_filter_scores_sublinear_candidates() {
    let (k, _c) = mk();
    let rare = seed(&k);
    let m = IndexMaintainer::start(
        &k,
        Arc::new(HnswVectorIndex::new(0, 100)),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m.clone());
    m.wait_caught_up(&k, Duration::from_secs(10)).unwrap();

    let before = k.vector_distance_probes();
    let hits = k.find_similar(filtered_query(10)).unwrap();
    let probes = k.vector_distance_probes() - before;

    assert_eq!(hits.len(), 10);
    for h in &hits {
        assert!(
            rare.contains(&h.ko.koid),
            "a Bulk KO leaked into a type-filtered query"
        );
    }
    assert!(
        probes < 1_000,
        "filtered query scored {probes} candidates for a 20/5020 type split"
    );
}

/// Guard: the unfiltered path keeps the bounded pool (one 512-candidate
/// round) — the fix must not push everything through the exact leg.
#[test]
fn ag20b_unfiltered_query_keeps_the_bounded_pool() {
    let (k, _c) = mk();
    seed(&k);
    let m = IndexMaintainer::start(
        &k,
        Arc::new(HnswVectorIndex::new(0, 100)),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m.clone());
    m.wait_caught_up(&k, Duration::from_secs(10)).unwrap();

    let mut q = filtered_query(10);
    q.filter = None;
    let before = k.vector_distance_probes();
    let hits = k.find_similar(q).unwrap();
    let probes = k.vector_distance_probes() - before;

    assert_eq!(hits.len(), 10);
    assert!(
        probes <= 512,
        "unfiltered query scored {probes} candidates — expected the bounded pool"
    );
}
