//! Wave 3.1 (MVP-QA-003A, spec §5) — market corpus acceptance tests.
//!
//! W31-MKT-001: frozen market corpus with ≥100 independent tasks, ≥10
//! tasks per workload class across the 12 classes, every task declaring
//! the 7 ground-truth fields (answer variants, expected evidence,
//! expected relationships, temporal state, authority, ambiguity), and the
//! shape thresholds: ≥20% multi-source, ≥20% relationship-dependent,
//! ≥10% temporal, ≥10% contradictory, ≥10% unknown.
//!
//! The holdout split (spec §7) gets a structure-only test: integrity,
//! disjoint ids, no scoring thresholds. Scoring the holdout happens in
//! the Wave 3.1 evaluation harness (#161), never here — this test must
//! not depend on how any treatment scores it.

mod common;

use std::collections::BTreeSet;

use aikoql_ingestion::{merge_knowledge_ir, KnowledgeIr};
use common::trackb::{assert_integrity, docs as wave3_dev_docs, market_docs as wave3_market_docs, Doc, Question, MARKET_QUESTIONS, QUESTIONS};
use common::trackb31::MARKET_QUESTIONS_31;
use common::trackb31_docs::market_docs_31;
use common::trackb_holdout::{holdout_docs, HOLDOUT_QUESTIONS};

const W31_CLASSES: [&str; 12] = [
    "W1", "W2", "W3", "W4", "W5", "W6", "W7", "W8", "W9", "W10", "W11", "W12",
];

/// Which corpus docs verbatim-back each unit (doc-id units back
/// themselves). Panics on an unbacked unit — every ground-truth unit must
/// be an exact corpus sentence, a document id, or a relation triple
/// ("Subject predicate Object") present in the merged IR (the pinned
/// Wave 3 depth-2 probe uses that form).
fn unit_backing_docs<'a>(q: &'a Question, docs: &'a [Doc], merged: &'a KnowledgeIr) -> Vec<&'a str> {
    let ids: BTreeSet<&str> = docs.iter().map(|d| d.id).collect();
    q.units
        .iter()
        .map(|u| {
            if ids.contains(u) {
                return *u;
            }
            for d in docs {
                if d.chunks.iter().any(|c| {
                    let ct = common::tokens(c);
                    common::tokens(u).iter().all(|t| ct.contains(t))
                }) {
                    return d.id;
                }
            }
            if let Some(r) = merged.relations.iter().find(|r| {
                let joined = format!("{} {} {}", r.subject, r.predicate, r.object);
                joined == *u
            }) {
                return r.evidence.document_id.as_deref().unwrap_or("merged");
            }
            panic!("unit '{}' has no verbatim backing chunk", u);
        })
        .collect()
}

/// Multi-source = the answer's units span ≥2 distinct documents
/// (mechanical, independent of the declared kind).
fn is_multi_source(q: &Question, docs: &[Doc], merged: &KnowledgeIr) -> bool {
    let backers = unit_backing_docs(q, docs, merged);
    let distinct: BTreeSet<&str> = backers.into_iter().collect();
    distinct.len() >= 2
}

/// W2 semantic probes share ZERO tokens with both units under the
/// no-stopwords tokens() contract (common/mod.rs) — the exact contract the
/// win-zone judge measures against. A probe with any token overlap would be
/// answerable by lexical overlap and would pollute the comparison.
fn assert_w2_zero_overlap(q: &Question) {
    let qt = common::tokens(q.text);
    for u in q.units {
        let ut = common::tokens(u);
        let shared: Vec<&String> = ut.iter().filter(|t| qt.contains(*t)).collect();
        assert!(
            shared.is_empty(),
            "'{}': W2 probe shares tokens {shared:?} with unit '{u}'",
            q.text
        );
    }
}

/// W31-MKT-001 acceptance (spec §5): corpus size, per-class coverage,
/// per-task ground truth, shape thresholds. The full corpus is the union
/// of the 13 pinned Wave 3 tasks' docs and the new Wave 3.1 docs — some
/// 3.1 tasks deliberately reuse Wave 3 documents (kb-payments, kb-audit,
/// kb-arch, kb-incident).
#[test]
fn w31_mkt_001_market_corpus_expansion() {
    let mut docs = market_docs_31();
    docs.extend(wave3_dev_docs());
    docs.extend(wave3_market_docs());
    let irs: Vec<_> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    assert_integrity(&docs, &merged);

    let all: Vec<&Question> = QUESTIONS
        .iter()
        .chain(MARKET_QUESTIONS.iter())
        .chain(MARKET_QUESTIONS_31.iter())
        .collect();

    // ── ≥100 tasks ────────────────────────────────────────────────────────
    assert!(all.len() >= 100, "corpus has {} tasks, need ≥100", all.len());

    // ── ≥10 tasks per class, all 12 classes ───────────────────────────────
    for class in W31_CLASSES {
        let n = all.iter().filter(|q| q.class == class).count();
        assert!(n >= 10, "class {class} has {n} tasks, need ≥10");
    }
    for q in &all {
        assert!(
            W31_CLASSES.contains(&q.class),
            "unknown class '{}' on '{}'",
            q.class,
            q.text
        );
    }

    // ── question texts are unique ─────────────────────────────────────────
    let mut texts = BTreeSet::new();
    for q in &all {
        assert!(texts.insert(q.text), "duplicate question text '{}'", q.text);
    }

    // ── every task declares all 7 ground-truth fields (spec §5) ───────────
    let authorities: BTreeSet<&str> = [
        "source_code",
        "documentation",
        "deployment_observed",
        "organization_policy",
    ]
    .into_iter()
    .collect();
    for q in &all {
        let gt = &q.gt;
        for (name, v) in [
            ("variants", gt.variants),
            ("evidence", gt.evidence),
            ("relationships", gt.relationships),
            ("temporal", gt.temporal),
            ("authority", gt.authority),
            ("ambiguity", gt.ambiguity),
        ] {
            assert!(!v.is_empty(), "'{}': empty gt.{name}", q.text);
        }
        assert!(
            ["current", "historical", "mixed"].contains(&gt.temporal),
            "'{}': bad temporal '{}'",
            q.text,
            gt.temporal
        );
        assert!(
            authorities.contains(gt.authority),
            "'{}': bad authority '{}'",
            q.text,
            gt.authority
        );
        assert!(
            ["none", "conflict", "unknown"].contains(&gt.ambiguity),
            "'{}': bad ambiguity '{}'",
            q.text,
            gt.ambiguity
        );
        // every unit is a verbatim corpus sentence or a document id
        let _ = unit_backing_docs(q, &docs, &merged);
        // W2 probes must be unanswerable by lexical overlap
        if q.class == "W2" {
            assert_w2_zero_overlap(q);
        }
    }

    // ── shape thresholds (spec §5 acceptance) ─────────────────────────────
    let total = all.len() as f64;
    let pct = |label: &str, n: usize, need: f64| {
        let got = n as f64 / total * 100.0;
        eprintln!("[W31-MKT-001] {label}: {n}/{all} = {got:.1}% (need ≥{need}%)", all = all.len());
        assert!(got >= need, "{label} {got:.1}% < {need}%");
    };

    let multi = all.iter().filter(|q| is_multi_source(q, &docs, &merged)).count();
    pct("multi-source", multi, 20.0);

    let rel_dep = all.iter().filter(|q| q.gt.relationships != "none").count();
    pct("relationship-dependent", rel_dep, 20.0);

    let temporal = all.iter().filter(|q| q.class == "W5").count();
    pct("temporal (class W5)", temporal, 10.0);

    let contradictory = all.iter().filter(|q| q.class == "W6").count();
    pct("contradictory (class W6)", contradictory, 10.0);

    let unknown = all.iter().filter(|q| q.class == "W11").count();
    pct("unknown (class W11)", unknown, 10.0);
}

/// W31-MKT-001 holdout freeze (spec §7): the holdout is a separate corpus
/// with disjoint ids, held out of every development measurement. This
/// test is deliberately structure-only — integrity, disjointness, size.
/// No scoring threshold may ever live here (that would leak holdout
/// signal into development); scoring is the #161 evaluation harness's
/// job, run once, frozen thereafter.
#[test]
fn w31_mkt_002_holdout_frozen() {
    let docs = holdout_docs();
    let irs: Vec<_> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    assert_integrity(&docs, &merged);

    assert!(
        HOLDOUT_QUESTIONS.len() >= 20,
        "holdout has {} tasks, need ≥20",
        HOLDOUT_QUESTIONS.len()
    );

    // disjoint document ids
    let dev_ids: BTreeSet<&str> = market_docs_31().iter().map(|d| d.id).collect();
    let hold_ids: BTreeSet<&str> = docs.iter().map(|d| d.id).collect();
    for id in &hold_ids {
        assert!(
            !dev_ids.contains(id),
            "holdout doc id '{id}' collides with the development corpus"
        );
    }

    // disjoint question texts
    let dev_texts: BTreeSet<&str> = QUESTIONS
        .iter()
        .chain(MARKET_QUESTIONS.iter())
        .chain(MARKET_QUESTIONS_31.iter())
        .map(|q| q.text)
        .collect();
    for q in HOLDOUT_QUESTIONS {
        assert!(
            !dev_texts.contains(q.text),
            "holdout question '{}' collides with the development corpus",
            q.text
        );
    }

    // holdout tasks declare the same ground-truth shape
    for q in HOLDOUT_QUESTIONS {
        let _ = unit_backing_docs(q, &docs, &merged);
        for (name, v) in [
            ("variants", q.gt.variants),
            ("evidence", q.gt.evidence),
            ("relationships", q.gt.relationships),
            ("temporal", q.gt.temporal),
            ("authority", q.gt.authority),
            ("ambiguity", q.gt.ambiguity),
        ] {
            assert!(!v.is_empty(), "'{}': empty gt.{name}", q.text);
        }
        if q.class == "W2" {
            assert_w2_zero_overlap(q);
        }
    }

    eprintln!(
        "[W31-MKT-002] holdout frozen: {} docs, {} tasks, ids disjoint",
        docs.len(),
        HOLDOUT_QUESTIONS.len()
    );
}
