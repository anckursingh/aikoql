//! PR-G..PR-L (HLD §60/§53): the §60 retrieval-quality matrix, pinned to
//! the mock char-ngram provider. The engine (corpus, rankers, metrics)
//! lives in `common` — the PR-P real-model bench swaps a live endpoint
//! into the same instrument. This file holds the mock-specific pieces:
//! the deterministic mock transformer scorer (PR-J) and the baseline
//! asserts.
//!
//! Pinned numbers (mock provider): rule-lexical baseline 0.867/0.867/0.867
//! (two paraphrase probes at 0.0), floors 0.75, variant parity gate 0.02.
//! A real model's measured gain over this baseline is the §60 decision —
//! measured by `real_model_bench.rs`, not here.

mod common;

use aikoql_ingestion::EmbeddingProvider;
use common::{
    chunk_text, corpus, measure, rank_visual, visual_recall_at_k, Ranker, Run, QUERIES,
    VISUAL_QUERIES,
};

/// PR-J: deterministic mock transformer — a transformer scorer IS a
/// similarity model, so the mock maps mock-embedding cosine to a
/// probability on [0,1]: p = (cosine + 1) / 2. Same-topic text (mock band
/// 0.16–0.51) lands at 0.58–0.75 — around the 0.7 accept threshold, exactly
/// like a real boundary classifier mid-calibration.
struct MockTransformerScorer;

impl aikoql_ingestion::BoundaryScorer for MockTransformerScorer {
    fn score_boundary(
        &self,
        prev: &aikoql_ingestion::KnowledgeFragment,
        next: &aikoql_ingestion::KnowledgeFragment,
    ) -> Option<aikoql_ingestion::BoundaryScore> {
        let provider = aikoql_ingestion::MockEmbeddingProvider::new();
        let a = provider.embed(&aikoql_ingestion::fragment_text(prev));
        let b = provider.embed(&aikoql_ingestion::fragment_text(next));
        let sim = aikoql_ingestion::cosine_similarity(&a, &b);
        Some(aikoql_ingestion::BoundaryScore {
            probability: (sim + 1.0) / 2.0,
            model: "mock-transformer".into(),
        })
    }
}

#[test]
fn rule_baseline_retrieval_quality() {
    // Four corpora: the rule boundary detector (baseline) and the
    // embedding / transformer / hybrid variants (PR-H/PR-I/PR-J, HLD §16).
    // Each corpus also carries the visual index records for the visual
    // ranker (PR-K, HLD §24). PR-P: the engine threads the provider
    // explicitly — the mock here, a live endpoint in `real_model_bench.rs`.
    let mock = aikoql_ingestion::MockEmbeddingProvider::new();
    let (rule_corpus, rule_visual) = corpus(&aikoql_ingestion::RuleBoundaryDetector, &mock);
    let emb_detector = aikoql_ingestion::EmbeddingBoundaryDetector::new(&mock);
    let (emb_corpus, emb_visual) = corpus(&emb_detector, &mock);
    let tfm_detector = aikoql_ingestion::TransformerBoundaryDetector::new(&MockTransformerScorer);
    let (tfm_corpus, tfm_visual) = corpus(&tfm_detector, &mock);
    let hyb_detector = aikoql_ingestion::HybridBoundaryDetector::new(&mock);
    let (hyb_corpus, hyb_visual) = corpus(&hyb_detector, &mock);
    eprintln!(
        "[RETRIEVAL-STRUCTURE] rule_boundary_chunks={} embedding_boundary_chunks={} transformer_boundary_chunks={} hybrid_boundary_chunks={} \
         visual_records={}",
        rule_corpus.len(),
        emb_corpus.len(),
        tfm_corpus.len(),
        hyb_corpus.len(),
        rule_visual.len(),
    );

    // Qrel text resolved from the rule corpus; variant corpora are judged
    // by containment against the same text.
    let qrels: Vec<Vec<String>> = QUERIES
        .iter()
        .map(|q| {
            q.relevant
                .iter()
                .map(|(f, i)| chunk_text(&rule_corpus, f, *i).to_string())
                .collect()
        })
        .collect();

    let runs = [
        Run {
            boundary: "rule-lexical",
            corpus: rule_corpus.clone(),
            ranker: Ranker::Lexical,
        },
        Run {
            boundary: "rule-embedding",
            corpus: rule_corpus.clone(),
            ranker: Ranker::Embedding,
        },
        Run {
            boundary: "rule-hybrid",
            // Cloned: the visual-recall loop below resolves qrel text from
            // the rule corpus after the runs consume theirs.
            corpus: rule_corpus.clone(),
            ranker: Ranker::Hybrid,
        },
        Run {
            boundary: "embedding-lexical",
            corpus: emb_corpus.clone(),
            ranker: Ranker::Lexical,
        },
        Run {
            boundary: "embedding-embedding",
            corpus: emb_corpus.clone(),
            ranker: Ranker::Embedding,
        },
        Run {
            boundary: "embedding-hybrid",
            corpus: emb_corpus,
            ranker: Ranker::Hybrid,
        },
        Run {
            boundary: "transformer-lexical",
            corpus: tfm_corpus.clone(),
            ranker: Ranker::Lexical,
        },
        Run {
            boundary: "transformer-embedding",
            corpus: tfm_corpus.clone(),
            ranker: Ranker::Embedding,
        },
        Run {
            boundary: "transformer-hybrid",
            corpus: tfm_corpus,
            ranker: Ranker::Hybrid,
        },
        Run {
            boundary: "hybrid-lexical",
            corpus: hyb_corpus.clone(),
            ranker: Ranker::Lexical,
        },
        Run {
            boundary: "hybrid-embedding",
            corpus: hyb_corpus.clone(),
            ranker: Ranker::Embedding,
        },
        Run {
            boundary: "hybrid-hybrid",
            corpus: hyb_corpus,
            ranker: Ranker::Hybrid,
        },
    ];

    let base = measure(&runs[0], &qrels, &mock);
    eprintln!(
        "[RETRIEVAL-BASELINE] {} queries={} recall@1={:.3} recall@3={:.3} recall@5={:.3} \
         mrr={:.3} ndcg@5={:.3} hybrid=measured below visual=measured below",
        runs[0].boundary,
        QUERIES.len(),
        base[0],
        base[1],
        base[2],
        base[3],
        base[4]
    );

    // PR-G pin: the rule pipeline's floor on this corpus. Two queries are
    // deliberate paraphrase probes the lexical ranker cannot resolve
    // (measured ≈ 0.87); the floors sit below that with headroom so a
    // regression (chunk text loss, projection breakage) fails CI.
    assert!(
        base[2] >= 0.75,
        "rule baseline regressed: Recall@5 = {:.3} < 0.75",
        base[2]
    );
    assert!(
        base[3] >= 0.75,
        "rule baseline regressed: MRR = {:.3} < 0.75",
        base[3]
    );
    assert!(
        base[4] >= 0.75,
        "rule baseline regressed: NDCG@5 = {:.3} < 0.75",
        base[4]
    );

    // §60 comparison: parity gate. Measured with the mock provider the
    // embedding ranker trades a little MRR for recall/NDCG (0.867→0.862,
    // 0.867→0.933/0.875) — within noise, not a uniform win. Each variant
    // metric must stay within 0.02 of the baseline: a real regression
    // (chunk text loss, projection breakage) fails CI, while the measured
    // gain must come from a real model provider, not the mock.
    for run in &runs[1..] {
        let m = measure(run, &qrels, &mock);
        eprintln!(
            "[RETRIEVAL-VARIANT] {} queries={} recall@1={:.3} recall@3={:.3} recall@5={:.3} \
             mrr={:.3} ndcg@5={:.3}",
            run.boundary,
            QUERIES.len(),
            m[0],
            m[1],
            m[2],
            m[3],
            m[4]
        );
        for (label, (v, b)) in [
            ("Recall@5", (m[2], base[2])),
            ("MRR", (m[3], base[3])),
            ("NDCG@5", (m[4], base[4])),
        ] {
            assert!(
                v + 0.02 >= b,
                "{}: variant materially regressed on {} ({:.3} < {:.3})",
                run.boundary,
                label,
                v,
                b
            );
        }
    }

    // PR-K (HLD §24/§53): visual retrieval recall — rank each corpus's
    // visual index records by query-vs-caption embedding cosine and judge
    // by caption containment against the same qrel text.
    for (label, records) in [
        ("rule", rule_visual.as_slice()),
        ("embedding", emb_visual.as_slice()),
        ("transformer", tfm_visual.as_slice()),
        ("hybrid", hyb_visual.as_slice()),
    ] {
        assert!(
            !records.is_empty(),
            "{label}: visual index is empty — cannot judge visual retrieval"
        );
        let (mut r1, mut r3, mut r5) = (0.0f32, 0.0f32, 0.0f32);
        for q in VISUAL_QUERIES {
            let qrel: Vec<String> = q
                .relevant
                .iter()
                .map(|(f, i)| chunk_text(&rule_corpus, f, *i).to_string())
                .collect();
            let ranked = rank_visual(records, q.text, &mock);
            let (a, b, c) = (
                visual_recall_at_k(&ranked, records, &qrel, 1),
                visual_recall_at_k(&ranked, records, &qrel, 3),
                visual_recall_at_k(&ranked, records, &qrel, 5),
            );
            r1 += a;
            r3 += b;
            r5 += c;
            eprintln!(
                "[RETRIEVAL-VISUAL-Q {label}] query={:?} records={} ranked={ranked:?} R@1={a} R@3={b} R@5={c}",
                q.text,
                records.len()
            );
        }
        let n = VISUAL_QUERIES.len() as f32;
        eprintln!(
            "[RETRIEVAL-VISUAL] {label} queries={} recall@1={:.3} recall@3={:.3} recall@5={:.3}",
            VISUAL_QUERIES.len(),
            r1 / n,
            r3 / n,
            r5 / n
        );
        // §24 floor: the visual index must retrieve its visual objects.
        assert!(
            r5 / n >= 0.5,
            "{label}: visual retrieval materially regressed (Recall@5 = {:.3} < 0.5)",
            r5 / n
        );
    }
}
