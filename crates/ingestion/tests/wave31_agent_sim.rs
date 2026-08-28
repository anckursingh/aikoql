//! Wave 3.1 (MVP-QA-003A) — W31-REAL-001: the deterministic agent-chain
//! sim. Full chain: user task → agent policy → AIKOQL context → answer /
//! refusal, over the 50-task sample × 5 repetitions, against the
//! conventional-RAG agent as the comparison leg.
//!
//! Predefined acceptance (spec REAL-001, declared before first
//! measurement):
//! 1. No Sev-1 behavior / 2. no unauthorized action — the policy has no
//!    Act arm (structural), and all 5 action-request probes must be
//!    Refused in every rep by both treatments.
//! 3. Unknown tasks produce no unsupported authoritative answers —
//!    answers are payload echoes (unsupported tokens asserted 0), and a
//!    pack that rode in on semantic fallback (§36) or is empty is
//!    Refused, never answered. The measured W11 false-confidence column
//!    (healthy packs on unknown-probe tasks) is printed here and
//!    formally measured by UNK-001 (#164).
//! 4. Repeatable advantage — the predefined class W7 (provenance, the
//!    structural win) shows AIKOQL > RAG in every rep. The mechanical
//!    sim is deterministic, so the 5 reps are identical by construction
//!    (the repetition exists for the gated real-LLM leg, where
//!    generation varies); cross-rep equality is asserted as the
//!    repeatability property of the mechanical slice.
//!
//! Measurements per treatment: task success (win-zone, W11 inversion),
//! groundedness, unsupported tokens, tool calls, retrieval retries,
//! tokens, latency, cost (G11 rates). Retrieval retries are structurally
//! 0 in the mechanical slice (compile_context is deterministic — a
//! re-query returns the same package), and SemanticFallback cannot arise
//! from the aikoql leg's plain compile_context (it needs semantic
//! scores); the live refusal boundary here is the empty pack. Both the
//! retry surface (generation retries) and the fallback boundary are
//! exercised for real in the gated LLM leg / embedder deployments.

mod common;

use aikoql_ingestion::{merge_knowledge_ir, KnowledgeIr, MockEmbeddingProvider, RetrievalStatus};
use common::trackb::{assert_integrity, corpus, Doc, Question, MARKET_QUESTIONS, QUESTIONS};
use common::trackb31::MARKET_QUESTIONS_31;
use common::trackb31_docs::market_docs_31;
use common::wave31_sim::{
    action_requests, agent_policy, aikoql_context, cost, rag_context, sample_tasks,
    unsupported_tokens, win_zone, AgentOutcome, SimContext, REPS,
};

fn union_docs() -> Vec<Doc> {
    let mut docs = market_docs_31();
    docs.extend(common::trackb::docs());
    docs.extend(common::trackb::market_docs());
    docs
}

fn union_questions() -> Vec<&'static Question> {
    QUESTIONS
        .iter()
        .chain(MARKET_QUESTIONS.iter())
        .chain(MARKET_QUESTIONS_31.iter())
        .collect()
}

#[derive(Default)]
struct Treatment {
    score: usize,
    answers: usize,
    refuses: usize,
    grounded: usize,
    tool_calls: usize,
    retries: usize,
    tokens: usize,
    micros: u128,
    /// W11 tasks answered with a healthy non-empty pack (false confidence
    /// — the UNK-001 column, printed not asserted here).
    w11_false_confidence: usize,
    w11_total: usize,
}

/// One task through one treatment: context → policy → judge → counters.
fn run_task(t: &mut Treatment, ctx: &SimContext, q: &Question, w7_score: &mut usize) {
    let outcome = agent_policy(q, ctx);
    let cites = common::tokens(&ctx.payload).contains("kb");
    match &outcome {
        AgentOutcome::Answer(answer) => {
            let s = win_zone(answer, q);
            if q.class == "W7" {
                *w7_score += s;
            }
            t.answers += 1;
            if cites {
                t.grounded += 1;
            }
            let ut = unsupported_tokens(answer, &ctx.payload);
            assert_eq!(
                ut, 0,
                "'{}': deterministic answer carries {ut} unsupported tokens",
                q.text
            );
            t.score += s;
        }
        AgentOutcome::Refuse(_) => {
            t.refuses += 1;
        }
    }
    t.tool_calls += ctx.tool_calls;
    t.retries += ctx.retries;
    t.tokens += ctx.payload.len() / 4;
    t.micros += ctx.micros;
    if q.class == "W11" {
        t.w11_total += 1;
        if ctx.status == RetrievalStatus::Healthy && !ctx.payload.trim().is_empty() {
            t.w11_false_confidence += 1;
        }
    }
}

#[test]
fn w31_real_001_deterministic_agent_sim() {
    let provider = MockEmbeddingProvider::new();
    let docs = union_docs();
    let corpus = corpus(&docs);
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    assert_integrity(&docs, &merged);
    let all = union_questions();
    let tasks = sample_tasks(&all);
    assert_eq!(tasks.len(), 50, "REAL-001 sample must be 50 tasks");

    let mut aikoql = Treatment::default();
    let mut rag = Treatment::default();
    let mut w7_per_rep: Vec<(usize, usize)> = Vec::new();

    for _rep in 0..REPS {
        let (mut a_w7, mut r_w7) = (0usize, 0usize);
        for q in &tasks {
            let actx = aikoql_context(q, &merged);
            run_task(&mut aikoql, &actx, q, &mut a_w7);
            let rctx = rag_context(q, &corpus, &provider);
            run_task(&mut rag, &rctx, q, &mut r_w7);
        }
        w7_per_rep.push((a_w7, r_w7));
    }

    // ── acceptance 1+2: action requests always refused ───────────────────
    for q in action_requests() {
        for _rep in 0..REPS {
            let ctx = aikoql_context(&q, &merged);
            assert!(
                matches!(agent_policy(&q, &ctx), AgentOutcome::Refuse(_)),
                "'{}': action request must be refused (aikoql)",
                q.text
            );
            let rctx = rag_context(&q, &corpus, &provider);
            assert!(
                matches!(agent_policy(&q, &rctx), AgentOutcome::Refuse(_)),
                "'{}': action request must be refused (rag)",
                q.text
            );
        }
    }
    assert_eq!(aikoql.answers + aikoql.refuses, tasks.len() * REPS);
    assert_eq!(rag.answers + rag.refuses, tasks.len() * REPS);

    // ── acceptance 4: repeatable W7 advantage, deterministic reps ────────
    for (rep, (a, r)) in w7_per_rep.iter().enumerate() {
        assert!(
            a > r,
            "W7 advantage lost in rep {rep}: aikoql {a} vs rag {r}"
        );
        if rep > 0 {
            assert_eq!(
                (*a, *r),
                w7_per_rep[0],
                "mechanical sim must be deterministic (rep {rep} ≠ rep 0)"
            );
        }
    }

    for (name, t) in [("aikoql", &aikoql), ("rag", &rag)] {
        eprintln!(
            "[W31-REAL-001] {name}: score {}/{} answers {} refuses {} grounded {}/{} \
             tool_calls {} retries {} tokens {} µs {} cost ${:.4} \
             W11 false-confidence {}/{}",
            t.score,
            tasks.len() * REPS * 2,
            t.answers,
            t.refuses,
            t.grounded,
            t.answers,
            t.tool_calls,
            t.retries,
            t.tokens,
            t.micros,
            cost(t.tokens, tasks.len() * REPS),
            t.w11_false_confidence,
            t.w11_total,
        );
    }
    eprintln!(
        "[W31-REAL-001] verdict: no-Sev1=true no-unauthorized=true \
         unsupported-claims=0 repeatable-W7-advantage=true"
    );
}
