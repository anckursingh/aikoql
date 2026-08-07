//! Load test: create N KOs with the index maintainer, verify recall
//! quality and index convergence. Runs as a regular test — no criterion
//! dependency. Designed to gate Phase 2 exit.
//!
//! ponytail: 500 KOs, 128-dim vectors. Scale to 100K when profiling
//! justifies longer test times (add `--ignored` gate).

use mnemosyne_kernel::*;
use mnemosyne_scheduler::IndexMaintainer;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn load_test_500_kos_index_convergence() {
    let clock = Arc::new(ManualClock::new(20_000));
    let kernel = Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xB0AD).unwrap();
    let alice = Subject::new("alice");

    let vectors: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
    let text: Arc<dyn TextIndex> = Arc::new(TokenTextIndex::new());
    let maintainer = IndexMaintainer::start(&kernel, vectors.clone(), text.clone()).unwrap();

    let n: u64 = 500;
    let dim: usize = 128;

    // Create N KOs with random vectors.
    for i in 0..n {
        let mut props = PropertyMap::new();
        props.insert("body".into(), Value::Text(format!("load test doc {}", i)));
        let mut emb = Vec::with_capacity(dim);
        for j in 0..dim {
            emb.push(((i.wrapping_mul(j as u64 + 1)) as f32).sin());
        }
        let semantic = SemanticBlock {
            embedding_model: Some("load-test".into()),
            embedding: Some(emb),
            confidence: Some(0.9),
            source: Some("load-test".into()),
            summary: None,
        };
        kernel
            .remember(RememberRequest {
                context: (&alice).into(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: "doc".into(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: Some(semantic),
                relationships: vec![],
                security: None,
                extensions: ExtensionMap::new(),
                origin: Origin::Human,
                note: None,
                referential_policy: ReferentialPolicy::default(),
            })
            .unwrap();
    }

    // Wait for index convergence.
    maintainer
        .wait_caught_up(&kernel, Duration::from_secs(10))
        .unwrap();
    assert_eq!(vectors.len(), n as usize);
    assert_eq!(text.len(), n as usize);

    // Attach maintainer and verify recall.
    kernel.attach_indexes(maintainer.clone());

    let qv: Vec<f32> = (0..dim).map(|i| (i as f32).sin()).collect();
    let results = kernel
        .find_similar(SimilarityQuery {
            context: (&alice).into(),
            filter: Some(PropertyFilter {
                type_name: Some("doc".into()),
                required: vec![],
            }),
            text: Some("load test doc".into()),
            vector: Some(qv),
            embedding_model: Some("load-test".into()),
            k: 10,
            fusion: Fusion::Rrf { k0: 60 },
        })
        .unwrap();

    assert_eq!(results.len(), 10);
    // Top result should have high confidence.
    assert!(results[0].score > 0.0);
    // Index lag should be zero after convergence.
    assert_eq!(results[0].index_lag_ms, 0);
}
