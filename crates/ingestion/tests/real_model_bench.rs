//! PR-P (HLD §60): the real-model measurement — the §60 retrieval
//! instrument (`common`) run with a live embedding endpoint, emitting the
//! gate verdict against the pinned baseline.
//!
//! SKIPs (passes) when `AIKOQL_EMBEDDING_ENDPOINT` is unset — CI never
//! dials out; the mock provider stays. Run the experiment with:
//!
//! ```text
//! $env:AIKOQL_EMBEDDING_ENDPOINT = "https://api.example.com/v1"
//! cargo test --features remote_emb --test real_model_bench -- --nocapture
//! ```
//!
//! No asserts on the measured scores — a real model's numbers are what
//! they are; the test prints `[REAL-MODEL] ... GO/NO-GO` per cell and the
//! instrument's correctness is pinned by the mock baseline in
//! `retrieval_quality.rs` (the same engine, byte-identical corpora).
//! The verdict thresholds are the §60 spec (IMPLEMENTATION-PLAN.md):
//! no metric may fall 0.02 below the baseline, and Recall@5 must gain at
//! least 0.05 — the two paraphrase probes sit at 0.0 in the baseline, so
//! +0.05 means the model resolves at least one of them.
#![cfg(feature = "remote_emb")]

mod common;

use aikoql_ingestion::remote_emb::{RemoteEmbeddingConfig, RemoteEmbeddingProvider};
use aikoql_ingestion::EmbeddingProvider;
use common::{
    chunk_text, corpus, measure, queries, rank_visual, visual_queries, visual_recall_at_k, Ranker,
    Run,
};
use std::collections::HashMap;

/// §60 spec gate: GO iff every metric holds baseline − 0.02 (no material
/// regression) AND Recall@5 gains at least 0.05 (a measured semantic win,
/// not mock noise).
fn gate_verdict(m: &[f32; 5], base: &[f32; 5]) -> bool {
    m.iter().zip(base).all(|(v, b)| *v + 0.02 >= *b) && m[2] >= base[2] + 0.05
}

/// Harness-only cache: corpus chunks and query texts embed ONCE — a §60 run
/// must not bill the endpoint per query scan (each query ranks ~100+
/// chunks). Keyed by text: identical text → identical embedding, so the
/// cache is semantics-preserving.
struct CachedEmbeddings<'a> {
    inner: &'a RemoteEmbeddingProvider,
    cache: std::sync::Mutex<HashMap<String, Vec<f32>>>,
}

impl<'a> CachedEmbeddings<'a> {
    fn new(inner: &'a RemoteEmbeddingProvider) -> Self {
        Self {
            inner,
            cache: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl EmbeddingProvider for CachedEmbeddings<'_> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        if let Some(v) = self.cache.lock().unwrap().get(text) {
            return v.clone();
        }
        let v = self.inner.embed(text);
        self.cache
            .lock()
            .unwrap()
            .insert(text.to_string(), v.clone());
        v
    }
}

#[test]
fn gate_verdict_requires_gain_without_regression() {
    let base = [0.8, 0.85, 0.867, 0.867, 0.867];
    // GO: R@5 up 0.1, nothing else moved.
    let mut m = base;
    m[2] += 0.1;
    assert!(gate_verdict(&m, &base), "measured gain, no regression → GO");
    // NO-GO: no gain.
    assert!(!gate_verdict(&base, &base), "parity is not a measured gain");
    // NO-GO: gain but a material regression elsewhere.
    let mut reg = base;
    reg[2] += 0.1;
    reg[3] -= 0.05;
    assert!(
        !gate_verdict(&reg, &base),
        "MRR regression beyond 0.02 blocks GO"
    );
    // NO-GO: gain below the 0.05 bar.
    let mut small = base;
    small[2] += 0.03;
    assert!(
        !gate_verdict(&small, &base),
        "a sub-0.05 gain is mock-scale noise"
    );
}

#[test]
fn real_model_measurement() {
    let Some(config) = RemoteEmbeddingConfig::from_env() else {
        eprintln!("[REAL-MODEL] SKIP: AIKOQL_EMBEDDING_ENDPOINT unset — mock stays the provider");
        return;
    };
    let provider = RemoteEmbeddingProvider::new(config);
    let cache = CachedEmbeddings::new(&provider);
    let t0 = std::time::Instant::now();
    eprintln!(
        "[REAL-MODEL] provider={} model=remote endpoint configured; mock pinned baseline 0.867/0.867/0.867",
        provider.name()
    );

    // Two corpora: the rule boundary (baseline corpus) and the
    // embedding-boundary variant with the real provider (boundary quality
    // = the same rankers over a differently segmented corpus).
    let (rule_corpus, rule_visual) = corpus(&aikoql_ingestion::RuleBoundaryDetector, &cache);
    let emb_detector = aikoql_ingestion::EmbeddingBoundaryDetector::new(&cache);
    let (emb_corpus, _) = corpus(&emb_detector, &cache);

    let qrels: Vec<Vec<String>> = queries()
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
            corpus: rule_corpus.clone(),
            ranker: Ranker::Hybrid,
        },
        Run {
            boundary: "embedding-embedding",
            corpus: emb_corpus,
            ranker: Ranker::Embedding,
        },
    ];

    // The same-run lexical cell is the baseline: same corpus, deterministic
    // ranker — self-contained against any corpus drift.
    let base = measure(&runs[0], &qrels, &cache);
    eprintln!(
        "[REAL-MODEL] baseline {} recall@1={:.3} recall@3={:.3} recall@5={:.3} mrr={:.3} ndcg@5={:.3}",
        runs[0].boundary, base[0], base[1], base[2], base[3], base[4]
    );
    for run in &runs[1..] {
        let m = measure(run, &qrels, &cache);
        eprintln!(
            "[REAL-MODEL] cell {} recall@1={:.3} recall@3={:.3} recall@5={:.3} mrr={:.3} ndcg@5={:.3} verdict={}",
            run.boundary,
            m[0],
            m[1],
            m[2],
            m[3],
            m[4],
            if gate_verdict(&m, &base) { "GO" } else { "NO-GO" }
        );
    }

    // Visual retrieval (§24/§53): records embedded by the real provider at
    // index-build time, ranked by the real provider — one consistent space.
    for (label, records) in [("rule", rule_visual.as_slice())] {
        assert!(
            !records.is_empty(),
            "{label}: visual index is empty — cannot judge visual retrieval"
        );
        let (mut r1, mut r3, mut r5) = (0.0f32, 0.0f32, 0.0f32);
        let vqs = visual_queries();
        for q in &vqs {
            let qrel: Vec<String> = q
                .relevant
                .iter()
                .map(|(f, i)| chunk_text(&rule_corpus, f, *i).to_string())
                .collect();
            let ranked = rank_visual(records, q.text, &cache);
            r1 += visual_recall_at_k(&ranked, records, &qrel, 1);
            r3 += visual_recall_at_k(&ranked, records, &qrel, 3);
            r5 += visual_recall_at_k(&ranked, records, &qrel, 5);
        }
        let n = vqs.len() as f32;
        eprintln!(
            "[REAL-MODEL] visual {label} recall@1={:.3} recall@3={:.3} recall@5={:.3} (mock pinned 1.000)",
            r1 / n,
            r3 / n,
            r5 / n
        );
    }

    eprintln!(
        "[REAL-MODEL] ingestion cost: embedding_api_calls={} wall_time={:?} (cached; corpus+queries embed once)",
        provider.call_count(),
        t0.elapsed()
    );
}
