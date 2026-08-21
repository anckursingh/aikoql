//! PR-G (HLD §60/§53): rule-baseline retrieval-quality benchmarks.
//!
//! §60 says the transformer decision must rest on *measured* retrieval
//! quality, not intuition — so the rule pipeline's numbers are pinned here
//! as the baseline future analyzer variants (embedding / transformer /
//! hybrid boundaries) will be compared against. §53 names the matrix:
//! Recall@K, MRR, NDCG.
//!
//! Method: every golden fixture (HLD §52) is compiled through the real mock
//! pipeline (`compile_document_mock`) and its retrieval projection
//! (`HeadingProjector` → `EmbeddedChunk`s) forms ONE corpus — retrieval is
//! corpus-wide, so distractor chunks from other fixtures compete. Hand-
//! authored queries carry qrels ((fixture, chunk-index) pairs); a
//! deterministic lexical ranker — the rule-baseline instrument — ranks the
//! corpus per query, and the matrix is computed macro-averaged over queries.
//!
//! Two of the fifteen queries are deliberate paraphrase probes ("best-
//! performing three-month period", "financial record book"): no lexical
//! token overlaps the relevant chunk (at most weak distractor hits), so the
//! probes score zero — the baseline is honest about its ceiling, and a
//! semantic (embedding/VLM) variant has a measured gap to close. Measured
//! baseline: 13/15 queries at 1.0, 2 at 0.0 → Recall@K = MRR = NDCG@5 = 0.867.
//!
//! Not covered here (both N/A for a rule-only pipeline, printed as such):
//! hybrid retrieval recall and visual retrieval recall (§53) — they need an
//! embedding/VLM boundary, which is exactly what later PRs add. `scanned.pdf`
//! is also excluded: the mock compile runs without OCR, so it projects zero
//! chunks and cannot be judged.
//!
//! ponytail: no stopword/IDF/position weights in the ranker — replace with
//! BM25 or an embedding ranker in the PRs that add those variants.

use std::collections::HashSet;
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

/// One retrieval query over the global corpus, with hand-annotated relevant
/// chunks as (fixture, 0-based `chunk.position.chunk_index`) pairs.
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

/// A corpus chunk: (fixture, chunk-index) + text.
type CorpusChunk<'a> = (&'a str, usize, String);

/// §60 rule-baseline instrument: deterministic lexical retrieval over the
/// global corpus — score = fraction of distinct query tokens present in the
/// chunk text; zero-overlap chunks are NOT retrieved (a lexical retriever
/// returns nothing when no term matches). Ties break by (score desc,
/// fixture asc, chunk-index asc).
/// ponytail: bare token overlap, no stopwords/IDF/position weights — replace
/// with BM25 or an embedding ranker in the PRs that add those variants.
fn rank<'a>(corpus: &[CorpusChunk<'a>], query: &str) -> Vec<(&'a str, usize)> {
    let q_tokens: HashSet<String> = tokens(query);
    let mut scored: Vec<(f32, &'a str, usize)> = corpus
        .iter()
        .map(|(fixture, index, text)| {
            let c_tokens = tokens(text);
            let hits = q_tokens.iter().filter(|t| c_tokens.contains(*t)).count();
            (hits as f32 / q_tokens.len().max(1) as f32, *fixture, *index)
        })
        .filter(|(score, _, _)| *score > 0.0)
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap()
            .then_with(|| a.1.cmp(b.1))
            .then(a.2.cmp(&b.2))
    });
    scored.into_iter().map(|(_, f, i)| (f, i)).collect()
}

fn tokens(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Fraction of relevant chunks appearing in the top-K ranks (binary relevance).
fn recall_at_k(ranked: &[(&str, usize)], relevant: &[(&str, usize)], k: usize) -> f32 {
    if relevant.is_empty() {
        return 1.0;
    }
    let top: HashSet<(&str, usize)> = ranked.iter().take(k).copied().collect();
    relevant.iter().filter(|r| top.contains(r)).count() as f32 / relevant.len() as f32
}

/// Reciprocal rank of the first relevant chunk (0 if none found).
fn mrr(ranked: &[(&str, usize)], relevant: &[(&str, usize)]) -> f32 {
    ranked
        .iter()
        .position(|r| relevant.contains(r))
        .map_or(0.0, |p| 1.0 / (p as f32 + 1.0))
}

/// NDCG@K with binary gains: DCG = Σ gainᵢ/log₂(rankᵢ+1), normalized by the
/// ideal DCG (all relevant chunks ranked first).
fn ndcg_at_k(ranked: &[(&str, usize)], relevant: &[(&str, usize)], k: usize) -> f32 {
    let dcg: f32 = ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, r)| {
            if relevant.contains(r) {
                1.0 / ((i as f32 + 2.0).log2())
            } else {
                0.0
            }
        })
        .sum();
    let ideal: f32 = (0..relevant.len().min(k))
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

#[test]
fn rule_baseline_retrieval_quality() {
    // Compile each fixture once into the global corpus, in FIXTURES order.
    let mut corpus: Vec<CorpusChunk<'_>> = Vec::new();
    for name in FIXTURES {
        let path = Path::new(FIXTURE_DIR).join(name);
        let dm =
            aikoql_ingestion::extract_document(&path.to_string_lossy(), "application/pdf", None)
                .unwrap_or_else(|e| panic!("{name}: extraction failed: {e}"));
        for chunk in aikoql_ingestion::compile_document_mock(&dm, &[]).embedded_chunks {
            corpus.push((name, chunk.chunk.position.chunk_index, chunk.chunk.text));
        }
    }
    assert!(
        !corpus.is_empty(),
        "corpus projects zero chunks — cannot judge retrieval"
    );

    let mut totals = [0.0f32; 3]; // recall@1, recall@3, recall@5
    let mut mrr_sum = 0.0f32;
    let mut ndcg5_sum = 0.0f32;
    let mut ndcg10_sum = 0.0f32;

    for q in QUERIES {
        let ranked = rank(&corpus, q.text);
        let (r1, r3, r5) = (
            recall_at_k(&ranked, q.relevant, 1),
            recall_at_k(&ranked, q.relevant, 3),
            recall_at_k(&ranked, q.relevant, 5),
        );
        let m = mrr(&ranked, q.relevant);
        let n5 = ndcg_at_k(&ranked, q.relevant, 5);
        let n10 = ndcg_at_k(&ranked, q.relevant, 10);
        totals[0] += r1;
        totals[1] += r3;
        totals[2] += r5;
        mrr_sum += m;
        ndcg5_sum += n5;
        ndcg10_sum += n10;
        eprintln!(
            "[RETRIEVAL-Q {:?}] ranked={ranked:?} relevant={:?} R@1={r1} R@3={r3} R@5={r5} MRR={m} NDCG@5={n5} NDCG@10={n10}",
            q.text, q.relevant
        );
    }

    let n = QUERIES.len() as f32;
    let (r1, r3, r5) = (totals[0] / n, totals[1] / n, totals[2] / n);
    let (mrr_, ndcg5, ndcg10) = (mrr_sum / n, ndcg5_sum / n, ndcg10_sum / n);
    eprintln!(
        "[RETRIEVAL-BASELINE] queries={} recall@1={r1:.3} recall@3={r3:.3} recall@5={r5:.3} \
         mrr={mrr_:.3} ndcg@5={ndcg5:.3} ndcg@10={ndcg10:.3} hybrid=N/A visual=N/A",
        QUERIES.len()
    );

    // §60 pin: the rule pipeline's floor on this corpus. Two queries are
    // deliberate paraphrase probes the lexical ranker cannot resolve
    // (expected ≈ 0.87); the floors sit below that with headroom so an
    // embedding/transformer variant passes trivially and a regression
    // (chunk text loss, projection breakage) fails CI.
    assert!(
        r5 >= 0.75,
        "rule baseline regressed: Recall@5 = {r5:.3} < 0.75"
    );
    assert!(
        mrr_ >= 0.75,
        "rule baseline regressed: MRR = {mrr_:.3} < 0.75"
    );
    assert!(
        ndcg5 >= 0.75,
        "rule baseline regressed: NDCG@5 = {ndcg5:.3} < 0.75"
    );
}
