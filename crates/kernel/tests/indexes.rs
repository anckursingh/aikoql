//! Index lifecycle acceptance suite (MRFC-0009 semantics).
//!
//! Gates:
//! - maintainer catch-up replay builds indexes from the journal
//! - live commits are applied via notify (water mark advances)
//! - find_similar through indexes is score-identical to the exact path (parity)
//! - index_lag is reported and returns to 0 after catch-up
//! - forget removes documents from recall through the index path

use aikoql_kernel::*;
use aikoql_scheduler::IndexMaintainer;
use aikoql_vector::{HnswVectorIndex, TantivyTextIndex};
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

fn query(k: &Kernel) -> Vec<ScoredKO> {
    k.find_similar(SimilarityQuery {
        context: alice().into(),
        filter: None,
        text: Some("cats".into()),
        vector: Some(vec![1.0, 0.0]),
        embedding_model: None,
        k: 5,
        fusion: Fusion::Rrf { k0: 60 },
    })
    .unwrap()
}

#[test]
fn i01_catchup_replay_indexes_existing_journal() {
    let (k, _c) = mk();
    let a = create_vec(&k, "cats and dogs", vec![1.0, 0.0]);
    let _b = create_vec(&k, "birds", vec![0.0, 1.0]);

    let m = IndexMaintainer::start(
        &k,
        Arc::new(BruteForceVectorIndex::new()),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    // replay happened synchronously in start()
    assert_eq!(m.water(), 2);
    assert_eq!(m.vectors().len(), 2);
    assert_eq!(m.text().len(), 2);
    let hits = m.vectors().search(&[1.0, 0.0], 1, None);
    assert_eq!(hits[0].0, a);
    m.shutdown();
}

#[test]
fn i02_live_commits_applied_and_lag_returns_to_zero() {
    let (k, _c) = mk();
    let m = IndexMaintainer::start(
        &k,
        Arc::new(BruteForceVectorIndex::new()),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m.clone());

    let a = create_vec(&k, "cats rule", vec![1.0, 0.0]);
    m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
    assert_eq!(m.lag(&k).unwrap(), 0);
    assert_eq!(m.vectors().len(), 1);

    let res = query(&k);
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].ko.koid, a);
    assert_eq!(
        res[0].index_lag_ms, 0,
        "caught-up maintainer must report zero lag"
    );
    m.shutdown();
}

#[test]
fn i03_indexed_recall_is_score_identical_to_exact_path() {
    let (k, _c) = mk();
    let _a = create_vec(&k, "cats", vec![1.0, 0.0]);
    let _b = create_vec(&k, "cats and dogs", vec![0.9, 0.1]);
    let _c = create_vec(&k, "unrelated fish", vec![0.0, 1.0]);

    // exact path (no indexes)
    let exact: Vec<(KOID, f32)> = query(&k)
        .into_iter()
        .map(|s| (s.ko.koid, s.score))
        .collect();

    // indexed path
    let m = IndexMaintainer::start(
        &k,
        Arc::new(BruteForceVectorIndex::new()),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m.clone());
    m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
    let indexed: Vec<(KOID, f32)> = query(&k)
        .into_iter()
        .map(|s| (s.ko.koid, s.score))
        .collect();

    assert_eq!(exact.len(), indexed.len());
    for (e, i) in exact.iter().zip(indexed.iter()) {
        assert_eq!(e.0, i.0, "ordering must match exact path");
        assert!(
            (e.1 - i.1).abs() < 1e-6,
            "scores must match exact path: {} vs {}",
            e.1,
            i.1
        );
    }
    m.shutdown();
}

#[test]
fn i04_forget_removes_from_indexed_recall() {
    let (k, _c) = mk();
    let a = create_vec(&k, "cats", vec![1.0, 0.0]);
    let m = IndexMaintainer::start(
        &k,
        Arc::new(BruteForceVectorIndex::new()),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m.clone());
    m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
    assert_eq!(m.vectors().len(), 1);

    k.forget(&alice(), &a, ForgetMode::Tombstone, None, None)
        .unwrap();
    m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
    assert_eq!(m.vectors().len(), 0);
    assert_eq!(m.text().len(), 0);
    assert!(query(&k).is_empty());
    m.shutdown();
}

#[test]
fn i05_maintainer_recovers_from_existing_data_on_restart() {
    let (k, _c) = mk();
    let _a = create_vec(&k, "cats", vec![1.0, 0.0]);
    {
        let m1 = IndexMaintainer::start(
            &k,
            Arc::new(BruteForceVectorIndex::new()),
            Arc::new(TokenTextIndex::new()),
        )
        .unwrap();
        m1.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
        m1.shutdown();
    }
    // "restart": a fresh maintainer must rebuild purely from the journal
    let m2 = IndexMaintainer::start(
        &k,
        Arc::new(BruteForceVectorIndex::new()),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    assert_eq!(m2.vectors().len(), 1);
    assert_eq!(m2.water(), 1);
    m2.shutdown();
}

#[test]
fn i06_hnsw_recall_parity_with_exact_path() {
    let (k, _c) = mk();
    let _a = create_vec(&k, "cats", vec![1.0, 0.0]);
    let _b = create_vec(&k, "cats and dogs", vec![0.9, 0.1]);
    let _c = create_vec(&k, "unrelated fish", vec![0.0, 1.0]);

    // exact path (no indexes)
    let exact: Vec<(KOID, f32)> = query(&k)
        .into_iter()
        .map(|s| (s.ko.koid, s.score))
        .collect();

    // HNSW indexed path
    let m = IndexMaintainer::start(
        &k,
        Arc::new(HnswVectorIndex::new(2, 100)),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m.clone());
    m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
    let indexed: Vec<(KOID, f32)> = query(&k)
        .into_iter()
        .map(|s| (s.ko.koid, s.score))
        .collect();

    // ANN parity gate: top-k overlap and score tolerance, not bit-exact.
    assert!(!indexed.is_empty(), "HNSW path must return results");
    let overlap = exact
        .iter()
        .zip(indexed.iter())
        .filter(|(e, i)| e.0 == i.0)
        .count();
    assert!(
        overlap >= exact.len().saturating_sub(1),
        "HNSW must recall at least top_k-1 of exact results; got {} of {}",
        overlap,
        exact.len()
    );
    for (e, i) in exact.iter().zip(indexed.iter()).filter(|(e, i)| e.0 == i.0) {
        assert!(
            (e.1 - i.1).abs() < 1e-3,
            "score drift too large: {} vs {}",
            e.1,
            i.1
        );
    }
    m.shutdown();
}

#[test]
fn i07_tantivy_text_recall_parity_with_exact_path() {
    let (k, _c) = mk();
    let _a = create_vec(&k, "cats", vec![1.0, 0.0]);
    let _b = create_vec(&k, "cats and dogs", vec![0.9, 0.1]);
    let _c = create_vec(&k, "unrelated fish", vec![0.0, 1.0]);

    // exact path (no indexes)
    let exact: Vec<KOID> = query(&k).into_iter().map(|s| s.ko.koid).collect();

    // Tantivy/BM25 text index + brute-force vector index
    let m = IndexMaintainer::start(
        &k,
        Arc::new(BruteForceVectorIndex::new()),
        Arc::new(TantivyTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m.clone());
    m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
    let indexed: Vec<KOID> = query(&k).into_iter().map(|s| s.ko.koid).collect();

    // BM25 changes scoring, so parity is recall-based, not score-identical.
    assert!(!indexed.is_empty(), "Tantivy path must return results");
    let overlap = exact
        .iter()
        .zip(indexed.iter())
        .filter(|(e, i)| e == i)
        .count();
    assert!(
        overlap >= exact.len().saturating_sub(1),
        "Tantivy must recall at least top_k-1 of exact results; got {} of {}",
        overlap,
        exact.len()
    );
    m.shutdown();
}

#[test]
fn i08_checkpoint_resume_skips_replay_and_keeps_live_apply() {
    let (k, _c) = mk();
    let _a = create_vec(&k, "cats", vec![1.0, 0.0]);
    let _b = create_vec(&k, "cats and dogs", vec![0.9, 0.1]);

    let vectors: Arc<HnswVectorIndex> = Arc::new(HnswVectorIndex::new(2, 100));
    let text: Arc<TantivyTextIndex> = Arc::new(TantivyTextIndex::new());
    let m1 = IndexMaintainer::start(
        &k,
        vectors.clone() as Arc<dyn VectorIndex>,
        text.clone() as Arc<dyn TextIndex>,
    )
    .unwrap();
    m1.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
    assert_eq!(m1.water(), 2);

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let checkpoint_dir = std::env::temp_dir().join(format!("aikoql-i08-{}", stamp));
    m1.checkpoint(&checkpoint_dir).unwrap();
    m1.shutdown();

    let water = IndexMaintainer::checkpoint_water(&checkpoint_dir)
        .unwrap()
        .expect("checkpoint must have a water mark");
    assert_eq!(water, 2);

    let vectors2: Arc<HnswVectorIndex> =
        Arc::new(HnswVectorIndex::load(&checkpoint_dir.join("vectors")).unwrap());
    let text2: Arc<TantivyTextIndex> =
        Arc::new(TantivyTextIndex::load(&checkpoint_dir.join("text")).unwrap());
    let m2 = IndexMaintainer::start_at(
        &k,
        vectors2.clone() as Arc<dyn VectorIndex>,
        text2.clone() as Arc<dyn TextIndex>,
        Some(water),
    )
    .unwrap();
    k.attach_indexes(m2.clone());

    assert_eq!(m2.water(), 2);
    assert_eq!(m2.vectors().len(), 2);
    assert_eq!(m2.text().len(), 2);

    let before = query(&k).len();
    assert!(
        before >= 2,
        "checkpointed indexes must still answer queries"
    );

    let _c = create_vec(&k, "cats everywhere", vec![0.95, 0.05]);
    m2.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
    assert_eq!(m2.water(), 3);
    assert_eq!(m2.vectors().len(), 3);

    m2.shutdown();
    let _ = std::fs::remove_dir_all(&checkpoint_dir);
}
