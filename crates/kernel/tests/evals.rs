//! Memory Evals acceptance suite — recall, staleness, contradiction metrics as queries.

use mnemosyne_kernel::{
    EvalContradictionQuery, EvalRecallQuery, EvalStalenessQuery, Fusion, Kernel, KnowledgeContext,
    ManualClock, RememberRequest, Subject,
};
use mnemosyne_kernel::{SemanticBlock, TokenTextIndex, Value};
use mnemosyne_scheduler::IndexMaintainer;
use std::sync::Arc;

fn meta(t: &str) -> mnemosyne_kernel::Metadata {
    mnemosyne_kernel::Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn fact(k: &Kernel, body: &str, embedding: &[f32]) -> mnemosyne_kernel::KOID {
    let mut req = RememberRequest::create(Subject::new("eval"), meta("fact"));
    req.properties
        .insert("body".into(), Value::Text(body.into()));
    req.semantic = Some(SemanticBlock {
        embedding_model: Some("test".into()),
        embedding: Some(embedding.to_vec()),
        confidence: None,
        source: None,
        summary: None,
    });
    k.remember(req).unwrap().koid
}

#[test]
fn e01_recall_at_k_against_expected_set() {
    let clock = Arc::new(ManualClock::new(1_000));
    let k = Kernel::open(Arc::new(mnemosyne_kernel::MemoryEngine::new()), clock, 1).unwrap();
    let a = fact(&k, "red ball", &[1.0, 0.0]);
    let b = fact(&k, "blue cube", &[0.0, 1.0]);
    let c = fact(&k, "red cube", &[0.9, 0.1]);

    let q = EvalRecallQuery {
        context: KnowledgeContext::new(Subject::new("eval")),
        type_name: Some("fact".into()),
        text: Some("red".into()),
        k: 5,
        fusion: Fusion::TextOnly,
        expected: [a, b, c].iter().copied().collect(),
        ..Default::default()
    };
    let r = k.eval_recall(q).unwrap();
    assert_eq!(r.k, 5);
    assert_eq!(r.hits, 3);
    assert!((r.recall - 1.0).abs() < 1e-6);
    assert_eq!(r.max_lag_ms, 0);
}

#[test]
fn e02_staleness_reports_lag_distribution() {
    let clock = Arc::new(ManualClock::new(1_000));
    let engine = Arc::new(mnemosyne_kernel::MemoryEngine::new());
    let k = Kernel::open(engine.clone(), clock.clone(), 1).unwrap();

    // Without indexes the exact path reports zero lag.
    fact(&k, "hello world", &[1.0, 0.0]);
    let r = k
        .eval_staleness(EvalStalenessQuery {
            context: KnowledgeContext::new(Subject::new("eval")),
            text: Some("hello".into()),
            k: 5,
            fusion: Fusion::TextOnly,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.results, 1);
    assert_eq!(r.mean_lag_ms, 0);
    assert_eq!(r.max_lag_ms, 0);
    assert_eq!(r.p95_lag_ms, 0);

    // With an async maintainer attached but empty at start, a new commit is
    // still being processed so lag is reported as non-negative (often > 0).
    let vec_idx = Arc::new(mnemosyne_kernel::BruteForceVectorIndex::new());
    let txt_idx = Arc::new(TokenTextIndex::new());
    let maintainer = IndexMaintainer::start(&k, vec_idx.clone(), txt_idx.clone()).unwrap();
    k.attach_indexes(maintainer.clone());
    maintainer
        .wait_caught_up(&k, std::time::Duration::from_millis(100))
        .unwrap();

    fact(&k, "goodbye world", &[0.0, 1.0]);
    let r2 = k
        .eval_staleness(EvalStalenessQuery {
            context: KnowledgeContext::new(Subject::new("eval")),
            text: Some("goodbye".into()),
            k: 5,
            fusion: Fusion::TextOnly,
            ..Default::default()
        })
        .unwrap();
    assert!(r2.results >= 1);
    // lag may be zero if the maintainer already caught up; the metric is still
    // valid because it reports whatever the recall path observed.
    assert!(r2.max_lag_ms >= r2.mean_lag_ms);
}

#[test]
fn e03_contradictions_between_similar_claims() {
    let clock = Arc::new(ManualClock::new(1_000));
    let k = Kernel::open(Arc::new(mnemosyne_kernel::MemoryEngine::new()), clock, 1).unwrap();

    let mut yes = RememberRequest::create(Subject::new("eval"), meta("claim"));
    yes.properties
        .insert("claim".into(), Value::Text("The sky is blue.".into()));
    yes.properties.insert("answer".into(), Value::Bool(true));
    yes.semantic = Some(SemanticBlock {
        embedding_model: Some("test".into()),
        embedding: Some(vec![1.0, 0.0]),
        confidence: None,
        source: None,
        summary: None,
    });
    let a = k.remember(yes).unwrap().koid;

    let mut no = RememberRequest::create(Subject::new("eval"), meta("claim"));
    no.properties
        .insert("claim".into(), Value::Text("The sky is not blue.".into()));
    no.properties.insert("answer".into(), Value::Bool(false));
    no.semantic = Some(SemanticBlock {
        embedding_model: Some("test".into()),
        embedding: Some(vec![0.99, 0.01]),
        confidence: None,
        source: None,
        summary: None,
    });
    let b = k.remember(no).unwrap().koid;

    let hits = k
        .eval_contradictions(EvalContradictionQuery {
            context: KnowledgeContext::new(Subject::new("eval")),
            type_name: "claim".into(),
            property: "answer".into(),
            similarity_threshold: 0.9,
            max_results: 10,
        })
        .unwrap();
    assert_eq!(hits.len(), 1);
    let pair = &hits[0];
    assert!(pair.score >= 0.9);
    assert!((pair.left == a && pair.right == b) || (pair.left == b && pair.right == a));
    assert!(pair.reason.contains("answer"));
}
