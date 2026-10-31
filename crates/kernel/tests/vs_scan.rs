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

// --- vs_scan_004/005 — AG-20: the ANN leg asks for a bounded candidate pool ----

/// Records every `k` the coordinator asks the index for.
struct SpyIndex {
    inner: BruteForceVectorIndex,
    asked: std::sync::Mutex<Vec<usize>>,
}

impl aikoql_kernel::index::VectorIndex for SpyIndex {
    fn upsert(&self, koid: KOID, model: &str, vec: &[f32]) {
        self.inner.upsert(koid, model, vec)
    }
    fn remove(&self, koid: &KOID) {
        self.inner.remove(koid)
    }
    fn search(&self, qv: &[f32], k: usize, model: Option<&str>) -> Vec<(KOID, f32)> {
        self.asked.lock().unwrap().push(k);
        self.inner.search(qv, k, model)
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
}

/// `n` notes on the unit circle at `start + i * step` degrees, indexed too.
fn arc(
    k: &Kernel,
    spy: &SpyIndex,
    owner: &KnowledgeContext,
    n: usize,
    start: f32,
    step: f32,
) -> Vec<KOID> {
    (0..n)
        .map(|i| {
            let a = (start + i as f32 * step).to_radians();
            let e = [a.cos(), a.sin()];
            let id = note(k, owner, &format!("n{i}"), Some(e));
            spy.upsert(id, "bge-m3", &e);
            id
        })
        .collect()
}

fn spied(k: &Kernel) -> (Arc<SpyIndex>, Arc<FakeMaintainer>) {
    let spy = Arc::new(SpyIndex {
        inner: BruteForceVectorIndex::new(),
        asked: Default::default(),
    });
    let fake = Arc::new(FakeMaintainer {
        vectors: spy.clone(),
        text: Arc::new(TokenTextIndex::new()),
    });
    k.attach_indexes(fake.clone());
    (spy, fake)
}

/// RED pre-fix: the coordinator asked the index for usize::MAX — at 1M
/// objects HNSW walked every node and the leg read 1M scoring records.
#[test]
fn vs_scan_004_a_top_k_query_asks_the_index_for_a_bounded_pool() {
    let k = mk();
    let alice = ctx("alice");
    let (spy, _owner) = spied(&k);
    arc(&k, &spy, &alice, 2000, 0.0, 0.17);
    let hits = k.find_similar(vector_only(&alice, [1.0, 0.0], 10)).unwrap();
    assert_eq!(hits.len(), 10);
    let asked = spy.asked.lock().unwrap().clone();
    assert_eq!(
        asked.len(),
        1,
        "an unfiltered query needs one index call: {asked:?}"
    );
    assert!(
        asked[0] <= 512,
        "asked the index for {} candidates (k = 10)",
        asked[0]
    );
}

/// Guard: when the ACL drops the nearest candidates, the pool widens until
/// k visible ones are found — the answer equals the exact ranking.
#[test]
fn vs_scan_005_a_filtered_query_widens_the_pool_until_k_survive() {
    let k = mk();
    let (alice, bob) = (ctx("alice"), ctx("bob"));
    let (spy, _owner) = spied(&k);
    arc(&k, &spy, &bob, 2000, 0.0, 0.01); // the 2 000 nearest, all bob's
    let mine = arc(&k, &spy, &alice, 20, 30.0, 1.0);
    let hits = k.find_similar(vector_only(&alice, [1.0, 0.0], 5)).unwrap();
    let got: Vec<KOID> = hits.iter().map(|h| h.ko.koid).collect();
    assert_eq!(
        got,
        mine[..5].to_vec(),
        "alice's own five nearest, in order"
    );
    assert!(spy.asked.lock().unwrap().len() > 1, "the pool widened");
}

/// RED pre-fix: `recall` asks VectorOnly but passes the question as text,
/// and any text sent the query down the full-scan loop — the text is never
/// scored under VectorOnly, so it must not cost the bounded pool.
#[test]
fn vs_scan_006_vector_only_with_text_still_asks_a_bounded_pool() {
    let k = mk();
    let alice = ctx("alice");
    let (spy, _owner) = spied(&k);
    arc(&k, &spy, &alice, 2000, 0.0, 0.17);
    let q = vector_only(&alice, [1.0, 0.0], 10).with_text("what is nearest");
    let hits = k.find_similar(q).unwrap();
    assert_eq!(hits.len(), 10);
    let asked = spy.asked.lock().unwrap().clone();
    assert!(
        asked.iter().all(|&n| n <= 512),
        "VectorOnly + text asked the index for {asked:?} candidates"
    );
}

/// RED pre-fix: RRF found each hit's rank by a linear search of both
/// ranked lists — N^2 positions per query (10^12 at 1M objects). Ranking
/// N candidates must examine O(N) positions.
#[test]
fn vs_scan_007_rrf_ranks_without_a_quadratic_search() {
    let k = mk();
    let alice = ctx("alice");
    let n = 2000u64;
    for i in 0..n {
        let a = (i as f32 * 0.17).to_radians();
        note(
            &k,
            &alice,
            &format!("note {i} body"),
            Some([a.cos(), a.sin()]),
        );
    }
    let before = k.fusion_rank_probes();
    let q = SimilarityQuery::new(alice.clone(), 10, Fusion::Rrf { k0: 60 })
        .with_vector(vec![1.0, 0.0])
        .with_text("note body");
    assert_eq!(k.find_similar(q).unwrap().len(), 10);
    let probes = k.fusion_rank_probes() - before;
    assert!(
        probes <= 4 * n,
        "RRF examined {probes} list positions for {n} candidates"
    );
}

// --- vs_scan_008/009 — AG-20: hybrid fusions score a bounded candidate union --

/// Records every `k` the coordinator asks the text index for.
struct SpyText {
    inner: TokenTextIndex,
    asked: std::sync::Mutex<Vec<usize>>,
}

impl aikoql_kernel::index::TextIndex for SpyText {
    fn upsert(&self, koid: KOID, tokens: &std::collections::BTreeSet<String>) -> KResult<()> {
        self.inner.upsert(koid, tokens)
    }
    fn remove(&self, koid: &KOID) -> KResult<()> {
        self.inner.remove(koid)
    }
    fn search(
        &self,
        tokens: &std::collections::BTreeSet<String>,
        k: usize,
    ) -> KResult<Vec<(KOID, f32)>> {
        self.asked.lock().unwrap().push(k);
        self.inner.search(tokens, k)
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
}

fn words(s: &str) -> std::collections::BTreeSet<String> {
    s.split_whitespace().map(str::to_lowercase).collect()
}

/// A note with body text and an embedding, in both indexes.
fn indexed(
    k: &Kernel,
    (v, t): (&SpyIndex, &SpyText),
    owner: &KnowledgeContext,
    body: &str,
    e: [f32; 2],
) -> KOID {
    let id = note(k, owner, body, Some(e));
    v.upsert(id, "bge-m3", &e);
    t.upsert(id, &words(body)).unwrap();
    id
}

fn spied_both(k: &Kernel) -> (Arc<SpyIndex>, Arc<SpyText>, Arc<FakeMaintainer>) {
    let v = Arc::new(SpyIndex {
        inner: BruteForceVectorIndex::new(),
        asked: Default::default(),
    });
    let t = Arc::new(SpyText {
        inner: TokenTextIndex::new(),
        asked: Default::default(),
    });
    let fake = Arc::new(FakeMaintainer {
        vectors: v.clone(),
        text: t.clone(),
    });
    k.attach_indexes(fake.clone());
    (v, t, fake)
}

fn at(deg: f32) -> [f32; 2] {
    let a = deg.to_radians();
    [a.cos(), a.sin()]
}

/// RED pre-fix: an RRF query scored every head and asked both indexes for
/// usize::MAX — hybrid search stayed a full scan after AG-20's vector fix.
#[test]
fn vs_scan_008_hybrid_asks_both_indexes_for_a_bounded_pool() {
    let k = mk();
    let alice = ctx("alice");
    let (v, t, _owner) = spied_both(&k);
    for i in 0..2000 {
        indexed(
            &k,
            (&v, &t),
            &alice,
            &format!("note {i}"),
            at(i as f32 * 0.17),
        );
    }
    let q = SimilarityQuery::new(alice.clone(), 10, Fusion::Rrf { k0: 60 })
        .with_vector(vec![1.0, 0.0])
        .with_text("note");
    assert_eq!(k.find_similar(q).unwrap().len(), 10);
    let (va, ta) = (
        v.asked.lock().unwrap().clone(),
        t.asked.lock().unwrap().clone(),
    );
    assert!(
        va.iter().chain(&ta).all(|&n| n <= 512),
        "asked vector {va:?}, text {ta:?}"
    );
}

/// Guard: both legs feed the union, and the pools widen past what the ACL
/// hides — a text-only match and alice's nearest vector both come back.
#[test]
fn vs_scan_009_hybrid_union_keeps_both_legs_and_widens() {
    let k = mk();
    let (alice, bob) = (ctx("alice"), ctx("bob"));
    let (v, t, _owner) = spied_both(&k);
    for i in 0..1500 {
        indexed(&k, (&v, &t), &bob, "alpha", at(i as f32 * 0.01)); // nearest, all bob's
    }
    let zebra = indexed(&k, (&v, &t), &alice, "zebra", at(90.0)); // text only
    let mine: Vec<KOID> = (0..20)
        .map(|i| indexed(&k, (&v, &t), &alice, "plain", at(30.0 + i as f32)))
        .collect();
    let q = SimilarityQuery::new(alice.clone(), 3, Fusion::Rrf { k0: 60 })
        .with_vector(vec![1.0, 0.0])
        .with_text("zebra");
    let got: Vec<KOID> = k
        .find_similar(q)
        .unwrap()
        .iter()
        .map(|h| h.ko.koid)
        .collect();
    assert_eq!(got.len(), 3, "{got:?}");
    let mut top2 = got[..2].to_vec();
    top2.sort();
    let mut want = vec![zebra, mine[0]];
    want.sort();
    assert_eq!(
        top2, want,
        "the text match and the nearest visible vector tie first"
    );
    assert_eq!(got[2], mine[1], "then the next nearest");
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
