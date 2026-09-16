//! P5-M16 — vector recall without corpus materialization. vs_scan_001–003.
//!
//! The exact-path `find_similar` (no maintainer — the SDK/benchmark default)
//! materializes every head KO (ACL check + deserialize) before scoring, so a
//! k=10 query over a 1 000-object corpus pays 1 000 point-reads. The M16 path
//! scores from a slim per-object record (the wire blob's own bytes — the
//! vector leg's representation) and materializes only the candidates that
//! survive ranking, in rank order, until k pass the full-KO ACL re-check.
//!
//! Detection power: 002 fails to COMPILE pre-impl (E0599 on the
//! `similarity_materializations` probe seam — the only honest observable of
//! how many KOs the query materialized). 001/003 are guard pins that fail a
//! BROKEN wiring, not the pre-impl state: a slim path that misread ACL or
//! filters would drop or leak rows (001 — hand-computed oracle incl. an
//! ACL-denied near-match and a no-embedding row), and index_lag_ms must keep
//! the P4-M7 exact/eventual contract on the new path (003).

use aikoql_kernel::index::{BruteForceVectorIndex, IndexMaintainerApi, TokenTextIndex};
use aikoql_kernel::knowledge::kom::{Metadata, SemanticBlock, Value};
use aikoql_kernel::storage::store::MemoryEngine;
use aikoql_kernel::transaction::kernel::{
    Fusion, Kernel, KnowledgeContext, ManualClock, RememberRequest, SimilarityQuery, Subject,
};
use aikoql_kernel::*;
use std::sync::Arc;

// --- helpers ------------------------------------------------------------------

fn mk() -> Kernel {
    Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap()
}

fn ctx(name: &str) -> KnowledgeContext {
    KnowledgeContext::new(Subject::new(name))
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

/// A note owned by `owner` with an optional 2-d embedding.
fn note(k: &Kernel, owner: &KnowledgeContext, body: &str, emb: Option<[f32; 2]>) -> KOID {
    let mut req = RememberRequest::create(owner.clone(), meta("note"));
    req.properties
        .insert("body".into(), Value::Text(body.into()));
    if let Some(e) = emb {
        req.semantic = Some(SemanticBlock {
            embedding_model: Some("bge-m3".into()),
            embedding: Some(e.to_vec()),
            confidence: None,
            source: None,
            summary: None,
        });
    }
    k.remember(req).unwrap().koid
}

fn vector_only(subject: &KnowledgeContext, qv: [f32; 2], k: usize) -> SimilarityQuery {
    SimilarityQuery::new(subject.clone(), k, Fusion::VectorOnly).with_vector(qv.to_vec())
}

/// Seeded corpus: alice owns a=[1,0], b=[0,1], c=[0.5,0.5], d = no
/// embedding; bob owns x=[0.99,0.01] — a slightly better match that alice
/// must never see. Returns (a, b, c, x).
fn seeded(k: &Kernel) -> (KOID, KOID, KOID, KOID) {
    let alice = ctx("alice");
    let bob = ctx("bob");
    let a = note(k, &alice, "a", Some([1.0, 0.0]));
    let b = note(k, &alice, "b", Some([0.0, 1.0]));
    let c = note(k, &alice, "c", Some([0.5, 0.5]));
    let _d = note(k, &alice, "d", None); // no embedding — scores 0
    let x = note(k, &bob, "x", Some([0.99, 0.01])); // ACL-denied for alice
    (a, b, c, x)
}

// --- vs_scan_001 — ranking parity incl. ACL-denied and no-embedding rows -------

#[test]
fn vs_scan_001_ranking_parity_with_acl_denied_and_embeddingless_rows() {
    let k = mk();
    let (a, _b, c, x) = seeded(&k);
    let alice = ctx("alice");

    // Hand-computed oracle for query [0.9, 0.1], k=2 (|q| = sqrt(0.82)):
    //   cos(a=[1,0])      = 0.9 / 0.9055        ≈ 0.9939 — the answer
    //   cos(c=[0.5,0.5])  = 0.5 / 0.6403        ≈ 0.7809 — second
    //   cos(b=[0,1])      = 0.1 / 0.9055        ≈ 0.1104
    //   cos(x=[0.99,0.01]) ≈ 0.9951 — would win, but is bob's
    //   d has no embedding — scores 0
    let hits = k.find_similar(vector_only(&alice, [0.9, 0.1], 2)).unwrap();
    assert_eq!(hits.len(), 2, "exact k");
    assert_eq!(hits[0].ko.koid, a, "a is the best alice-visible match");
    assert_eq!(hits[1].ko.koid, c, "c is second");
    assert!(
        hits.iter().all(|h| h.ko.koid != x),
        "the ACL-denied near-match never leaks"
    );
    // Scores are the committed truth, ~ hand-computed values.
    assert!(
        (hits[0].score - 0.9939).abs() < 1e-3,
        "score {}",
        hits[0].score
    );
    assert!(
        (hits[1].score - 0.7809).abs() < 1e-3,
        "score {}",
        hits[1].score
    );
    assert!(
        hits.iter().all(|h| h.index_lag_ms == 0),
        "exact path: zero lag"
    );
}

// --- vs_scan_002 — the materialization-count probe (the pre-impl failing pin) ---

#[test]
fn vs_scan_002_top_k_query_materializes_at_most_k_plus_margin() {
    let k = mk();
    let alice = ctx("alice");
    for i in 0..1000 {
        let angle = (i % 360) as f32 * std::f32::consts::PI / 180.0;
        note(
            &k,
            &alice,
            &format!("n{i}"),
            Some([angle.cos(), angle.sin()]),
        );
    }
    let before = k.similarity_materializations();
    let hits = k.find_similar(vector_only(&alice, [1.0, 0.0], 10)).unwrap();
    assert_eq!(hits.len(), 10);
    let read = k.similarity_materializations() - before;
    assert!(
        read <= 10 + 10,
        "the top-k query materialized {read} KOs (old path: 1 000) — more than k + margin"
    );
}

// --- vs_scan_003 — index_lag_ms semantics unchanged on the new path -------------

#[test]
fn vs_scan_003_index_lag_ms_keeps_the_exact_eventual_contract() {
    let k = mk();
    let (a, b, _c, x) = seeded(&k);
    let alice = ctx("alice");

    // Eventual: a maintainer whose vectors index holds the same committed
    // embeddings, lagging by 1 event — hits must carry the lag (P4-M7).
    let vectors: Arc<dyn aikoql_kernel::index::VectorIndex> =
        Arc::new(BruteForceVectorIndex::new());
    vectors.upsert(a, "bge-m3", &[1.0, 0.0]);
    vectors.upsert(b, "bge-m3", &[0.0, 1.0]);
    vectors.upsert(x, "bge-m3", &[0.99, 0.01]);
    let fake = Arc::new(FakeMaintainer {
        vectors,
        text: Arc::new(TokenTextIndex::new()),
    });
    k.attach_indexes(fake.clone());
    // P5-M18: the coordinator holds the maintainer WEAKLY — the owner keeps
    // the Arc alive (the SDK/MCP hosts do exactly this).
    let _owner = fake;
    let hits = k.find_similar(vector_only(&alice, [0.9, 0.1], 1)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].ko.koid, a, "the index-assisted path ranks a first");
    assert_eq!(
        hits[0].index_lag_ms, 1,
        "the eventual path surfaces its lag"
    );
}

/// The P4-M7 fake from the index subsystem tests: indexes lag the kernel.
struct FakeMaintainer {
    vectors: Arc<dyn aikoql_kernel::index::VectorIndex>,
    text: Arc<dyn aikoql_kernel::index::TextIndex>,
}

impl IndexMaintainerApi for FakeMaintainer {
    fn lag(&self, _kernel: &Kernel) -> KResult<u64> {
        Ok(1)
    }
    fn vectors(&self) -> &Arc<dyn aikoql_kernel::index::VectorIndex> {
        &self.vectors
    }
    fn text(&self) -> &Arc<dyn aikoql_kernel::index::TextIndex> {
        &self.text
    }
}
