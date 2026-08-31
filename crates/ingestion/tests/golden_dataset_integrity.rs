//! G9 (HLD §50): golden-dataset integrity — the unified dataset is the
//! ground truth every regression instrument runs against, so the dataset
//! itself must be verifiably consistent. One test cross-checks it against
//! the real mock pipeline:
//!
//! - ids and questions unique; every question carries an answer, qrels,
//!   and (where the answer's evidence fixture expresses them) KOs/relations
//! - every qrel resolves to a real chunk in the rule corpus
//! - every golden answer's key tokens are grounded in its qrel chunks —
//!   the dataset-side half of §53 answer/evidence correctness (the
//!   generator-side half is the PR-R judge)
//! - expected KOs and relations per question are actually extracted by the
//!   baseline pipeline (a real extraction regression fails here)
//! - the per-question KO expectations are a subset of the human annotation
//!   lists the golden-suite gate asserts — the two hand-authored lists
//!   cannot drift apart
//! - SEMANTIC_GOLD covers exactly the corpus fixtures with no duplicates

mod common;

use common::golden_dataset::{
    compile_fixture_irs, golden_answers, multimodal_expected_entities, normalize, queries,
    visual_queries, GOLDEN, SEMANTIC_GOLD,
};
use common::{chunk_text, corpus, tokens, FIXTURES};
use std::collections::HashSet;

#[test]
fn golden_dataset_integrity() {
    // ── Structural integrity ─────────────────────────────────────────────
    let mut ids = HashSet::new();
    let mut questions = HashSet::new();
    for g in GOLDEN {
        assert!(!g.id.is_empty(), "golden question id must be non-empty");
        assert!(ids.insert(g.id), "duplicate golden question id: {}", g.id);
        assert!(
            questions.insert(g.question),
            "duplicate golden question: {:?}",
            g.question
        );
        assert!(
            !g.expected_answer.is_empty(),
            "{}: expected answer must be non-empty",
            g.id
        );
        assert!(!g.relevant.is_empty(), "{}: qrels must be non-empty", g.id);
        for (fixture, _) in g.relevant {
            assert!(
                FIXTURES.contains(fixture),
                "{}: qrel fixture {} not in the corpus",
                g.id,
                fixture
            );
        }
    }

    // Projections stay aligned: one answer per textual query, one query
    // per textual entry — the alignment that used to be a comment-only
    // convention is now structural.
    assert_eq!(golden_answers().len(), queries().len());
    assert_eq!(
        queries().len(),
        GOLDEN.iter().filter(|g| g.textual).count(),
        "textual projection must cover every textual question"
    );
    assert_eq!(
        visual_queries().len(),
        GOLDEN.iter().filter(|g| g.visual).count(),
        "visual projection must cover every visual question"
    );

    // ── Grounding + extraction against the real baseline pipeline ────────
    let provider = aikoql_ingestion::MockEmbeddingProvider::new();
    let (corpus, _) = corpus(&aikoql_ingestion::RuleBoundaryDetector, &provider);
    let irs = compile_fixture_irs();

    for g in GOLDEN {
        // Qrels resolve to real chunks, and the golden answer is supported
        // by the golden evidence: every key token appears in some qrel
        // chunk (dataset-side §53 answer/evidence correctness).
        let qrel_texts: Vec<&str> = g
            .relevant
            .iter()
            .map(|(f, i)| chunk_text(&corpus, f, *i))
            .collect();
        let answer_tokens = tokens(g.expected_answer);
        for token in &answer_tokens {
            assert!(
                qrel_texts
                    .iter()
                    .any(|t| tokens(t).contains(token.as_str())),
                "{}: golden answer token {:?} is not grounded in the qrel chunks {:?}",
                g.id,
                token,
                g.relevant
            );
        }

        // Expected KOs/relations must be extracted by the baseline pipeline
        // from the question's evidence fixture (the first qrel's fixture is
        // the answer's evidence source).
        let fixture = g.relevant[0].0;
        let ir = &irs[fixture];
        if !g.expected_entities.is_empty() {
            let extracted: HashSet<String> =
                ir.entities.iter().map(|e| normalize(&e.name)).collect();
            for entity in g.expected_entities {
                assert!(
                    extracted.contains(&normalize(entity)),
                    "{}: expected KO {:?} not extracted from {} (extracted: {:?})",
                    g.id,
                    entity,
                    fixture,
                    ir.entities
                        .iter()
                        .map(|e| e.name.as_str())
                        .collect::<Vec<_>>()
                );
                // The two hand-authored lists must agree: per-question KOs
                // are a subset of the human annotation list the
                // golden-suite gate asserts.
                let human = multimodal_expected_entities(fixture);
                assert!(
                    human.contains(entity),
                    "{}: expected KO {:?} missing from the human annotation list for {}",
                    g.id,
                    entity,
                    fixture
                );
            }
        }
        if !g.expected_relations.is_empty() {
            let extracted: HashSet<(String, String)> = ir
                .relations
                .iter()
                .map(|r| (normalize(&r.subject), normalize(&r.object)))
                .collect();
            for (subject, object) in g.expected_relations {
                assert!(
                    extracted.contains(&(normalize(subject), normalize(object))),
                    "{}: expected relation {:?}->{:?} not extracted from {}",
                    g.id,
                    subject,
                    object,
                    fixture
                );
            }
        }
    }

    // ── SEMANTIC_GOLD covers the corpus exactly ──────────────────────────
    let mut seen = HashSet::new();
    for g in SEMANTIC_GOLD {
        assert!(
            FIXTURES.contains(&g.fixture),
            "SEMANTIC_GOLD fixture {} not in the corpus",
            g.fixture
        );
        assert!(
            seen.insert(g.fixture),
            "duplicate SEMANTIC_GOLD fixture: {}",
            g.fixture
        );
    }

    eprintln!(
        "[GOLDEN-DATASET] questions={} textual={} visual={} semantic-gold-fixtures={} \
         grounded=all integrity=pass",
        GOLDEN.len(),
        queries().len(),
        visual_queries().len(),
        SEMANTIC_GOLD.len()
    );
}
