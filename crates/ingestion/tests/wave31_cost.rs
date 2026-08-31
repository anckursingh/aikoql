//! Wave 3.1 (MVP-QA-003A) — W31-COST-001 cost per successful task.
//!
//! The spec's five-term cost
//!
//!     infrastructure + LLM + embedding + retrieval + agent/tool calls
//!     ──────────────────────────────────────────────────────────────────
//!                        successful tasks
//!
//! for RAG / Graph-RAG / AIKOQL over the declared workload scope: the
//! 148-task union corpus, all 12 workload classes (W1..W12). Success =
//! win-zone 2 (the frozen judge, unknown-probe inversion — the COMP-001
//! contract).
//!
//! Declared rates (none is measured; each is a named convention so the
//! claim's scope is explicit):
//! - LLM: the frozen G11/G12 convention (wave31_sim::cost) — input
//!   $0.15/M, output $0.60/M at 100 answer tokens per answered query.
//!   Refusal rows deliver no context and generate no answer → $0.
//! - embedding: $0.02/M corpus tokens, one embedding pass per run for
//!   the treatments that embed (rag, graph-rag); aikoql indexes
//!   deterministically → $0.
//! - infrastructure: $100 per component per 100k tasks — rag 3
//!   (embedder, vector store, retriever), graph-rag 4 (+ graph store),
//!   aikoql 1 (the kernel).
//! - retrieval: $0.0005 per query on the vector-store treatments
//!   (every task issues one); aikoql's compile is in-process → $0 (its
//!   infra term carries the compute — that is the product claim).
//! - agent/tool calls: $0 all three — the mechanical slice has no
//!   metered tool-call surface (honest row; tool_calls = 1 in-process
//!   call per task, not an API charge).
//!
//! Acceptance (spec): the universal "cheaper" claim is allowed ONLY if
//! AIKOQL wins cost/success in all 12 declared classes. The test
//! computes the verdict from the table (strictly cheaper than both
//! baselines, per class) and asserts the computation's internal
//! consistency — the verdict itself is printed, never rigged.

mod common;

use std::collections::BTreeMap;

use aikoql_ingestion::{
    compile_context, merge_knowledge_ir, render_context_markdown, KnowledgeIr,
    MockEmbeddingProvider,
};
use common::trackb::{corpus, Question};
use common::wave31_sim::{
    cost as llm_cost, entity_chunk_index, graph_expand, pack_budgeted, rank_positions, union_docs,
    union_questions, win_zone,
};

const BUDGET: usize = 300;
const EMBED_PRICE_PER_M: f32 = 0.02;
const INFRA_PER_COMPONENT_PER_100K: f32 = 100.0;
const RETRIEVAL_PER_QUERY: f32 = 0.0005;
/// Components per treatment — indexed like `measure`'s return order
/// (aikoql, graph-rag, rag), declared above.
const COMPONENTS: [usize; 3] = [1, 4, 3];
/// Treatments that run an embedding pass over the corpus (same order).
const EMBEDS: [bool; 3] = [false, true, true];
/// Treatments that pay per-query retrieval (same order).
const RETRIEVES: [bool; 3] = [false, true, true];

/// One task's three payloads (aikoql, graph-rag, rag) — the COMP-001
/// treatment construction.
fn measure(
    q: &Question,
    corpus: &[common::CorpusChunk],
    index: &[(String, Vec<usize>)],
    merged: &KnowledgeIr,
    provider: &MockEmbeddingProvider,
) -> [String; 3] {
    let aikoql = render_context_markdown(&compile_context(q.text, merged, BUDGET));
    let graph = pack_budgeted(
        &graph_expand(&rank_positions(corpus, q.text, provider), corpus, index),
        corpus,
    );
    let rag = pack_budgeted(&rank_positions(corpus, q.text, provider), corpus);
    [aikoql, graph, rag]
}

#[derive(Default, Clone, Copy)]
struct Roll {
    tasks: usize,
    success: usize,
    tokens: usize,
    answered: usize,
}

/// The five-term cost for one roll. `embed_share` is this roll's slice of
/// the treatment's one embedding pass (class share = embed_total ×
/// class_tokens/total_tokens; the totals roll passes embed_total).
fn roll_cost(r: &Roll, embed_share: f32, t: usize) -> f32 {
    let llm = llm_cost(r.tokens, r.answered);
    let infra = COMPONENTS[t] as f32 * INFRA_PER_COMPONENT_PER_100K / 100_000.0 * r.tasks as f32;
    let retrieval = if RETRIEVES[t] {
        RETRIEVAL_PER_QUERY * r.tasks as f32
    } else {
        0.0
    };
    let embed = if EMBEDS[t] { embed_share } else { 0.0 };
    // agent/tool calls: $0 (declared above).
    llm + infra + retrieval + embed
}

#[test]
fn w31_cost_001_cost_per_successful_task() {
    let docs = union_docs();
    let questions = union_questions();
    let provider = MockEmbeddingProvider::new();
    let corpus = corpus(&docs);
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    let index = entity_chunk_index(&merged, &corpus);
    let corpus_tokens: usize = corpus.iter().map(|(_, _, t)| t.len() / 4).sum();
    let embed_total = corpus_tokens as f32 / 1e6 * EMBED_PRICE_PER_M;

    let mut classes: BTreeMap<&'static str, [Roll; 3]> = BTreeMap::new();
    for q in &questions {
        let payloads = measure(q, &corpus, &index, &merged, &provider);
        let c = classes.entry(q.class).or_default();
        for (t, p) in payloads.iter().enumerate() {
            c[t].tasks += 1;
            c[t].tokens += p.len() / 4;
            if !p.trim().is_empty() {
                c[t].answered += 1;
            }
            if win_zone(p, q) == 2 {
                c[t].success += 1;
            }
        }
    }

    let mut totals = [Roll::default(); 3];
    for c in classes.values() {
        for t in 0..3 {
            totals[t].tasks += c[t].tasks;
            totals[t].success += c[t].success;
            totals[t].tokens += c[t].tokens;
            totals[t].answered += c[t].answered;
        }
    }
    // The declared scope: all 12 workload classes must be present, or the
    // "across the declared workload scope" verdict is being computed on a
    // partial corpus.
    assert_eq!(
        classes.len(),
        12,
        "the declared workload scope must be the 12 classes, got {}",
        classes.len()
    );

    // ── per-class cost/success table + the computed verdict ─────────────
    let mut wins = 0usize;
    let mut class_cost_sum = [0.0f32; 3];
    eprintln!("[W31-COST-001] per-class cost per successful task ($):");
    for (class, c) in &classes {
        let costs: [f32; 3] = std::array::from_fn(|t| {
            let share = if EMBEDS[t] && totals[t].tokens > 0 {
                embed_total * c[t].tokens as f32 / totals[t].tokens as f32
            } else {
                0.0
            };
            roll_cost(&c[t], share, t)
        });
        for t in 0..3 {
            class_cost_sum[t] += costs[t];
        }
        let cps = |t: usize| -> Option<f32> {
            (c[t].success > 0).then(|| costs[t] / c[t].success as f32)
        };
        let (a, g, r) = (cps(0), cps(1), cps(2));
        let won = match (a, g, r) {
            (Some(a), Some(g), Some(r)) => a < g && a < r,
            _ => false,
        };
        wins += won as usize;
        let fmt = |v: Option<f32>| match v {
            Some(v) => format!("{v:.5}"),
            None => "n/a".into(),
        };
        eprintln!(
            "  {class}: aikoql {} ({} /{} tasks) graph-rag {} ({}/{}) rag {} ({}/{}) {}",
            fmt(a),
            c[0].success,
            c[0].tasks,
            fmt(g),
            c[1].success,
            c[1].tasks,
            fmt(r),
            c[2].success,
            c[2].tasks,
            if won { "— aikoql cheaper" } else { "" },
        );
    }
    let claim_allowed = wins == 12;
    eprintln!(
        "[W31-COST-001] verdict: aikoql cheaper in {wins}/12 declared classes — \
         universal 'cheaper' claim allowed: {claim_allowed} (acceptance: only if 12/12)"
    );

    // ── the spec's report block, per treatment ──────────────────────────
    for (t, name) in ["aikoql", "graph-rag", "rag"].iter().enumerate() {
        let roll = &totals[t];
        let cost = roll_cost(roll, embed_total, t);
        let cps = if roll.success > 0 {
            format!("${:.5}", cost / roll.success as f32)
        } else {
            "n/a".into()
        };
        let failure_rate = (roll.tasks - roll.success) as f32 / roll.tasks.max(1) as f32 * 100.0;
        let tokens_per_success = if roll.success > 0 {
            roll.tokens as f32 / roll.success as f32
        } else {
            0.0
        };
        eprintln!(
            "[W31-COST-001] {name}: cost ${cost:.5} = llm ${llm:.5} + embed ${embed:.5} \
             + infra ${infra:.5} + retrieval ${retr:.5} + agent/tool $0",
            llm = llm_cost(roll.tokens, roll.answered),
            embed = if EMBEDS[t] { embed_total } else { 0.0 },
            infra =
                COMPONENTS[t] as f32 * INFRA_PER_COMPONENT_PER_100K / 100_000.0 * roll.tasks as f32,
            retr = if RETRIEVES[t] {
                RETRIEVAL_PER_QUERY * roll.tasks as f32
            } else {
                0.0
            },
        );
        eprintln!(
            "[W31-COST-001] {name}: successes {}/{} cost/success {cps} failure rate \
             {failure_rate:.1}% tokens/success {tokens_per_success:.0}",
            roll.success, roll.tasks,
        );
    }

    // ── internal consistency of the report (the numbers cannot drift) ──
    for t in 0..3 {
        let total = roll_cost(&totals[t], embed_total, t);
        assert!(
            (total - class_cost_sum[t]).abs() < 0.01,
            "total cost must equal the class-cost sum: {total} vs {}",
            class_cost_sum[t]
        );
    }
    eprintln!(
        "[W31-COST-001] consistency: total == Σ per-class per treatment \
         (embedding amortized by token share); verdict computed from the table, never rigged"
    );
}
