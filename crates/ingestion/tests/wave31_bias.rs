//! Wave 3.1 (MVP-QA-003A) — W31-BIAS-001 benchmark bias audit.
//!
//! The spec asks, for every headline result: *could this test have been
//! deliberately constructed to make AIKOQL win?* The judgment half of
//! that audit lives in docs/benchmarks/bias-audit.md; this test pins the
//! checkable structural laws the audit's verdict rests on. Laws are
//! declared BEFORE measurement — a violation here is a real finding, not
//! a threshold to tune.
//!
//! Pinned laws:
//! 1. No answer unit's token set is fully contained in its own
//!    question's token set — a question that contains its own answer
//!    would make every treatment trivially correct (the worst
//!    construction leak).
//! 2. No question text names a corpus doc id — naming the source would
//!    hand the treatments the answer's location.
//! 3. Union (development) and holdout doc ids are disjoint — the
//!    holdout must be unseen surface, not re-labeled dev data.
//! 4. The counter-measures exist: the W1 lookup control class (the
//!    rig check — a trivial class nobody may win) and the NEG-001
//!    mandated falsification scenarios (the spec's own adversarial
//!    set) are present and loadable.
//! 5. ≥100 union tasks (the MKT-001 scale law, re-pinned).

mod common;

use std::collections::HashSet;

use common::trackb::Question;
use common::trackb_holdout::{holdout_docs, HOLDOUT_QUESTIONS};
use common::wave31_sim::{union_docs, union_questions};

/// (3) — dev and holdout surfaces must not share a document.
#[test]
fn w31_bias_001_benchmark_bias_audit() {
    let dev_docs = union_docs();
    let dev_ids: HashSet<&str> = dev_docs.iter().map(|d| d.id).collect();
    let hold_ids: HashSet<&str> = holdout_docs().iter().map(|d| d.id).collect();
    let shared: Vec<&&str> = dev_ids.intersection(&hold_ids).collect();
    assert!(
        shared.is_empty(),
        "dev/holdout doc-id overlap — holdout is re-labeled dev data: {shared:?}"
    );

    let dev_qs = union_questions();
    assert!(dev_qs.len() >= 100, "union corpus has {} tasks, need ≥100", dev_qs.len());

    // (1) + (2) — per-question leakage laws, over union AND holdout tasks.
    let hold_qs: Vec<&Question> = HOLDOUT_QUESTIONS.iter().collect();
    for (set, qs) in [("union", dev_qs.as_slice()), ("holdout", hold_qs.as_slice())] {
        for q in qs {
            let q_toks = common::tokens(q.text);
            for unit in q.units {
                let u_toks = common::tokens(unit);
                assert!(
                    !u_toks.is_subset(&q_toks),
                    "{set} Q '{text}' unit '{unit}' is fully contained in its own \
                     question — every treatment is trivially correct",
                    text = q.text
                );
            }
            for id in dev_ids.iter().chain(hold_ids.iter()) {
                assert!(
                    !q.text.to_lowercase().contains(&id.to_lowercase()),
                    "{set} Q '{text}' names doc '{id}' — the answer's source is leaked \
                     to the treatments",
                    text = q.text
                );
            }
        }
    }

    // (4a) — the W1 control class must exist in the union task set (the
    // rig check: a class where nobody may claim an advantage).
    assert!(
        dev_qs.iter().any(|q| q.class == "W1"),
        "union task set has no W1 control class — the rig check is missing"
    );

    // (4b) — the NEG-001 mandated falsification scenarios exist and are
    // the ones the spec lists (source-level, same pattern as the
    // SCALE-001 R14 pointer — the file must carry the mandated four).
    let neg_src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wave31_neg.rs"),
    )
    .expect("wave31_neg.rs must exist");
    for key in ["exact-lookup", "doc-qa", "small-corpus", "single-source"] {
        assert!(
            neg_src.contains(key),
            "wave31_neg.rs lost the mandated '{key}' scenario — the falsification \
             surface shrank"
        );
    }

    println!(
        "\n[W31-BIAS-001] structural laws held: {} union + {} holdout tasks, \
         zero unit-in-question leaks, zero doc-id leaks, dev/holdout doc sets \
         disjoint, W1 control present, NEG-001 four scenarios present",
        dev_qs.len(),
        HOLDOUT_QUESTIONS.len()
    );
}

/// The overlap ceiling reported (not thresholded — the law above only
/// forbids *full* containment): how lexically close questions get to
/// their own answers, for the audit doc's honesty column.
#[test]
fn w31_bias_002_question_answer_overlap_report() {
    let mut max_frac: f64 = 0.0;
    let mut max_case = "";
    for q in union_questions() {
        let q_toks = common::tokens(q.text);
        for unit in q.units {
            let u_toks = common::tokens(unit);
            let frac = u_toks.iter().filter(|t| q_toks.contains(*t)).count() as f64
                / u_toks.len().max(1) as f64;
            if frac > max_frac {
                max_frac = frac;
                max_case = q.text;
            }
        }
    }
    println!(
        "\n[W31-BIAS-001] max question↔answer token overlap across union tasks: \
         {:.2} (case: '{}')",
        max_frac, max_case
    );
}
