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
fn ann002_model_less_embeddings_reach_the_ann() {
    // The production shape: SDK/MCP remembers often carry an embedding with
    // no embedding_model. The exact path scored those (the coordinator reads
    // ko.semantic.embedding directly); the ANN adapter must not drop them —
    // a model-less vector is a ""-model entry (R7 label ":<koid_hex>"),
    // found by model-less queries and excluded by model-scoped ones.
    let (k, _c) = mk();
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.properties
        .insert("body".into(), Value::Text("cats".into()));
    req.semantic = Some(SemanticBlock {
        embedding_model: None, // the production shape under test
        embedding: Some(vec![1.0, 0.0]),
        confidence: None,
        source: None,
        summary: None,
    });
    let a = k.remember(req).unwrap().koid;
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.properties
        .insert("body".into(), Value::Text("dogs".into()));
    req.semantic = Some(SemanticBlock {
        embedding_model: None,
        embedding: Some(vec![0.0, 1.0]),
        confidence: None,
        source: None,
        summary: None,
    });
    k.remember(req).unwrap();

    let m = IndexMaintainer::start(
        &k,
        Arc::new(HnswVectorIndex::new(0, 100)),
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
        k: 2,
        fusion: Fusion::VectorOnly,
    };
    let got = k.find_similar(q).unwrap();
    assert_eq!(got.len(), 2, "model-less vectors must reach the ANN");
    assert_eq!(got[0].ko.koid, a, "the nearer vector ranks first");

    // A model-scoped query must NOT return model-less entries.
    let q = SimilarityQuery {
        context: alice().into(),
        filter: None,
        text: None,
        vector: Some(vec![1.0, 0.0]),
        embedding_model: Some("m".into()),
        k: 2,
        fusion: Fusion::VectorOnly,
    };
    let got = k.find_similar(q).unwrap();
    assert!(
        got.is_empty(),
        "model-scoped queries exclude model-less entries"
    );
    m.shutdown();
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

#[test]
fn ann004_insert_past_capacity_no_drop_full_answers() {
    // PR6 P0-10 (capacity evidence): both hosts build capacity 10_000, and
    // the review's D1 cell inserts 10_001/100k/1M through it. The capacity
    // is an ALLOCATOR HINT, not a bound — but `search` caps the internal
    // candidate pool at the INITIAL capacity, so a past-capacity graph
    // answers fewer than k. Pin: insert 64 into capacity 8, query k=10 —
    // today 8 come back.
    let idx = Arc::new(HnswVectorIndex::new(2, 8));
    for i in 0..64u8 {
        let ang = (i as f32) * 2.0 * std::f32::consts::PI / 64.0;
        idx.upsert(KOID::from_bytes([i; KOID_LEN]), "m", &[ang.cos(), ang.sin()]);
    }
    assert_eq!(idx.len(), 64, "no drop past capacity");
    let h = idx.health().unwrap();
    assert_eq!(h.physical, 64, "physical counts every inserted node");
    assert_eq!(h.live, 64, "live tracks the distinct map");

    // The recall pin: the query's own vector must rank first, and k=10
    // must answer 10 — today the candidate pool caps at capacity 8.
    let hits = idx.search(&[1.0, 0.0], 10, Some("m"));
    assert_eq!(hits.len(), 10, "k=10 must answer 10 past capacity (got {})", hits.len());
    assert_eq!(
        hits[0].0,
        KOID::from_bytes([0u8; KOID_LEN]),
        "the query's own vector ranks first"
    );
}

#[test]
fn ann005_reupsert_same_koid_model_physical_stable() {
    // PR6 P1-17 (physical accounting): the maintainer re-upserts every
    // replay pass — a repeated (koid, model) must UPDATE the vector, not
    // inflate the physical count (dead_ratio = tombstones/physical, so
    // inflation hides real dead nodes). Today every upsert increments
    // physical: the same pair twice reports 2.
    let idx = Arc::new(HnswVectorIndex::new(2, 16));
    let a = KOID::from_bytes([7u8; KOID_LEN]);
    idx.upsert(a, "m", &[1.0, 0.0]);
    idx.upsert(a, "m", &[0.0, 1.0]); // same (koid, model) — an update
    assert_eq!(idx.len(), 1, "the map holds the distinct pair once");
    let h = idx.health().unwrap();
    assert_eq!(h.physical, 1, "re-upsert must not inflate physical (got {})", h.physical);
    assert_eq!(h.live, 1);

    // The update took: the re-upsert's vector answers.
    let hits = idx.search(&[0.0, 1.0], 1, Some("m"));
    assert_eq!(hits.len(), 1, "the re-upserted koid still answers");
    assert_eq!(hits[0].0, a);

    // A different model for the same koid IS a distinct entry.
    idx.upsert(a, "n", &[1.0, 1.0]);
    let h = idx.health().unwrap();
    assert_eq!(h.physical, 2, "a new model is a new physical node");
    assert_eq!(h.live, 2);
}
