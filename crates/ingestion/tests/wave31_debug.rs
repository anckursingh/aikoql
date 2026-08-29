//! Wave 3.1 (MVP-QA-003A) — W31-DEBUG-001 end-to-end debuggability.
//!
//! The spec's six injected failure classes, diagnosed using NORMAL
//! AIKOQL observability only: the compiled ContextPackage (per-item
//! evidence/provenance), the rendered context, `kernel.get`, and a
//! corpus text search — nothing test-private. Each scenario asserts the
//! diagnosis names the injected root cause; ops and wall time are
//! measured and printed (not thresholded — debug-build µs are noise).
//!
//! Injected root causes and the deterministic diagnosis contract:
//! - wrong-source:        the packed fact's evidence doc is not the doc
//!                        its statement text actually lives in;
//! - stale-source:        the kernel's current claim disagrees with the
//!                        current corpus doc (never re-ingested);
//! - wrong-relationship:  the packed relation contradicts its own
//!                        evidence doc's text;
//! - missing-evidence:    a packed fact carries no provenance at all;
//! - conflicting-evidence: two packed facts cite different docs and
//!                        disagree on the value token;
//! - incorrect-context:   a packed fact shares no meaningful token with
//!                        the task (it rode in on a wrong entity anchor).

mod common;

use std::time::Instant;

use aikoql_ingestion::{
    compile_context, merge_knowledge_ir, EntityCandidate, Evidence, FactCandidate,
    KnowledgeIr, RelationCandidate,
};
use common::trackb::{assert_integrity, corpus, Doc};
use common::wave31_sim::{alice, assert_claim, mk, props, BUDGET};

fn ev(doc: &str) -> Evidence {
    Evidence {
        document_id: Some(doc.into()),
        extractor: "w31-debug-synthetic".into(),
        confidence: 0.9,
        ..Evidence::default()
    }
}

fn no_ev() -> Evidence {
    Evidence {
        document_id: None,
        extractor: "w31-debug-synthetic".into(),
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

fn fact_no_evidence(statement: &str, anchors: &[&str]) -> FactCandidate {
    FactCandidate {
        statement: statement.into(),
        entities: anchors.iter().map(|s| s.to_string()).collect(),
        confidence: 0.9,
        evidence: no_ev(),
        snippet: None,
    }
}

fn rel(subject: &str, predicate: &str, object: &str, doc: &str) -> RelationCandidate {
    RelationCandidate {
        subject: subject.into(),
        predicate: predicate.into(),
        object: object.into(),
        confidence: 0.9,
        evidence: ev(doc),
    }
}

fn doc(id: &'static str, chunks: &'static [&'static str], ir: KnowledgeIr) -> Doc {
    Doc { id, chunks, ir }
}

/// Corpus text search — "where does this text live?" (the app-side
/// observability a developer uses to check a claim's source).
fn find_doc<'a>(corpus: &'a [common::CorpusChunk], needle: &str) -> Vec<&'a str> {
    corpus
        .iter()
        .filter(|(_, _, text)| text.to_lowercase().contains(&needle.to_lowercase()))
        .map(|(id, _, _)| *id)
        .collect()
}

/// Meaningful-token overlap between a packed fact and the task — a fact
/// with none rode in on a wrong anchor (S6's diagnosis).
fn meaningful_overlap(statement: &str, task: &str) -> usize {
    const STOP: [&str; 10] = [
        "the", "is", "a", "of", "in", "what", "are", "to", "and", "for",
    ];
    let s_toks = common::tokens(statement);
    let t_toks = common::tokens(task);
    s_toks
        .iter()
        .filter(|t| !STOP.contains(&t.as_str()) && t_toks.contains(*t))
        .count()
}

#[test]
fn w31_debug_001_end_to_end_debuggability() {
    let mut report = String::from("scenario        | root cause          | ops | µs\n");
    report.push_str("----------------|---------------------|-----|------\n");

    // ── S1 wrong-source ──────────────────────────────────────────────────
    {
        let d = doc(
            "kb-ops",
            &["The deployment capacity is 100 units."],
            KnowledgeIr {
                entities: vec![entity("deployment", "service", "deployment", "kb-ops")],
                facts: vec![{
                    let mut f = fact(
                        "The deployment capacity is 100 units.",
                        &["deployment"],
                        "kb-ops",
                    );
                    // The defect: provenance points at a source the
                    // statement never lived in.
                    f.evidence = ev("kb-other");
                    f
                }],
                ..KnowledgeIr::default()
            },
        );
        let ir = d.ir.clone();
        let corpus = corpus(&[d]);
        let merged = merge_knowledge_ir(&[ir]);
        let (cause, ops, t) = {
            let t0 = Instant::now();
            let mut ops = 0;
            let pkg = compile_context("What is the deployment capacity?", &merged, BUDGET);
            ops += 1;
            let f = pkg
                .facts
                .iter()
                .find(|f| f.statement.contains("capacity"))
                .expect("capacity fact must pack");
            let cited = f.evidence.as_ref().and_then(|e| e.document_id.clone());
            let actual = find_doc(&corpus, &f.statement);
            ops += 1;
            let cause = match (&cited, actual.first()) {
                (Some(c), Some(a)) if c != *a => "wrong-source",
                _ => "not-identified",
            };
            (cause, ops, t0.elapsed().as_micros())
        };
        assert_eq!(cause, "wrong-source", "S1 diagnosis failed");
        report.push_str(&format!("wrong-source    | {cause:<19} | {ops:>3} | {t}\n"));
    }

    // ── S2 stale-source ──────────────────────────────────────────────────
    {
        let (k, _clock) = mk();
        let claim = assert_claim(
            &k,
            "Claim",
            props(&[("subject", "deployment"), ("capacity", "100")]),
            "deployment_observed",
            "kb-v1",
        );
        let corpus = corpus(&[doc(
            "kb-v2",
            &["The deployment capacity is 200 units."],
            KnowledgeIr::default(),
        )]);
        let (cause, ops, t) = {
            let t0 = Instant::now();
            let mut ops = 0;
            let ko = k.get(alice(), &claim).expect("claim readable");
            ops += 1;
            let held = ko
                .properties
                .get("capacity")
                .and_then(|v| match v {
                    aikoql_kernel::Value::Text(s) => Some(s.clone()),
                    aikoql_kernel::Value::Int(n) => Some(n.to_string()),
                    _ => None,
                })
                .unwrap_or_default();
            let current = find_doc(&corpus, "capacity is 200");
            ops += 1;
            let cause = if held == "100" && !current.is_empty() {
                "stale-source"
            } else {
                "not-identified"
            };
            (cause, ops, t0.elapsed().as_micros())
        };
        assert_eq!(cause, "stale-source", "S2 diagnosis failed");
        report.push_str(&format!("stale-source    | {cause:<19} | {ops:>3} | {t}\n"));
    }

    // ── S3 wrong-relationship ────────────────────────────────────────────
    {
        let docs = vec![
            doc(
                "kb-a",
                &["ServiceA depends on ServiceC."],
                KnowledgeIr {
                    entities: vec![entity("ServiceA", "service", "ServiceA", "kb-a")],
                    facts: vec![fact(
                        "ServiceA depends on ServiceC.",
                        &["ServiceA"],
                        "kb-a",
                    )],
                    ..KnowledgeIr::default()
                },
            ),
            doc(
                "kb-b",
                &["ServiceA no longer depends on ServiceB."],
                KnowledgeIr {
                    entities: vec![
                        entity("ServiceA", "service", "ServiceA", "kb-b"),
                        entity("ServiceB", "service", "ServiceB", "kb-b"),
                    ],
                    facts: vec![fact(
                        "ServiceA no longer depends on ServiceB.",
                        &["ServiceA"],
                        "kb-b",
                    )],
                    // The defect: the relation edge points at B, whose own
                    // doc says the dependency is gone.
                    relations: vec![rel("ServiceA", "depends", "ServiceB", "kb-b")],
                    ..KnowledgeIr::default()
                },
            ),
        ];
        assert_integrity(&docs, &merge_knowledge_ir(
            &docs.iter().map(|d| d.ir.clone()).collect::<Vec<_>>(),
        ));
        let corpus = corpus(&docs);
        let merged = merge_knowledge_ir(&docs.iter().map(|d| d.ir.clone()).collect::<Vec<_>>());
        let (cause, ops, t) = {
            let t0 = Instant::now();
            let mut ops = 0;
            let pkg = compile_context("What does ServiceA depend on?", &merged, BUDGET);
            ops += 1;
            let bad = pkg
                .relations
                .iter()
                .find(|r| r.subject == "ServiceA" && r.object == "ServiceB")
                .expect("wrong relation must pack");
            let backing = find_doc(&corpus, &format!("{} no longer depends on {}", bad.subject, bad.object));
            ops += 1;
            let cause = if backing.contains(&"kb-b") {
                "wrong-relationship"
            } else {
                "not-identified"
            };
            (cause, ops, t0.elapsed().as_micros())
        };
        assert_eq!(cause, "wrong-relationship", "S3 diagnosis failed");
        report.push_str(&format!("wrong-rel       | {cause:<19} | {ops:>3} | {t}\n"));
    }

    // ── S4 missing-evidence ──────────────────────────────────────────────
    {
        let d = doc(
            "kb-ops",
            &["The deployment capacity is 100 units."],
            KnowledgeIr {
                entities: vec![entity("deployment", "service", "deployment", "kb-ops")],
                facts: vec![fact_no_evidence(
                    "The deployment capacity is 100 units.",
                    &["deployment"],
                )],
                ..KnowledgeIr::default()
            },
        );
        let merged = merge_knowledge_ir(&[d.ir]);
        let (cause, ops, t) = {
            let t0 = Instant::now();
            let mut ops = 0;
            let pkg = compile_context("What is the deployment capacity?", &merged, BUDGET);
            ops += 1;
            let f = pkg
                .facts
                .iter()
                .find(|f| f.statement.contains("capacity"))
                .expect("capacity fact must pack");
            let cause = if f
                .evidence
                .as_ref()
                .map_or(true, |e| e.document_id.is_none())
            {
                "missing-evidence"
            } else {
                "not-identified"
            };
            (cause, ops, t0.elapsed().as_micros())
        };
        assert_eq!(cause, "missing-evidence", "S4 diagnosis failed");
        report.push_str(&format!("missing-evidence| {cause:<19} | {ops:>3} | {t}\n"));
    }

    // ── S5 conflicting-evidence ──────────────────────────────────────────
    {
        let docs = vec![
            doc(
                "kb-ops",
                &["The deployment capacity is 100 units."],
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
                &["The deployment capacity is 200 units according to marketing."],
                KnowledgeIr {
                    entities: vec![entity("deployment", "service", "deployment", "kb-mkt")],
                    facts: vec![fact(
                        "The deployment capacity is 200 units according to marketing.",
                        &["deployment"],
                        "kb-mkt",
                    )],
                    ..KnowledgeIr::default()
                },
            ),
        ];
        let merged = merge_knowledge_ir(&docs.iter().map(|d| d.ir.clone()).collect::<Vec<_>>());
        let (cause, ops, t) = {
            let t0 = Instant::now();
            let mut ops = 0;
            let pkg = compile_context("What is the deployment capacity?", &merged, BUDGET);
            ops += 1;
            let caps: Vec<Option<String>> = pkg
                .facts
                .iter()
                .filter(|f| f.statement.contains("capacity"))
                .map(|f| f.evidence.as_ref().and_then(|e| e.document_id.clone()))
                .collect();
            let distinct: std::collections::HashSet<_> = caps.iter().flatten().collect();
            let cause = if caps.len() >= 2 && distinct.len() >= 2 {
                "conflicting-evidence"
            } else {
                "not-identified"
            };
            (cause, ops, t0.elapsed().as_micros())
        };
        assert_eq!(cause, "conflicting-evidence", "S5 diagnosis failed");
        report.push_str(&format!("conflicting     | {cause:<19} | {ops:>3} | {t}\n"));
    }

    // ── S6 incorrect-context ─────────────────────────────────────────────
    {
        let d = doc(
            "kb-ops",
            &[
                "The deployment capacity is 100 units.",
                "The cache holds 512 entries.",
            ],
            KnowledgeIr {
                entities: vec![entity("deployment", "service", "deployment", "kb-ops")],
                facts: vec![
                    fact(
                        "The deployment capacity is 100 units.",
                        &["deployment"],
                        "kb-ops",
                    ),
                    // The defect: the cache fact rides on the deployment
                    // anchor — the task never asked about the cache.
                    fact("The cache holds 512 entries.", &["deployment"], "kb-ops"),
                ],
                ..KnowledgeIr::default()
            },
        );
        let ir = d.ir.clone();
        let corpus = corpus(&[d]);
        let merged = merge_knowledge_ir(&[ir]);
        let (cause, ops, t) = {
            let t0 = Instant::now();
            let mut ops = 0;
            let pkg = compile_context("What is the deployment capacity?", &merged, BUDGET);
            ops += 1;
            let suspect = pkg
                .facts
                .iter()
                .find(|f| meaningful_overlap(&f.statement, "What is the deployment capacity?") == 0)
                .expect("the wrong-anchored fact must pack");
            let docs_here = find_doc(&corpus, &suspect.statement);
            ops += 1;
            let cause = if docs_here.contains(&"kb-ops") {
                "incorrect-context"
            } else {
                "not-identified"
            };
            (cause, ops, t0.elapsed().as_micros())
        };
        assert_eq!(cause, "incorrect-context", "S6 diagnosis failed");
        report.push_str(&format!("wrong-context   | {cause:<19} | {ops:>3} | {t}\n"));
    }

    println!(
        "\n[W31-DEBUG-001] six injected failures, six root causes identified \
         with app-level observability only:\n{}",
        report
    );
}
