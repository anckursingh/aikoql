//! Wave 3.1 (MVP-QA-003A) — W31-REPRO-001 clean-environment reproduction.
//!
//! A separate test binary (separate compile + execution — the closest
//! in-repo analogue of an independent run) re-derives the headline
//! result over the frozen corpus/task set, executing the SAME
//! measurement code as COMP-001 (`common/wave31_sim::measure_task`).
//! A reproduction that re-implements the measurement can drift from
//! what it claims to reproduce; sharing the function rules that out.
//!
//! Frozen inputs (dataset version = this repository's git HEAD — the
//! corpus is versioned in-repo): union docs + questions (trackb +
//! trackb31 + market_docs_31), 300-token budget, the `units_hit` judge
//! with unknown-probe inversion, deterministic MockEmbeddingProvider.
//! No LLM, no clock in the judge. Model row: none (the mechanical
//! slice); the real-model leg is REAL-001's gated harness.
//!
//! Pinned acceptance (spec REPRO-001: "an independent execution must
//! reproduce the direction and conclusion of the headline result"),
//! declared before measurement:
//! - direction:  aikoql total units ≥ rag AND ≥ graph-rag;
//! - conclusion: ≥1 Strong Fit class AND W1 control at full parity AND
//!   worst class regression ≤ 2 units (COMP-001's acceptance verdict);
//! - determinism: two full passes produce identical per-task rows on
//!   every mechanical column (units, tokens, grounding, cost).
//!   Latency is wall-clock on a debug build and is explicitly NOT part
//!   of the reproduction claim — it varies with machine load.

mod common;

use std::collections::{BTreeMap, HashSet};

use aikoql_ingestion::{merge_knowledge_ir, MockEmbeddingProvider};
use common::trackb::{assert_integrity, corpus};
use common::wave31_sim::{
    cost, entity_chunk_index, measure_task, union_docs, union_questions, TaskRow,
};

/// The mechanical columns compared for determinism (the µs columns are
/// excluded — wall-clock on a debug build varies run to run).
fn mech(row: &TaskRow) -> (usize, usize, usize, usize, usize, usize, bool, bool, bool) {
    (
        row.a, row.g, row.r, row.at, row.gt, row.rt, row.ag, row.gg, row.rg,
    )
}

/// One full measurement pass over the frozen union corpus.
fn run_pass() -> Vec<TaskRow> {
    let docs = union_docs();
    let questions = union_questions();
    let provider = MockEmbeddingProvider::new();
    let corpus = corpus(&docs);
    let irs: Vec<_> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    assert_integrity(&docs, &merged);
    let index = entity_chunk_index(&merged, &corpus);
    let doc_ids: HashSet<&str> = docs
        .iter()
        .map(|d| d.id.split('-').next().unwrap_or(d.id))
        .collect();
    questions
        .iter()
        .map(|q| measure_task(q, &corpus, &index, &merged, &provider, &doc_ids))
        .collect()
}

#[test]
fn w31_repro_001_clean_environment_reproduction() {
    // Two full passes — the determinism leg.
    let pass1 = run_pass();
    let pass2 = run_pass();
    assert!(
        pass1.len() >= 100,
        "frozen corpus has {} tasks, need ≥100",
        pass1.len()
    );
    for (i, (r1, r2)) in pass1.iter().zip(&pass2).enumerate() {
        assert_eq!(
            mech(r1),
            mech(r2),
            "pass divergence at task {i}: mechanical columns must be identical"
        );
    }

    // Direction + conclusion rollup (COMP-001's acceptance verdict,
    // re-derived from the first pass).
    let questions = union_questions();
    let mut class: BTreeMap<&'static str, (usize, usize, usize)> = BTreeMap::new();
    let (mut ta, mut tg, mut tr) = (0usize, 0usize, 0usize);
    let (mut toka, mut tokg, mut tokr) = (0usize, 0usize, 0usize);
    for (q, row) in questions.iter().zip(&pass1) {
        let c = class.entry(q.class).or_default();
        c.0 += row.a;
        c.1 += row.r;
        c.2 += 2;
        ta += row.a;
        tg += row.g;
        tr += row.r;
        toka += row.at;
        tokg += row.gt;
        tokr += row.rt;
    }

    // Direction: the headline result's direction must hold.
    assert!(
        ta >= tr && ta >= tg,
        "headline direction broken: aikoql {ta} vs rag {tr} / graph-rag {tg}"
    );

    // Conclusion: the COMP-001 acceptance verdict, re-derived.
    let strong = class
        .values()
        .filter(|(a, r, m)| a > r && *a as f64 / *m as f64 >= 0.75)
        .count();
    assert!(strong >= 1, "no Strong Fit class — COMP-001 conclusion not reproduced");
    let w1 = class.get("W1").copied().unwrap_or_default();
    assert_eq!(
        (w1.0, w1.1),
        (w1.2, w1.2),
        "W1 control parity broken: aikoql {}/{} rag {}/{}",
        w1.0, w1.2, w1.1, w1.2
    );
    let worst = class
        .values()
        .map(|(a, r, _)| *r as isize - *a as isize)
        .max()
        .unwrap_or(0);
    assert!(
        worst <= 2,
        "worst class regression {worst} exceeds COMP-001 bound 2"
    );

    println!(
        "\n[W31-REPRO-001] reproduction summary ({} tasks, pass1):\n  \
         aikoql {ta}/{max} units, {toka} tok, cost ${:.4}\n  \
         graphrag {tg}/{max} units, {tokg} tok, cost ${:.4}\n  \
         rag {tr}/{max} units, {tokr} tok, cost ${:.4}\n  \
         strong_fit={strong} worst_regression={worst} control_parity=true \
         determinism=mechanical-columns-identical-across-2-passes\n  \
         frozen inputs: union corpus at git HEAD, budget 300, units_hit \
         judge, mock embeddings, no LLM (model row: none)",
        questions.len(),
        cost(toka, questions.len()),
        cost(tokg, questions.len()),
        cost(tokr, questions.len()),
        max = questions.len() * 2,
    );
}
