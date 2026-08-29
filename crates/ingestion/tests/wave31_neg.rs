//! Wave 3.1 (MVP-QA-003A) — W31-NEG-001 mandatory falsification.
//!
//! The spec's four mandated adversarial scenarios, where AIKOQL must be
//! allowed to lose:
//!   1. simple exact lookup   — the answer is a literal string a grep finds;
//!   2. simple document Q&A   — a direct question over one prose doc;
//!   3. small corpus          — a two-doc KB, indexing has nothing to pay for;
//!   4. single-source query   — the answer lives in one source of five.
//!
//! Each scenario runs two treatments under the same frozen judge
//! (`units_hit`): the kernel (merge → compile → render, BUDGET-bounded)
//! and the plain baseline (keyword-overlap ranking + budget packing —
//! the thing a developer writes before any RAG stack). Columns per row:
//! delivered units, tokens, latency, G11 cost (wave31_sim::cost).
//!
//! Pinned classification law (declared BEFORE first measurement; the
//! verdicts are computed from the measured columns by the pure
//! classifier, never reclassified after the fact):
//!   win            strictly more delivered units, OR equal units AND no
//!                  more tokens (equal delivery, no extra machinery);
//!   loss           strictly fewer delivered units;
//!   no-advantage   equal units, strictly more tokens — machinery
//!                  without delivery.
//! The test asserts the scenario set matches the mandated four and that
//! every row's class agrees with the law. The measured table is printed
//! as raw evidence for the docs — parity/losses get whatever the rows
//! honestly are; nothing here is thresholded to aikoql's favor.

mod common;

use std::time::Instant;

use aikoql_ingestion::{
    compile_context, merge_knowledge_ir, render_context_markdown, EntityCandidate,
    Evidence, FactCandidate, KnowledgeIr,
};
use common::trackb::{assert_integrity, g, units_hit, Doc, Question};
use common::wave31_sim::{cost, BUDGET};

fn ev(doc: &str) -> Evidence {
    Evidence {
        document_id: Some(doc.into()),
        extractor: "w31-neg-synthetic".into(),
        confidence: 0.9,
        ..Evidence::default()
    }
}

fn entity(name: &str, ty: &str, mention: &str, doc: &str) -> EntityCandidate {
    EntityCandidate {
        name: name.into(),
        type_hint: Some(ty.into()),
        mentions: vec![mention.into()],
        confidence: 0.9,
        evidence: ev(doc),
    }
}

fn fact(statement: &str, anchors: &[&str], doc: &str) -> FactCandidate {
    FactCandidate {
        statement: statement.into(),
        entities: anchors.iter().map(|s| s.to_string()).collect(),
        confidence: 0.9,
        evidence: ev(doc),
        snippet: None,
    }
}

fn doc(id: &'static str, chunks: &'static [&'static str], ir: KnowledgeIr) -> Doc {
    Doc { id, chunks, ir }
}

/// One mandated adversarial scenario.
struct Scenario {
    key: &'static str,
    docs: Vec<Doc>,
    question: Question,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        // 1. Simple exact lookup — answer is a literal "100" a grep finds.
        Scenario {
            key: "exact-lookup",
            docs: vec![
                doc(
                    "kb-ops",
                    &[
                        "The deployment capacity is 100 units.",
                        "The cache holds 512 entries.",
                    ],
                    KnowledgeIr {
                        entities: vec![entity("deployment", "service", "deployment", "kb-ops")],
                        facts: vec![fact(
                            "The deployment capacity is 100 units.",
                            &["deployment"],
                            "kb-ops",
                        )],
                        ..KnowledgeIr::default()
                    },
                ),
                doc(
                    "kb-mkt",
                    &["The marketing site mentions deployment capacity planning for Q3."],
                    KnowledgeIr {
                        entities: vec![entity("deployment", "service", "deployment", "kb-mkt")],
                        facts: vec![fact(
                            "The marketing site mentions deployment capacity planning for Q3.",
                            &["deployment"],
                            "kb-mkt",
                        )],
                        ..KnowledgeIr::default()
                    },
                ),
            ],
            question: Question {
                text: "What is the deployment capacity?",
                kind: "lookup",
                class: "W1",
                units: ["100", "units"],
                gt: g("none", "kb-ops", "none", "current", "deployment_observed", "none"),
            },
        },
        // 2. Simple document Q&A — a direct question over one prose doc.
        Scenario {
            key: "doc-qa",
            docs: vec![doc(
                "kb-build",
                &[
                    "The build is flaky because the cache is not purged between runs.",
                    "Stale artifacts persist in the shared cache directory.",
                ],
                KnowledgeIr {
                    entities: vec![entity("build", "service", "build", "kb-build")],
                    facts: vec![
                        fact(
                            "The build is flaky because the cache is not purged between runs.",
                            &["build"],
                            "kb-build",
                        ),
                        fact(
                            "Stale artifacts persist in the shared cache directory.",
                            &["build"],
                            "kb-build",
                        ),
                    ],
                    ..KnowledgeIr::default()
                },
            )],
            question: Question {
                text: "Why is the build flaky?",
                kind: "qa",
                class: "W1",
                units: ["cache is not purged", "stale artifacts persist"],
                gt: g("none", "kb-build", "none", "current", "source_code", "none"),
            },
        },
        // 3. Small corpus — two docs total, nothing for the machinery to
        //    index. Overhead is pure cost here.
        Scenario {
            key: "small-corpus",
            docs: vec![
                doc(
                    "kb-tiny-a",
                    &["The answer to everything is 42."],
                    KnowledgeIr {
                        entities: vec![entity("everything", "concept", "everything", "kb-tiny-a")],
                        facts: vec![fact(
                            "The answer to everything is 42.",
                            &["everything"],
                            "kb-tiny-a",
                        )],
                        ..KnowledgeIr::default()
                    },
                ),
                doc(
                    "kb-tiny-b",
                    &["The sky is blue and the grass is green."],
                    KnowledgeIr {
                        entities: vec![entity("sky", "concept", "sky", "kb-tiny-b")],
                        facts: vec![fact(
                            "The sky is blue and the grass is green.",
                            &["sky"],
                            "kb-tiny-b",
                        )],
                        ..KnowledgeIr::default()
                    },
                ),
            ],
            question: Question {
                text: "What is the answer to everything?",
                kind: "lookup",
                class: "W1",
                units: ["42", "everything"],
                gt: g("none", "kb-tiny-a", "none", "current", "documentation", "none"),
            },
        },
        // 4. Single-source query — the answer lives in one source of five;
        //    multi-source synthesis is irrelevant to the task.
        Scenario {
            key: "single-source",
            docs: vec![
                doc(
                    "kb-src-1",
                    &["The severity threshold is 7."],
                    KnowledgeIr {
                        entities: vec![entity("severity", "policy", "severity", "kb-src-1")],
                        facts: vec![fact(
                            "The severity threshold is 7.",
                            &["severity"],
                            "kb-src-1",
                        )],
                        ..KnowledgeIr::default()
                    },
                ),
                doc(
                    "kb-src-2",
                    &["The payroll runs on Fridays."],
                    KnowledgeIr {
                        entities: vec![entity("payroll", "system", "payroll", "kb-src-2")],
                        facts: vec![fact("The payroll runs on Fridays.", &["payroll"], "kb-src-2")],
                        ..KnowledgeIr::default()
                    },
                ),
                doc(
                    "kb-src-3",
                    &["Coffee is restocked on Mondays."],
                    KnowledgeIr {
                        entities: vec![entity("coffee", "supply", "coffee", "kb-src-3")],
                        facts: vec![fact(
                            "Coffee is restocked on Mondays.",
                            &["coffee"],
                            "kb-src-3",
                        )],
                        ..KnowledgeIr::default()
                    },
                ),
                doc(
                    "kb-src-4",
                    &["The VPN rotates keys monthly."],
                    KnowledgeIr {
                        entities: vec![entity("vpn", "system", "vpn", "kb-src-4")],
                        facts: vec![fact("The VPN rotates keys monthly.", &["vpn"], "kb-src-4")],
                        ..KnowledgeIr::default()
                    },
                ),
                doc(
                    "kb-src-5",
                    &["Staging mirrors production config."],
                    KnowledgeIr {
                        entities: vec![entity("staging", "env", "staging", "kb-src-5")],
                        facts: vec![fact(
                            "Staging mirrors production config.",
                            &["staging"],
                            "kb-src-5",
                        )],
                        ..KnowledgeIr::default()
                    },
                ),
            ],
            question: Question {
                text: "What is the severity threshold?",
                kind: "lookup",
                class: "W1",
                units: ["7", "severity"],
                gt: g("none", "kb-src-1", "none", "current", "policy", "none"),
            },
        },
    ]
}

/// The plain baseline: rank chunks by question-token overlap (the
/// keyword scan a developer writes first — no embeddings, no IR), pack
/// into the same budget the kernel gets.
fn plain_rank(q: &Question, corpus: &[common::CorpusChunk]) -> Vec<usize> {
    let q_toks = common::tokens(q.text);
    let mut scored: Vec<(usize, usize)> = corpus
        .iter()
        .enumerate()
        .map(|(i, c)| {
            (
                i,
                common::tokens(&c.2)
                    .iter()
                    .filter(|t| q_toks.contains(*t))
                    .count(),
            )
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, _)| i).collect()
}

/// The pinned classification law (spec §5 W31-NEG-001: AIKOQL must be
/// allowed to lose; no advantage may be claimed from machinery that
/// delivered nothing extra).
fn classify(a_units: usize, p_units: usize, a_tok: usize, p_tok: usize) -> &'static str {
    use std::cmp::Ordering::*;
    match a_units.cmp(&p_units) {
        Greater => "win",
        Less => "loss",
        Equal => {
            if a_tok <= p_tok {
                "win"
            } else {
                "no-advantage"
            }
        }
    }
}

/// The four scenarios the spec mandates — the falsification surface.
const MANDATED: [&str; 4] = ["exact-lookup", "doc-qa", "small-corpus", "single-source"];

#[test]
fn w31_neg_001_mandatory_falsification() {
    let scs = scenarios();

    // Coverage: exactly the mandated four, nothing dropped, nothing added.
    let keys: Vec<&str> = scs.iter().map(|s| s.key).collect();
    assert_eq!(keys, MANDATED.to_vec(), "scenario set must match the mandated four");

    let mut table = String::from(
        "scenario        | aikoql units | plain units | aikoql tok | plain tok | aikoql µs | plain µs | verdict\n",
    );
    table.push_str("----------------|--------------|-------------|------------|-----------|-----------|----------|----------------\n");

    for sc in &scs {
        assert_integrity(&sc.docs, &merge_knowledge_ir(
            &sc.docs.iter().map(|d| d.ir.clone()).collect::<Vec<_>>(),
        ));
        let corpus = common::trackb::corpus(&sc.docs);
        let merged = merge_knowledge_ir(&sc.docs.iter().map(|d| d.ir.clone()).collect::<Vec<_>>());

        // The kernel treatment.
        let t0 = Instant::now();
        let pkg = compile_context(sc.question.text, &merged, BUDGET);
        let aikoql = render_context_markdown(&pkg);
        let am = t0.elapsed().as_micros();
        let a_units = units_hit(&aikoql, &sc.question).0;
        let a_tok = aikoql.len() / 4;

        // The plain baseline treatment — same judge, same budget.
        let t0 = Instant::now();
        let plain = {
            let order = plain_rank(&sc.question, &corpus);
            common::wave31_sim::pack_budgeted(&order, &corpus)
        };
        let pm = t0.elapsed().as_micros();
        let p_units = units_hit(&plain, &sc.question).0;
        let p_tok = plain.len() / 4;

        // The law: the verdict is computed from the measured columns,
        // never reclassified.
        let verdict = classify(a_units, p_units, a_tok, p_tok);
        if verdict == "win" {
            assert!(
                a_units > p_units || (a_units == p_units && a_tok <= p_tok),
                "{}: win requires strictly more units, or equal units with no more tokens \
                 (a={}/{}, p={}/{})",
                sc.key, a_units, a_tok, p_units, p_tok
            );
        }
        if verdict == "loss" {
            assert!(a_units < p_units, "{}: loss requires strictly fewer units", sc.key);
        }

        table.push_str(&format!(
            "{:<15} | {:>12} | {:>11} | {:>10} | {:>9} | {:>9} | {:>8} | {:<15}\n",
            sc.key, a_units, p_units, a_tok, p_tok, am, pm, verdict
        ));

        // G11 cost per query (printed, not thresholded — a falsification
        // row may cost more and say so).
        table.push_str(&format!(
            "                | cost {:>7} | cost {:>6} |            |           |           |          |\n",
            format!("${:.6}", cost(a_tok, 1)),
            format!("${:.6}", cost(p_tok, 1)),
        ));
    }

    println!(
        "\n[W31-NEG-001 mandatory falsification — measured, verdicts computed not rigged]\n{}",
        table
    );
}
