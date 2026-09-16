//! P5-M18 (ANN production) — RED suite: powering the HNSW in the production
//! hosts must not corrupt ranking. Today the coordinator's vector leg builds
//! `vmap` from `vectors().search(qv, usize::MAX, model)` — the HNSW truncates
//! that to `capacity` candidates, so every head outside the candidate set
//! ("vmap hole") scores 0.0. A hole outranks any real candidate whose cosine
//! is negative: the returned top-k mixes in non-neighbors with bogus scores.

use aikoql_kernel::*;
use aikoql_scheduler::IndexMaintainer;
use aikoql_vector::HnswVectorIndex;
use std::sync::Arc;
use std::time::Duration;

fn mk() -> (Kernel, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(20_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0x1D4).unwrap();
    (k, clock)
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

fn create_vec(k: &Kernel, body: &str, emb: Vec<f32>) -> KOID {
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.properties
        .insert("body".into(), Value::Text(body.into()));
    req.semantic = Some(SemanticBlock {
        embedding_model: Some("m".into()),
        embedding: Some(emb),
        confidence: None,
        source: None,
        summary: None,
    });
    k.remember(req).unwrap().koid
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

#[test]
fn ann001_index_holes_never_outrank_real_candidates() {
    let (k, _c) = mk();
    // 4 objects near the query direction (cosines ≈ 1.0, 0.995, 0.98, 0.95)
    // and 26 antipodal objects (cosine exactly -1.0). True top-5 = the 4 near
    // + one antipodal at -1.0. HNSW capacity 10 → the coordinator's vmap
    // covers at most 10 koids; the ~20 antipodal holes score 0.0, which
    // outranks the -1.0 candidates: a hole lands in the top-5 with a score
    // that is not its true cosine. RED fails for that stated reason.
    let mut koids: Vec<KOID> = Vec::new();
    let near = [(1.0f32, 0.0f32), (0.995, 0.1), (0.98, 0.2), (0.95, 0.3)];
    for (x, y) in near {
        koids.push(create_vec(&k, "near", vec![x, y]));
    }
    for i in 0..26u32 {
        koids.push(create_vec(
            &k,
            &format!("far{}", i),
            vec![-(0.5 + i as f32 / 26.0), 0.0],
        ));
    }
    let m = IndexMaintainer::start(
        &k,
        Arc::new(HnswVectorIndex::new(2, 10)),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m.clone());
    m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();

    let q = SimilarityQuery {
        context: alice().into(),
        filter: None,
        text: None,
        vector: Some(vec![1.0, 0.0]),
        embedding_model: None,
        k: 5,
        fusion: Fusion::VectorOnly,
    };
    let got = k.find_similar(q).unwrap();
    assert_eq!(got.len(), 5, "vector-only k=5 over 30 indexed objects");
    for s in &got {
        let idx = koids
            .iter()
            .position(|ko| *ko == s.ko.koid)
            .expect("returned koid must be a fixture object");
        let true_vec = if idx < 4 {
            let (x, y) = near[idx];
            vec![x, y]
        } else {
            vec![-(0.5 + (idx as f32 - 4.0) / 26.0), 0.0]
        };
        let true_cos = cosine(&[1.0, 0.0], &true_vec);
        assert!(
            (s.score - true_cos).abs() < 1e-2,
            "returned score must be the koid's true cosine (holes score 0.0): \
             {} has score {} vs true {}",
            s.ko.koid,
            s.score,
            true_cos
        );
    }
    m.shutdown();
}
