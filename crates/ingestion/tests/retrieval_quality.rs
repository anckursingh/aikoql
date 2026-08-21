//! PR-G + PR-H + PR-I + PR-J + PR-L (HLD §60/§53): retrieval-quality
//! benchmarks with the four-boundary comparison matrix.
//!
//! §60 says the transformer decision must rest on *measured* retrieval
//! quality, not intuition. PR-G pinned the rule pipeline's numbers; PR-H
//! added the first variant and the comparison itself; PR-I and PR-J widen
//! it to the full Rule vs Embedding vs Transformer vs Hybrid matrix;
//! PR-L adds the hybrid ranker (lexical+embedding reciprocal rank fusion)
//! as a third ranker slot, filling the §53 "hybrid retrieval recall" cell:
//!
//! ```text
//!                      lexical ranker   embedding ranker   hybrid ranker
//! rule boundary        [baseline]       [variant]          [variant]
//! embedding boundary   [variant]        [variant]          [variant]
//! transformer boundary [variant]        [variant]          [variant]
//! hybrid boundary      [variant]        [variant]          [variant]
//! ```
//!
//! The transformer cell runs the `TransformerBoundaryDetector` (PR-J) with
//! a deterministic mock scorer — the same mock char-ngram similarity
//! calibrated to a probability: a mock transformer IS a similarity model,
//! so its cell is honest about the mechanism while a real model's gain
//! stays the §60 decision.
//!
//! Every golden fixture (HLD §52) compiles through the real mock pipeline
//! (`compile_document_mock_with_detector`) and its retrieval projection
//! (`HeadingProjector` → `EmbeddedChunk`s) forms ONE corpus per boundary
//! variant — retrieval is corpus-wide, so distractor chunks from other
//! fixtures compete. Hand-authored queries carry qrels ((fixture,
//! chunk-index) pairs) resolved to chunk text from the rule corpus; the
//! variant corpus is judged by the same qrel text via containment (split
//! sub-chunks and merged super-chunks both count as relevant). Three
//! rankers: bare token overlap (the rule-baseline instrument), embedding
//! cosine over the `EmbeddingProvider` seam, and their reciprocal-rank
//! fusion (the §53 hybrid retriever). Metrics are macro-averaged over
//! queries: Recall@1/3/5, MRR, NDCG@5/10; every cell prints its per-query
//! lines and one summary line.
//!
//! Two of the fifteen queries are deliberate paraphrase probes ("best-
//! performing three-month period", "financial record book"): no lexical
//! token overlaps the relevant chunk, so the lexical ranker scores zero —
//! the baseline is honest about its ceiling, and the embedding ranker's
//! result on the probes is the measured headroom a stronger model would
//! close. Measured baseline: 13/15 queries at 1.0, 2 at 0.0 →
//! Recall@K = MRR = NDCG@5 = 0.867.
//!
//! Measured PR-H matrix (Recall@5 / MRR / NDCG@5; mock char-ngram provider):
//! rule-lexical 0.867/0.867/0.867 (baseline); rule-embedding
//! 0.933/0.862/0.875 — the mock trades a little MRR for recall: parity,
//! within noise (its cosine band has no topic gap, so it ranks everything
//! and shifts a few rank-1s to rank-2/3). §60's honest conclusion: variants
//! must not *materially* regress (gate: every metric within 0.02 of the
//! baseline) and the measured gain must come from a real model provider,
//! not the mock.
//!
//! PR-K (HLD §24) adds the visual ranker: each corpus also builds a visual
//! index (`VisualIndexRecord`s derived from visual fragments), and a small
//! set of visual queries is ranked by query-vs-record embedding cosine,
//! judged against the same qrel text via caption containment — visual
//! retrieval recall (§53), printed as `[RETRIEVAL-VISUAL]`.
//! `scanned.pdf` is also excluded: the mock compile runs without OCR, so it
//! projects zero chunks and cannot be judged.
//!
//! ponytail: no stopword/IDF/position weights in the lexical ranker — replace
//! with BM25 in the PR that adds a real model provider.

use aikoql_ingestion::{
    BoundaryScore, BoundaryScorer, EmbeddingProvider, KnowledgeFragment, VisualIndexRecord,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const FIXTURE_DIR: &str = "tests/fixtures/multimodal";

/// The HLD §52 fixtures, in deterministic corpus order.
const FIXTURES: &[&str] = &[
    "plain-text.pdf",
    "tables.pdf",
    "complex-table.pdf",
    "charts.pdf",
    "architecture-diagram.pdf",
    "mixed-report.pdf",
    "annual-report.pdf",
    "formulas.pdf",
    "images.pdf",
];

/// One retrieval query over the corpus, with hand-annotated relevant
/// chunks as (fixture, 0-based `chunk.position.chunk_index`) pairs in the
/// RULE corpus; qrel text is resolved from there and matched by containment
/// on every corpus.
struct Query {
    text: &'static str,
    relevant: &'static [(&'static str, usize)],
}

const QUERIES: &[Query] = &[
    Query {
        text: "What was the revenue for Q3 2025?",
        relevant: &[("plain-text.pdf", 1)],
    },
    Query {
        text: "Who publishes quarterly reports?",
        relevant: &[("plain-text.pdf", 0)],
    },
    Query {
        text: "How old is Alice Smith?",
        relevant: &[("tables.pdf", 0)],
    },
    Query {
        text: "What is the revenue in North America?",
        relevant: &[("tables.pdf", 1)],
    },
    Query {
        text: "How many units were sold in Q1 2025?",
        relevant: &[("complex-table.pdf", 0)],
    },
    Query {
        text: "What is the warranty on Home Automation?",
        relevant: &[("complex-table.pdf", 1)],
    },
    Query {
        text: "Which quarter had the highest total revenue?",
        relevant: &[("charts.pdf", 0)],
    },
    // Paraphrase probe: no lexical token overlap with any chunk — the
    // measured gap a semantic retriever must close.
    Query {
        text: "What was their best-performing three-month period?",
        relevant: &[("charts.pdf", 0)],
    },
    Query {
        text: "How does the client reach the database?",
        relevant: &[("architecture-diagram.pdf", 0)],
    },
    Query {
        text: "Who validates payments?",
        relevant: &[("mixed-report.pdf", 0)],
    },
    // Paraphrase probe: "in charge of" for Owner, "financial record book"
    // for Ledger — no token overlap.
    Query {
        text: "Who is in charge of the financial record book?",
        relevant: &[("mixed-report.pdf", 0)],
    },
    Query {
        text: "What was Globex Industries revenue?",
        relevant: &[("annual-report.pdf", 1)],
    },
    Query {
        text: "What do Gamma Partners expect?",
        relevant: &[("annual-report.pdf", 2)],
    },
    Query {
        text: "What is the energy mass equation?",
        relevant: &[("formulas.pdf", 0)],
    },
    Query {
        text: "What logo is shown in figure 3?",
        relevant: &[("images.pdf", 0)],
    },
];

/// Visual queries (§53 visual retrieval recall, PR-K): qrels resolved from
/// the rule corpus (containment) and judged against visual index records'
/// captions. Both probe the same visual object — an exact-phrase query and
/// a paraphrase — because images.pdf carries the only asset-backed visuals
/// in the fixture set: PDF chart drawings are not extracted as assets, so
/// no chart records exist to judge (ponytail ceiling, visual_index.rs).
const VISUAL_QUERIES: &[Query] = &[
    Query {
        text: "What logo is shown in figure 3?",
        relevant: &[("images.pdf", 0)],
    },
    Query {
        text: "What does the company logo depict?",
        relevant: &[("images.pdf", 0)],
    },
];

/// A corpus chunk: (fixture, chunk-index) + text.
type CorpusChunk<'a> = (&'a str, usize, String);

/// Compile each fixture once into a corpus, in FIXTURES order, with the
/// given boundary detector (PR-H: the §60 variant seam). Returns the text
/// chunks plus the visual index records (PR-K, HLD §24) for the visual
/// ranker.
fn corpus(
    detector: &dyn aikoql_ingestion::KnowledgeBoundaryDetector,
) -> (Vec<CorpusChunk<'static>>, Vec<VisualIndexRecord>) {
    let mut corpus: Vec<CorpusChunk> = Vec::new();
    let mut visual: Vec<VisualIndexRecord> = Vec::new();
    for name in FIXTURES {
        let path = Path::new(FIXTURE_DIR).join(name);
        let dm =
            aikoql_ingestion::extract_document(&path.to_string_lossy(), "application/pdf", None)
                .unwrap_or_else(|e| panic!("{name}: extraction failed: {e}"));
        let result =
            aikoql_ingestion::compile_document_mock_with_detector(&dm, &[], None, detector);
        for chunk in result.embedded_chunks {
            corpus.push((*name, chunk.chunk.position.chunk_index, chunk.chunk.text));
        }
        visual.extend(result.visual_index);
    }
    assert!(
        !corpus.is_empty(),
        "corpus projects zero chunks — cannot judge retrieval"
    );
    (corpus, visual)
}

fn chunk_text<'a>(corpus: &'a [CorpusChunk<'a>], fixture: &str, index: usize) -> &'a str {
    corpus
        .iter()
        .find(|(f, i, _)| *f == fixture && *i == index)
        .map(|(_, _, t)| t.as_str())
        .unwrap_or_else(|| panic!("{fixture} chunk {index} missing from corpus"))
}

/// A variant chunk is relevant when its text equals, contains, or is
/// contained by the qrel text: split sub-chunks and merged super-chunks of
/// the annotated unit both count.
fn is_relevant(chunk: &str, qrel: &str) -> bool {
    let a = qrel.trim();
    let b = chunk.trim();
    !a.is_empty() && !b.is_empty() && (a == b || a.contains(b) || b.contains(a))
}

fn tokens(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Visual ranker (PR-K, HLD §24): query embedding vs record embedding
/// cosine over the same mock provider; records with zero/negative
/// similarity are not retrieved. Ties break by (score desc, document_id
/// asc, page asc).
fn rank_visual(records: &[VisualIndexRecord], query: &str) -> Vec<usize> {
    let provider = aikoql_ingestion::MockEmbeddingProvider::new();
    let q = provider.embed(query);
    let mut scored: Vec<(f32, usize, String, u32)> = records
        .iter()
        .enumerate()
        .map(|(i, r)| {
            (
                aikoql_ingestion::cosine_similarity(&q, &r.embedding),
                i,
                r.document_id.clone(),
                r.page,
            )
        })
        .filter(|(s, _, _, _)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap()
            .then_with(|| a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });
    scored.into_iter().map(|(_, i, _, _)| i).collect()
}

/// Fraction of visual queries whose relevant record appears in the top-K
/// (binary relevance; caption containment against the qrel text).
fn visual_recall_at_k(
    ranked: &[usize],
    records: &[VisualIndexRecord],
    qrels: &[String],
    k: usize,
) -> f32 {
    if qrels.is_empty() {
        return 1.0;
    }
    let found = ranked.iter().take(k).any(|&i| {
        let caption = records[i].semantic_caption.as_deref().unwrap_or("");
        qrels.iter().any(|q| is_relevant(caption, q))
    });
    if found {
        1.0
    } else {
        0.0
    }
}

/// §60 rankers — both deterministic over the corpus:
/// - Lexical: fraction of distinct query tokens present in the chunk text;
///   zero-overlap chunks are NOT retrieved (a lexical retriever returns
///   nothing when no term matches). ponytail: no stopwords/IDF/position
///   weights — replace with BM25 in the PR that adds a real model provider.
/// - Embedding: cosine similarity over the `EmbeddingProvider` seam (mock
///   char-ngram here; a real model provider swaps in without changing the
///   ranker). Exactly-orthogonal chunks are not retrieved.
///
/// Ties break by (score desc, fixture asc, chunk-index asc).
fn rank<'a>(corpus: &[CorpusChunk<'a>], query: &str, embedding: bool) -> Vec<(&'a str, usize)> {
    let mut scored: Vec<(f32, &'a str, usize)> = if embedding {
        let provider = aikoql_ingestion::MockEmbeddingProvider::new();
        let q = provider.embed(query);
        corpus
            .iter()
            .map(|(fixture, index, text)| {
                (
                    aikoql_ingestion::cosine_similarity(&q, &provider.embed(text)),
                    *fixture,
                    *index,
                )
            })
            .filter(|(s, _, _)| *s > 0.0)
            .collect()
    } else {
        let q_tokens: HashSet<String> = tokens(query);
        corpus
            .iter()
            .map(|(fixture, index, text)| {
                let c_tokens = tokens(text);
                let hits = q_tokens.iter().filter(|t| c_tokens.contains(*t)).count();
                (hits as f32 / q_tokens.len().max(1) as f32, *fixture, *index)
            })
            .filter(|(score, _, _)| *score > 0.0)
            .collect()
    };
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap()
            .then_with(|| a.1.cmp(b.1))
            .then(a.2.cmp(&b.2))
    });
    scored.into_iter().map(|(_, f, i)| (f, i)).collect()
}

/// §53 hybrid ranker (PR-L): reciprocal rank fusion of the lexical and
/// embedding ranked lists — score(d) = Σ 1/(60 + rankᵣ(d)) over the rankers
/// that retrieve d (standard RRF k=60; no score normalization needed because
/// ranks are comparable). Chunks retrieved by both rankers rank strongly,
/// one-ranker chunks stay reachable — a hybrid retriever's characteristic
/// recall over lexical misses.
fn rank_hybrid<'a>(corpus: &[CorpusChunk<'a>], query: &str) -> Vec<(&'a str, usize)> {
    let lex = rank(corpus, query, false);
    let emb = rank(corpus, query, true);
    let pos: HashMap<(&str, usize), usize> = corpus
        .iter()
        .enumerate()
        .map(|(p, (f, i, _))| ((*f, *i), p))
        .collect();
    let mut scores = vec![0.0f32; corpus.len()];
    for (r, pair) in lex.iter().enumerate() {
        if let Some(&p) = pos.get(pair) {
            scores[p] += 1.0 / (60.0 + r as f32 + 1.0);
        }
    }
    for (r, pair) in emb.iter().enumerate() {
        if let Some(&p) = pos.get(pair) {
            scores[p] += 1.0 / (60.0 + r as f32 + 1.0);
        }
    }
    let mut ranked: Vec<(f32, &'a str, usize)> = scores
        .into_iter()
        .enumerate()
        .filter(|(_, s)| *s > 0.0)
        .map(|(p, s)| (s, corpus[p].0, corpus[p].1))
        .collect();
    ranked.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap()
            .then_with(|| a.1.cmp(b.1))
            .then(a.2.cmp(&b.2))
    });
    ranked.into_iter().map(|(_, f, i)| (f, i)).collect()
}

/// Fraction of relevant chunks appearing in the top-K ranks (binary relevance).
fn recall_at_k(
    ranked: &[(&str, usize)],
    corpus: &[CorpusChunk],
    qrels: &[String],
    k: usize,
) -> f32 {
    if qrels.is_empty() {
        return 1.0;
    }
    let found = ranked.iter().take(k).any(|(f, i)| {
        qrels
            .iter()
            .any(|q| is_relevant(chunk_text(corpus, f, *i), q))
    });
    if found {
        1.0
    } else {
        0.0
    }
}

/// Reciprocal rank of the first relevant chunk (0 if none found).
fn mrr(ranked: &[(&str, usize)], corpus: &[CorpusChunk], qrels: &[String]) -> f32 {
    ranked
        .iter()
        .position(|(f, i)| {
            qrels
                .iter()
                .any(|q| is_relevant(chunk_text(corpus, f, *i), q))
        })
        .map_or(0.0, |p| 1.0 / (p as f32 + 1.0))
}

/// NDCG@K with binary gains: DCG = Σ gainᵢ/log₂(rankᵢ+1), normalized by the
/// ideal DCG (all relevant chunks ranked first).
fn ndcg_at_k(ranked: &[(&str, usize)], corpus: &[CorpusChunk], qrels: &[String], k: usize) -> f32 {
    let dcg: f32 = ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, (f, ci))| {
            if qrels
                .iter()
                .any(|q| is_relevant(chunk_text(corpus, f, *ci), q))
            {
                1.0 / ((i as f32 + 2.0).log2())
            } else {
                0.0
            }
        })
        .sum();
    let ideal: f32 = (0..qrels.len().min(k))
        .map(|i| 1.0 / ((i as f32 + 2.0).log2()))
        .sum();
    let value = if ideal == 0.0 { 1.0 } else { dcg / ideal };
    // An empty fold's 0.0 can surface as -0.0 (fast-math reduction identity);
    // a metric of negative zero is wrong output.
    if value == 0.0 {
        0.0
    } else {
        value
    }
}

/// PR-J: deterministic mock transformer — a transformer scorer IS a
/// similarity model, so the mock maps mock-embedding cosine to a
/// probability on [0,1]: p = (cosine + 1) / 2. Same-topic text (mock band
/// 0.16–0.51) lands at 0.58–0.75 — around the 0.7 accept threshold, exactly
/// like a real boundary classifier mid-calibration.
struct MockTransformerScorer;

impl BoundaryScorer for MockTransformerScorer {
    fn score_boundary(
        &self,
        prev: &KnowledgeFragment,
        next: &KnowledgeFragment,
    ) -> Option<BoundaryScore> {
        let provider = aikoql_ingestion::MockEmbeddingProvider::new();
        let a = provider.embed(&aikoql_ingestion::fragment_text(prev));
        let b = provider.embed(&aikoql_ingestion::fragment_text(next));
        let sim = aikoql_ingestion::cosine_similarity(&a, &b);
        Some(BoundaryScore {
            probability: (sim + 1.0) / 2.0,
            model: "mock-transformer".into(),
        })
    }
}

/// §60 ranker slot (PR-L adds the hybrid fusion as the third slot).
enum Ranker {
    Lexical,
    Embedding,
    Hybrid,
}

/// One §60 matrix cell: (boundary label, corpus, ranker).
struct Run {
    boundary: &'static str,
    corpus: Vec<CorpusChunk<'static>>,
    ranker: Ranker,
}

/// Run every query through the cell; print one line per query and return
/// the macro-averaged matrix (Recall@1/3/5, MRR, NDCG@5).
fn measure(run: &Run, qrels: &[Vec<String>]) -> [f32; 5] {
    let mut totals = [0.0f32; 3];
    let mut mrr_sum = 0.0f32;
    let mut ndcg5_sum = 0.0f32;

    for (qi, q) in QUERIES.iter().enumerate() {
        let ranked = match run.ranker {
            Ranker::Lexical => rank(&run.corpus, q.text, false),
            Ranker::Embedding => rank(&run.corpus, q.text, true),
            Ranker::Hybrid => rank_hybrid(&run.corpus, q.text),
        };
        let (r1, r3, r5) = (
            recall_at_k(&ranked, &run.corpus, &qrels[qi], 1),
            recall_at_k(&ranked, &run.corpus, &qrels[qi], 3),
            recall_at_k(&ranked, &run.corpus, &qrels[qi], 5),
        );
        let m = mrr(&ranked, &run.corpus, &qrels[qi]);
        let n5 = ndcg_at_k(&ranked, &run.corpus, &qrels[qi], 5);
        let n10 = ndcg_at_k(&ranked, &run.corpus, &qrels[qi], 10);
        totals[0] += r1;
        totals[1] += r3;
        totals[2] += r5;
        mrr_sum += m;
        ndcg5_sum += n5;
        eprintln!(
            "[RETRIEVAL-Q {} {:?}] ranked={ranked:?} relevant={:?} R@1={r1} R@3={r3} R@5={r5} MRR={m} NDCG@5={n5} NDCG@10={n10}",
            run.boundary, q.text, q.relevant
        );
    }

    let n = QUERIES.len() as f32;
    [
        totals[0] / n,
        totals[1] / n,
        totals[2] / n,
        mrr_sum / n,
        ndcg5_sum / n,
    ]
}

#[test]
fn rule_baseline_retrieval_quality() {
    // Four corpora: the rule boundary detector (baseline) and the
    // embedding / transformer / hybrid variants (PR-H/PR-I/PR-J, HLD §16).
    // Each corpus also carries the visual index records for the visual
    // ranker (PR-K, HLD §24).
    let (rule_corpus, rule_visual) = corpus(&aikoql_ingestion::RuleBoundaryDetector);
    let emb_provider = aikoql_ingestion::MockEmbeddingProvider::new();
    let emb_detector = aikoql_ingestion::EmbeddingBoundaryDetector::new(&emb_provider);
    let (emb_corpus, emb_visual) = corpus(&emb_detector);
    let tfm_detector = aikoql_ingestion::TransformerBoundaryDetector::new(&MockTransformerScorer);
    let (tfm_corpus, tfm_visual) = corpus(&tfm_detector);
    let hyb_provider = aikoql_ingestion::MockEmbeddingProvider::new();
    let hyb_detector = aikoql_ingestion::HybridBoundaryDetector::new(&hyb_provider);
    let (hyb_corpus, hyb_visual) = corpus(&hyb_detector);
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

    let base = measure(&runs[0], &qrels);
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
        let m = measure(run, &qrels);
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
            let ranked = rank_visual(records, q.text);
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
