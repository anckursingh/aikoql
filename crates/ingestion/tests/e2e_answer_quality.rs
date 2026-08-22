//! PR-R (HLD §53 "End-to-end"): answer / citation / evidence correctness —
//! the only §53 stage without an instrument. §53:
//!
//! ```text
//! answer correctness
//! citation correctness
//! evidence correctness
//! ```
//!
//! AIKOQL is a knowledge store, not an answer engine (HLD §56/§59), so the
//! generator lives ONLY in this instrument: feature `answer_gen` (ureq,
//! env-configured, never in the default build) generates answers from a
//! local Ollama-compatible endpoint, and a **mechanical judge** scores
//! them — no LLM judge, which both keeps the instrument self-contained and
//! avoids self-judging bias:
//!
//! - **answer correctness**: the answer contains the golden answer's key
//!   tokens (at most one missing — small local models paraphrase, so exact
//!   match would measure phrasing, not knowledge).
//! - **citation correctness**: the answer cites `[n]` and the cited evidence
//!   chunk is qrel-relevant (the prompt demands numbered citations; a
//!   citation pointing at the wrong chunk is wrong).
//! - **evidence correctness**: the evidence pack given to the generator
//!   contains the qrel chunk — the retrieval instrument's Recall@K, reported
//!   per query (evidence quality is retrieval quality; no new machinery).
//!
//! The experiment measures whether retrieved evidence *improves answers*:
//! every query runs twice — with the top-3 lexical evidence chunks and
//! closed-book (no evidence) — and `gate_verdict` applies the §60 rule:
//! GO iff with-evidence answers are measurably better (≥ 0.2 over
//! closed-book) and themselves non-trivial (≥ 0.5). The verdict rests on
//! measurement against a real model, exactly like the PR-P real-model run.
//!
//! Env: `AIKOQL_ANSWER_ENDPOINT` (default `http://127.0.0.1:11434` — the
//! Ollama API), `AIKOQL_ANSWER_MODEL` (required — the caller's installed
//! model, e.g. `qwen2.5:3b`). Without the model env the test SKIPs with a
//! message (the real_model_bench convention); CI never runs a model.
//! Transport/parse failures score that answer 0.0 and are counted — a
//! failed generation must never silently become a guessed number (§58:
//! untrusted optional output).

#![cfg(feature = "answer_gen")]

mod common;

use std::time::Instant;

/// Golden answer per query (same order as `common::QUERIES`): the key
/// tokens a correct answer must contain, authored from the fixture content.
const GOLDEN_ANSWERS: &[&str] = &[
    "$10M",             // Q3 2025 revenue (plain-text.pdf)
    "Acme Corporation", // who publishes quarterly reports
    "30",               // Alice Smith age (tables.pdf)
    "1200",             // North America revenue
    "4000",             // Q1 2025 units sold (complex-table.pdf)
    "24 months",        // Home Automation warranty
    "Q2",               // highest total revenue (charts.pdf)
    "Q2",               // best-performing three-month period (paraphrase probe)
    "Gateway",          // client → database path (architecture-diagram.pdf)
    "Billing Team",     // who validates payments (mixed-report.pdf)
    "Ledger Team",      // who owns the financial record book (paraphrase probe)
    "$10M",             // Globex Industries revenue (annual-report.pdf)
    "continued growth", // Gamma Partners expectation
    "E = mc^2",         // energy-mass equation (formulas.pdf)
    "Company logo",     // figure 3 (images.pdf)
];

/// Evidence chunks handed to the generator per query (top-3 lexical).
const EVIDENCE_K: usize = 3;

/// One query's full measurement row.
struct Row {
    with_correct: bool,
    without_correct: bool,
    citation_correct: bool,
    evidence_recall: f32,
    generation_failed: u32,
}

/// §53 answer-correctness judge: the answer contains the golden key tokens,
/// with at most one missing (exact match measures phrasing, not knowledge;
/// a 1-token golden must be present). Pure — unit-tested below.
fn answer_correct(answer: &str, golden: &str) -> bool {
    let golden_tokens = common::tokens(golden);
    let answer_tokens = common::tokens(answer);
    let hits = golden_tokens
        .iter()
        .filter(|t| answer_tokens.contains(*t))
        .count();
    hits >= golden_tokens.len().saturating_sub(1).max(1)
}

/// §53 citation-correctness judge: the answer cites at least one `[n]`
/// whose evidence chunk is qrel-relevant. Pure — unit-tested below.
fn citation_correct(answer: &str, evidence: &[String], qrels: &[String]) -> bool {
    let mut any_citation = false;
    let mut cited_relevant = false;
    let mut rest = answer;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        let inner = &rest[open + 1..open + close];
        if let Ok(n) = inner.trim().parse::<usize>() {
            any_citation = true;
            if (1..=evidence.len()).contains(&n)
                && qrels
                    .iter()
                    .any(|q| common::is_relevant(&evidence[n - 1], q))
            {
                cited_relevant = true;
            }
        }
        rest = &rest[open + close + 1..];
    }
    // A citation to a relevant chunk is correct; no citation, or only
    // citations to wrong chunks, is wrong (the prompt demands [n]).
    any_citation && cited_relevant
}

/// §60-style gate: GO iff evidence measurably improves answers (≥ 0.2 over
/// closed-book) and the with-evidence correctness is non-trivial (≥ 0.5).
/// Pure — unit-tested below.
fn gate_verdict(with_evidence: f32, without_evidence: f32) -> bool {
    with_evidence >= 0.5 && with_evidence >= without_evidence + 0.2
}

/// One generation against an Ollama-compatible `/api/chat` endpoint.
/// `Ok(None)` when no answer came back (transport/parse failure).
fn generate(endpoint: &str, model: &str, system: &str, user: &str) -> Option<String> {
    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "options": { "temperature": 0 },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    let agent: ureq::Agent = ureq::Agent::config_builder()
        // Generation can be slow on CPU-only local models; a stuck call
        // must not hang the harness forever.
        .timeout_global(Some(std::time::Duration::from_secs(180)))
        .build()
        .into();
    agent
        .post(&format!("{}/api/chat", endpoint))
        .header("Content-Type", "application/json")
        .send_json(body)
        .ok()
        .and_then(|resp| resp.into_body().read_json::<serde_json::Value>().ok())
        .and_then(|v| v["message"]["content"].as_str().map(str::to_string))
        .filter(|s| !s.trim().is_empty())
}

fn render_evidence(evidence: &[String]) -> String {
    evidence
        .iter()
        .enumerate()
        .map(|(i, text)| format!("[{}] {}", i + 1, text.replace('\n', " ")))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The §53 end-to-end instrument: 15 queries × 2 conditions (with /
/// without evidence) through a live generator, judged mechanically.
/// SKIPs without `AIKOQL_ANSWER_MODEL`; prints per-query rows, a summary,
/// and the gate verdict. No asserts on generated answers — a model's
/// answers are what they are; the verdict is printed, not enforced.
#[test]
fn e2e_answer_quality() {
    let Some(model) = std::env::var("AIKOQL_ANSWER_MODEL").ok() else {
        eprintln!(
            "[E2E] SKIP — set AIKOQL_ANSWER_MODEL (and optionally AIKOQL_ANSWER_ENDPOINT) to \
             run the §53 end-to-end instrument against a local model"
        );
        return;
    };
    let endpoint = std::env::var("AIKOQL_ANSWER_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());

    let provider = aikoql_ingestion::MockEmbeddingProvider::new();
    let (corpus, _) = common::corpus(&aikoql_ingestion::RuleBoundaryDetector, &provider);
    let qrels: Vec<Vec<String>> = common::QUERIES
        .iter()
        .map(|q| {
            q.relevant
                .iter()
                .map(|(f, i)| common::chunk_text(&corpus, f, *i).to_string())
                .collect()
        })
        .collect();

    let system_with = "Answer the question using ONLY the numbered source excerpts below. \
                       Cite every source you use with its [n] number. If the sources do not \
                       answer the question, answer exactly: not in sources.";
    let system_without = "Answer the question briefly from your own knowledge.";

    let started = Instant::now();
    let mut rows: Vec<Row> = Vec::new();
    let mut calls = 0usize;

    for (qi, q) in common::QUERIES.iter().enumerate() {
        // Baseline retriever: lexical top-3 (the §60 rule-baseline ranker).
        let ranked = common::rank(&corpus, q.text, &provider, false);
        let evidence: Vec<String> = ranked
            .iter()
            .take(EVIDENCE_K)
            .map(|(f, i)| common::chunk_text(&corpus, f, *i).to_string())
            .collect();

        let evidence_recall = common::recall_at_k(&ranked, &corpus, &qrels[qi], EVIDENCE_K);

        let user_with = format!(
            "Question: {}\n\nSources:\n{}",
            q.text,
            render_evidence(&evidence)
        );
        let answer_with = generate(&endpoint, &model, system_with, &user_with);
        calls += 1;
        let answer_without = generate(&endpoint, &model, system_without, q.text);
        calls += 1;

        let with_correct = answer_with
            .as_deref()
            .map(|a| answer_correct(a, GOLDEN_ANSWERS[qi]))
            .unwrap_or(false);
        let without_correct = answer_without
            .as_deref()
            .map(|a| answer_correct(a, GOLDEN_ANSWERS[qi]))
            .unwrap_or(false);
        let citation = answer_with
            .as_deref()
            .map(|a| citation_correct(a, &evidence, &qrels[qi]))
            .unwrap_or(false);

        eprintln!(
            "[E2E-A {:?}] with={} ({:?}) without={} ({:?}) citation={} evidence={evidence_recall}",
            q.text,
            with_correct,
            answer_with.as_deref().map(|a| a.replace('\n', " ")),
            without_correct,
            answer_without.as_deref().map(|a| a.replace('\n', " ")),
            citation
        );
        rows.push(Row {
            with_correct,
            without_correct,
            citation_correct: citation,
            evidence_recall,
            generation_failed: u32::from(answer_with.is_none())
                + u32::from(answer_without.is_none()),
        });
    }

    let n = rows.len() as f32;
    let with = rows.iter().filter(|r| r.with_correct).count() as f32 / n;
    let without = rows.iter().filter(|r| r.without_correct).count() as f32 / n;
    let citation = rows.iter().filter(|r| r.citation_correct).count() as f32 / n;
    let evidence = rows.iter().map(|r| r.evidence_recall).sum::<f32>() / n;
    let failed_generations: u32 = rows.iter().map(|r| r.generation_failed).sum();
    let verdict = if gate_verdict(with, without) {
        "GO"
    } else {
        "NO-GO"
    };
    eprintln!(
        "[E2E-SUMMARY] model={model} with-evidence={with:.3} closed-book={without:.3} \
         citation={citation:.3} evidence-R@3={evidence:.3} calls={calls} failed={failed_generations} \
         wall={:.1}s verdict={verdict}",
        started.elapsed().as_secs_f32()
    );
}

// ---------------------------------------------------------------------------
// Pure-judge tests — the mechanical judges are the instrument's semantics
// and must not drift silently.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_correct_accepts_paraphrase_with_key_tokens() {
        assert!(answer_correct("The revenue for Q3 2025 was $10M.", "$10M"));
        assert!(answer_correct("Acme Corp published it", "Acme Corporation"));
        assert!(answer_correct("She is 30 years old", "30"));
        // One golden token missing is tolerated; both missing is not.
        assert!(answer_correct("Acme is the publisher", "Acme Corporation"));
        assert!(!answer_correct("Some other company", "Acme Corporation"));
    }

    #[test]
    fn answer_correct_rejects_empty_and_wrong() {
        assert!(!answer_correct("not in sources", "Q2"));
        assert!(!answer_correct("", "30"));
    }

    #[test]
    fn citation_correct_requires_relevant_cited_chunk() {
        let evidence = vec!["Region: North America 1200".to_string()];
        let qrels = vec!["Region: North America".to_string()];
        assert!(citation_correct("Revenue is 1200 [1]", &evidence, &qrels));
        // Citing a wrong (irrelevant) chunk is wrong.
        let other = vec!["Unrelated text about weather".to_string()];
        assert!(!citation_correct("Revenue is 1200 [1]", &other, &qrels));
        // No citation at all is wrong.
        assert!(!citation_correct("Revenue is 1200", &evidence, &qrels));
        // Out-of-range citation is wrong.
        assert!(!citation_correct("Revenue is 1200 [7]", &evidence, &qrels));
    }

    #[test]
    fn gate_verdict_requires_measured_gain() {
        assert!(gate_verdict(0.7, 0.3), "strong gain → GO");
        assert!(!gate_verdict(0.7, 0.6), "no material gain → NO-GO");
        assert!(!gate_verdict(0.4, 0.0), "weak even with gain → NO-GO");
        assert!(!gate_verdict(0.0, 0.0), "nothing worked → NO-GO");
    }
}
