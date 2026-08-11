//! Benchmark: hybrid recall (vector + text + RRF fusion).
//!
//! Measures P50/P99 latency for `find_similar` at increasing dataset sizes.
//! Run with: `cargo bench -p aikoql-benchmarks`
//!
//! Hardware note: results are machine-dependent. Gate thresholds must be
//! defined on a fixed instance type (per MRFC-0005 §NFRs).

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use aikoql_kernel::*;

use std::sync::Arc;
use std::time::Duration;

fn bench_hybrid_recall(c: &mut Criterion) {
    let mut group = c.benchmark_group("hybrid_recall");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(10));

    for n in [100u64, 1000, 10_000] {
        let clock = Arc::new(ManualClock::new(20_000));
        let kernel = Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xC0FFEE).unwrap();
        let alice = Subject::new("alice");

        // Populate N KOs with random 128-dim vectors.
        for i in 0..n {
            let mut props = PropertyMap::new();
            props.insert("body".into(), Value::Text(format!("document {}", i)));
            let mut emb = Vec::with_capacity(128);
            for j in 0..128 {
                emb.push(((i.wrapping_mul(j + 1)) as f32).sin());
            }
            let semantic = SemanticBlock {
                embedding_model: Some("bench".into()),
                embedding: Some(emb),
                confidence: Some(0.9),
                source: Some("bench".into()),
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

        // Warm up: run find_similar once.
        let qv: Vec<f32> = (0..128).map(|i| (i as f32).sin()).collect();
        kernel
            .find_similar(SimilarityQuery {
                context: (&alice).into(),
                filter: Some(PropertyFilter {
                    type_name: Some("doc".into()),
                    required: vec![],
                }),
                text: Some("document".into()),
                vector: Some(qv.clone()),
                embedding_model: Some("bench".into()),
                k: 10,
                fusion: Fusion::Rrf { k0: 60 },
            })
            .unwrap();

        group.bench_function(format!("n={}", n), |b| {
            b.iter(|| {
                black_box(
                    kernel
                        .find_similar(SimilarityQuery {
                            context: (&alice).into(),
                            filter: Some(PropertyFilter {
                                type_name: Some("doc".into()),
                                required: vec![],
                            }),
                            text: Some("document".into()),
                            vector: Some(qv.clone()),
                            embedding_model: Some("bench".into()),
                            k: 10,
                            fusion: Fusion::Rrf { k0: 60 },
                        })
                        .unwrap(),
                )
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_hybrid_recall);
criterion_main!(benches);
