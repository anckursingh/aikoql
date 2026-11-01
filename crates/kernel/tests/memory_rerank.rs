//! AG-22 — MMR reranking: over a scored candidate list, pick the top-k
//! trade-off between relevance and novelty, so near-duplicate memories do
//! not crowd out distinct ones. Pure reorder — scores are never invented.
//!
//! RED: `aikoql_kernel::mmr_rerank` does not exist.

use aikoql_kernel::kernel::mmr_rerank;
use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::transaction::kernel::ScoredKO;

const LAMBDA: f64 = 0.7;

fn hit(i: u8, text: &str, score: f32) -> ScoredKO {
    let mut ko = KnowledgeObject::new(
        KOID::from_bytes([i; KOID_LEN]),
        Metadata {
            type_name: "Memory".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
        SecurityDescriptor {
            owner: "ag22".into(),
            acl: vec![],
            classification: None,
        },
    );
    ko.properties
        .insert("content".into(), Value::Text(text.into()));
    ScoredKO {
        ko,
        score,
        index_lag_ms: 0,
    }
}

#[test]
fn mmr001_lambda_one_is_score_order() {
    let hits = vec![
        hit(1, "launch review covered the pricing page", 0.95),
        hit(2, "launch review covered the pricing pages", 0.90),
        hit(3, "the k8s lease apiserver certification passed", 0.85),
    ];
    let out = mmr_rerank(hits, 2, 1.0);
    let ids: Vec<u8> = out.iter().map(|h| h.ko.koid.as_bytes()[0]).collect();
    assert_eq!(ids, vec![1, 2], "λ=1 collapses to score order");
}

#[test]
fn mmr002_diversity_demotes_near_duplicates() {
    let hits = vec![
        hit(1, "launch review covered the pricing page", 0.95),
        hit(2, "launch review covered the pricing pages", 0.90),
        hit(3, "the k8s lease apiserver certification passed", 0.85),
    ];
    let out = mmr_rerank(hits, 2, LAMBDA);
    let ids: Vec<u8> = out.iter().map(|h| h.ko.koid.as_bytes()[0]).collect();
    assert_eq!(
        ids,
        vec![1, 3],
        "the near-duplicate (2) must lose the second slot to the distinct (3)"
    );
}

#[test]
fn mmr003_bounds_are_total() {
    let hits = vec![
        hit(1, "launch review covered the pricing page", 0.95),
        hit(2, "launch review covered the pricing pages", 0.90),
    ];
    assert!(mmr_rerank(vec![], 2, LAMBDA).is_empty());
    assert!(mmr_rerank(hits.clone(), 0, LAMBDA).is_empty());
    let all = mmr_rerank(hits, 5, LAMBDA);
    assert_eq!(all.len(), 2, "k beyond the list returns everything");
    // First pick is always the top score — MMR never invents relevance.
    assert_eq!(all[0].ko.koid.as_bytes()[0], 1);
}

/// The plan's latency cell: 50 candidates well inside the 30 ms budget
/// (one pass over a token-indexed list, no model call).
#[test]
fn mmr004_fifty_candidates_within_budget() {
    let mut hits = Vec::new();
    for i in 0..50 {
        let text = if i % 5 == 0 {
            format!("the weekly sync covered the {i} pricing page review")
        } else {
            format!("distinct fact number {i} about the apiserver lease")
        };
        hits.push(hit(i as u8, &text, 1.0 - i as f32 * 0.01));
    }
    let t = std::time::Instant::now();
    let out = mmr_rerank(hits, 10, LAMBDA);
    assert_eq!(out.len(), 10);
    // The 30 ms budget is the plan's release gate; debug builds (tests run
    // unoptimized, plus first-call allocator/page-fault warmup) get slack.
    let budget_ms = if cfg!(debug_assertions) { 300 } else { 30 };
    assert!(
        t.elapsed() < std::time::Duration::from_millis(budget_ms),
        "MMR over 50 candidates must stay within the 30 ms budget"
    );
}
