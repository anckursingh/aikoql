//! Wave 3 (AIKOQL_Wave3_Market_Reality_TDD_Test_Plan_v2) — market-reality
//! experiments, deterministic (no LLM, CI-reproducible, the G12 convention).
//! Wave 3 is product-evidence work, NOT new substrate features: every
//! experiment runs the existing kernel/compiler machinery over the
//! market-extended corpus and measures the product claims.
//!
//! Instruments in this file:
//! - `w3_mkt_001_market_corpus_integrity` — W3-G02: the market corpus is
//!   versioned (git), fair (every answer unit verbatim-backed), and labeled
//!   with the §8 workload classes.
//! - `w3_win_001_workload_classification` — W3-WIN-001: both mechanical
//!   treatments (AikoQL compile+render vs lexical RAG pack, the
//!   `knowledge_bench.rs` machinery) over the Track-B + market union,
//!   rolled up per workload class into Strong Fit / Good Fit / Parity /
//!   Poor Fit / Unknown. W3-G04 (≥1 repeatable strong-fit class) is
//!   asserted.
//!
//! The kernel-state experiments (W3-TEMP-001 temporal confusion, W3-UNK-001
//! unknown handling, W3-CONF-001 contradiction value, W3-LONG-001
//! longitudinal, W3-DEBUG-001 debuggability) live in
//! `crates/kernel/tests/wave3_market_reality.rs` — they need kernel ops
//! (supersede, retention, explain, lineage) this IR-only bench doesn't have.
//!
//! Negative evidence is MANDATORY (plan §29): probes whose measured outcome
//! is parity or a loss are kept and reported, never dropped.

mod common;

use aikoql_ingestion::{
    compile_context, merge_knowledge_ir, render_context_markdown, KnowledgeIr,
    MockEmbeddingProvider,
};
use common::trackb::{
    assert_integrity, corpus, docs, market_docs, units_hit, Doc, Question, MARKET_QUESTIONS,
    QUESTIONS,
};

/// Token budget both treatments must respect (len/4 estimate — the G12
/// convention; same value `knowledge_bench.rs` uses).
const BUDGET: usize = 300;

/// The union question set, in a fixed order: the pinned Track-B questions
/// first, then the market extension.
fn all_questions() -> Vec<&'static Question> {
    QUESTIONS.iter().chain(MARKET_QUESTIONS.iter()).collect()
}

fn all_docs() -> Vec<Doc> {
    docs().into_iter().chain(market_docs()).collect()
}

/// W3-MKT-001 / W3-G02 — market corpus integrity. Fairness (RAG could in
/// principle retrieve every unit — verbatim backing) + the workload-class
/// labels the win-zone rollup depends on. Document-id units (the W7
/// provenance question) are backed by IR evidence ids instead — they exist
/// precisely because no chunk text carries them.
#[test]
fn w3_mkt_001_market_corpus_integrity() {
    let docs = all_docs();
    let chunks = corpus(&docs);
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    assert_integrity(&docs, &merged);

    let doc_ids: Vec<&str> = docs.iter().map(|d| d.id).collect();
    let all_chunks: String = chunks
        .iter()
        .map(|(_, _, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for (i, q) in all_questions().iter().enumerate() {
        for unit in q.units {
            if doc_ids.contains(&unit) {
                // A doc-id unit must be backed by evidence, not chunk text.
                assert!(
                    merged
                        .entities
                        .iter()
                        .map(|e| &e.evidence)
                        .chain(merged.facts.iter().map(|f| &f.evidence))
                        .chain(merged.relations.iter().map(|r| &r.evidence))
                        .any(|ev| ev.document_id.as_deref() == Some(unit)),
                    "provenance unit '{unit}' has no IR evidence backing (question {i})"
                );
            } else {
                assert!(
                    chunk_tokens_back(&all_chunks, unit),
                    "unit '{unit}' of question {i} has no verbatim backing chunk — \
                     expected evidence missing"
                );
            }
        }
        assert!(
            q.class.starts_with('W')
                && q.class[1..].parse::<u8>().is_ok()
                && q.class[1..].parse::<u8>().map_or(false, |n| (1..=12).contains(&n)),
            "question {i} lacks a W1-W12 workload class label"
        );
    }

    // Class coverage: every static-corpus class the plan's taxonomy (§8)
    // can carry must be present. W8 (personal memory) is the §32 memory
    // bench, W10 (agent planning) needs agent loops (out of substrate
    // scope), W12 (longitudinal) is the kernel W3-LONG-001 experiment.
    let covered: std::collections::BTreeSet<&str> =
        all_questions().iter().map(|q| q.class).collect();
    for needed in ["W1", "W2", "W3", "W4", "W5", "W6", "W7", "W9", "W11"] {
        assert!(
            covered.contains(needed),
            "workload class {needed} has no market question"
        );
    }
    assert!(
        all_questions().len() >= 10,
        "market question set too small: {}",
        all_questions().len()
    );
    eprintln!(
        "[W3-MKT-001] market corpus: {} docs / {} chunks / {} questions across classes {:?}",
        docs.len(),
        chunks.len(),
        all_questions().len(),
        covered,
    );
}

/// Token-containment backing check: every content token of `unit` appears
/// in `all_chunks` (the same verbatim-backing rule assert_integrity uses).
fn chunk_tokens_back(all_chunks: &str, unit: &str) -> bool {
    let pool = common::tokens(all_chunks);
    common::tokens(unit).iter().all(|t| pool.contains(t))
}

/// W3-WIN-001 — per-workload-class comparison and classification.
/// Treatments: AikoQL (merged-IR compile + render) vs the lexical RAG pack,
/// exactly `knowledge_bench.rs`. Per class: Σ delivered units, mean
/// delivered tokens, Δ, and the Strong Fit / Good Fit / Parity / Poor Fit /
/// Unknown verdict. W3-G04 asserted: at least one Strong Fit class.
#[test]
fn w3_win_001_workload_classification() {
    let provider = MockEmbeddingProvider::new();
    let docs = all_docs();
    let corpus = corpus(&docs);
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    assert_integrity(&docs, &merged);
    let questions = all_questions();

    // Per-class rollups: (aikoql score, rag score, max, aikoql tokens, rag
    // tokens, n questions). BTreeMap = deterministic iteration order.
    let mut classes: std::collections::BTreeMap<
        &str,
        (usize, usize, usize, usize, usize, usize),
    > = std::collections::BTreeMap::new();

    for (qi, q) in questions.iter().enumerate() {
        // ── AikoQL treatment ──────────────────────────────────────────────
        let pkg = compile_context(q.text, &merged, BUDGET);
        let delivered = render_context_markdown(&pkg);
        let a_tokens = delivered.len() / 4;

        // ── RAG baseline treatment ────────────────────────────────────────
        let ranked = common::rank(&corpus, q.text, &provider, false);
        let mut packed_text = String::new();
        for (f, i) in &ranked {
            let text = common::chunk_text(&corpus, f, *i);
            if (packed_text.len() + text.len() + 1) / 4 > BUDGET {
                break;
            }
            packed_text.push_str(text);
            packed_text.push(' ');
        }
        let r_tokens = packed_text.len() / 4;

        // ── Judge (the knowledge_bench token-containment judge) ──────────
        let (a_hits, _) = units_hit(&delivered, q);
        let (r_hits, _) = units_hit(&packed_text, q);
        // unknown-probe inverts: the units are traps — delivering them is
        // false confidence, so the correct payload scores 2/2.
        let (a_score, r_score) = if q.kind == "unknown-probe" {
            (2 - a_hits, 2 - r_hits)
        } else {
            (a_hits, r_hits)
        };

        let e = classes.entry(q.class).or_insert((0, 0, 0, 0, 0, 0));
        e.0 += a_score;
        e.1 += r_score;
        e.2 += 2;
        e.3 += a_tokens;
        e.4 += r_tokens;
        e.5 += 1;

        eprintln!(
            "[W3-WIN Q{qi} {class} {kind}] aikoql={a_score}/2 rag={r_score}/2 \
             aikoql_tokens={a_tokens} rag_tokens={r_tokens}",
            class = q.class,
            kind = q.kind,
        );
    }

    // The control question must tie (Q5: both treatments deliver both
    // units) or the bench is rigged in AikoQL's favor.
    if let Some((a, r, mx, ..)) = classes.get("W1") {
        assert_eq!(
            (*a, *r),
            (*mx, *mx),
            "control class W1 must be full parity, got aikoql {a} rag {r}"
        );
    }

    // ── Classification + verdict ─────────────────────────────────────────
    let mut strong_fit = 0usize;
    eprintln!("[W3-WIN-001] workload class table:");
    for (class, (a, r, mx, at, rt, n)) in &classes {
        let a_frac = *a as f64 / *mx as f64;
        let verdict = if a > r {
            if a_frac >= 0.75 {
                strong_fit += 1;
                "Strong Fit"
            } else {
                "Good Fit"
            }
        } else if a == r {
            if *mx == 0 || *a == 0 {
                "Unknown"
            } else {
                "Parity"
            }
        } else {
            "Poor Fit"
        };
        eprintln!(
            "  {class}: aikoql {a}/{mx} ({:.2}) vs rag {r}/{mx} — Δ {} — \
             tokens {}/{n}q vs {}/{n}q — {verdict}",
            a_frac,
            *a as isize - *r as isize,
            at / n,
            rt / n,
        );
    }

    // W3-G04 — at least one important workload class must show a repeatable
    // advantage. The multi-hop class is the structural one (zero-overlap
    // answer facts the chunk retriever cannot rank); if this fails, the
    // product thesis is broken and the release gate must block.
    assert!(
        strong_fit >= 1,
        "W3-G04 FAILED: no workload class shows a repeatable AikoQL advantage \
         (strong_fit={strong_fit})"
    );
}
