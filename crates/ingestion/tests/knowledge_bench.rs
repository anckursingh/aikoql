//! Track-B (TESTING-PLAN P1): the knowledge-centric benchmark — questions
//! where a flat chunk retriever cannot win structurally: multi-hop, cross-
//! document, and graph-traversal answers whose content shares no keywords
//! with the question.
//!
//! Treatments, both fully mechanical over the SAME synthetic corpus (the
//! G12 convention, CI-reproducible):
//! - **AikoQL**: the hand-built IRs of 15 documents merged into one graph
//!   (A3), then `compile_context` packs the package under the budget. The
//!   corpus is synthetic because the real mock pipeline extracts relations
//!   only from markdown links; Track-B needs hand-authored graph structure
//!   to pose the questions at all.
//! - **RAG**: `common::rank` lexical retriever (exact token overlap, the
//!   G12 baseline) packs ranked chunks until the budget runs out. Zero-
//!   overlap chunks are dropped at RANK time — the structural miss is the
//!   retriever's, not the budget's (the corpus fits well under the budget,
//!   so every ranked chunk packs deterministically).
//!
//! Judge: each question requires 2 evidence units (a fact statement and/or
//! a relation triple); a unit is delivered when every token of the unit
//! string appears in the payload — token containment, the same definition
//! G12 uses for answer-hit. Both treatments are judged on the payload the
//! agent actually receives: the rendered markdown for AikoQL, the packed
//! chunk text for RAG.
//!
//! Corpus integrity (fairness): every fact statement appears verbatim
//! (token-identical) in a chunk of its document; every entity name appears
//! in a chunk of its document; every relation's endpoints co-occur in a
//! chunk of its document (its extraction basis). AikoQL gets no knowledge
//! RAG could not in principle have retrieved from the same text.
//!
//! Question types:
//! - Q0/Q1 multi-hop: the question names the hub entity; the answer fact
//!   lives in a chunk with zero lexical overlap. AikoQL follows the
//!   relation boost to the neighbor's fact; RAG never ranks the chunk.
//! - Q2 cross-document: hub doc carries the relation, a second doc carries
//!   the answer fact (keyword-invisible). Traversal across the merge.
//! - Q3/Q4 probes (documented, not scored as wins): temporal supersession
//!   and contradiction. BOTH treatments surface both claims — neither
//!   compiler nor retriever suppresses the stale/conflicting claim (no
//!   temporal policy in the compiler; a trust-model/temporal-policy item
//!   remains open).
//! - Q5 control: a plain single-doc keyword question — RAG's home turf.
//!   Both treatments must deliver both units, or the bench is rigged.
//! - Q6 depth-2 probe: A → B → C. The boost is single-round (ponytail:
//!   no transitivity in context.rs), so B ranks but C's fact is gated
//!   out; the B→C RELATION still renders (B was boosted). AikoQL delivers
//!   the pointer but not the content — the documented ceiling that
//!   progressive context expansion (P1) is meant to lift.
//!
//! Determinism: hand-built IRs in a fixed doc order, deterministic merge
//! (BTreeMaps), deterministic compiler tie-breaks, and `common::rank`'s
//! deterministic sort — the bench is bit-reproducible.
//!
//! The gates pin the structural separation with headroom (the PR-G
//! convention): a regression in graph traversal — or a leak that hands RAG
//! the zero-overlap chunks — fails CI; an improvement passes trivially.
//!
//! The corpus + questions live in `common::trackb` — shared with the G11
//! §52 comparative experiment (`comparative_chatbot_bench.rs`).

mod common;

use aikoql_ingestion::{
    compile_context, merge_knowledge_ir, render_context_markdown, KnowledgeIr,
    MockEmbeddingProvider,
};
use common::trackb::{assert_integrity, corpus, docs, units_hit, QUESTIONS};

/// Token budget both treatments must respect (len/4 estimate, the G12
/// convention). Deliberately large enough that RAG packs every ranked
/// chunk — its misses must come from ranking, not the budget — and small
/// enough that AikoQL still minimizes (worst question est ≈ 240).
const BUDGET: usize = 300;

#[test]
fn knowledge_bench() {
    let provider = MockEmbeddingProvider::new();
    let docs = docs();

    // Both treatments read the same documents: chunks for RAG, IRs for
    // AikoQL (fixed doc order → deterministic merge).
    let corpus = corpus(&docs);
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    assert_integrity(&docs, &merged);
    eprintln!(
        "[TRACK-B STRUCTURE] chunks={} merged_entities={} merged_facts={} merged_relations={} budget={BUDGET}",
        corpus.len(),
        merged.entities.len(),
        merged.facts.len(),
        merged.relations.len(),
    );

    let mut a_units = 0usize;
    let mut r_units = 0usize;
    let mut a_tokens = 0usize;
    let mut r_tokens = 0usize;

    for (qi, q) in QUESTIONS.iter().enumerate() {
        // ── AikoQL treatment ──────────────────────────────────────────────
        let pkg = compile_context(q.text, &merged, BUDGET);
        assert!(
            pkg.estimated_tokens <= BUDGET,
            "{}: aikoql package exceeded the budget: {} > {BUDGET}",
            q.text,
            pkg.estimated_tokens
        );
        let delivered = render_context_markdown(&pkg);
        let (ah, a_hits) = units_hit(&delivered, q);

        // ── RAG baseline treatment ───────────────────────────────────────
        let ranked = common::rank(&corpus, q.text, &provider, false);
        let mut packed_text = String::new();
        for (f, i) in &ranked {
            let text = common::chunk_text(&corpus, f, *i);
            if (packed_text.len() + text.len() + 1) / 4 > BUDGET {
                break;
            }
            packed_text.push_str(text);
            packed_text.push(' ');
        }
        let r_delivered_tokens = packed_text.len() / 4;
        assert!(
            r_delivered_tokens <= BUDGET,
            "{}: rag pack exceeded the budget: {r_delivered_tokens} > {BUDGET}",
            q.text
        );
        let (rh, r_hits) = units_hit(&packed_text, q);

        a_units += ah;
        r_units += rh;
        a_tokens += delivered.len() / 4;
        r_tokens += r_delivered_tokens;

        eprintln!(
            "[TRACK-B Q{qi} {} {:?}] aikoql={ah}/2 {:?} rag={rh}/2 {:?} aikoql_tokens={} rag_tokens={}",
            q.kind,
            q.text,
            a_hits.map(|h| if h { "hit" } else { "miss" }),
            r_hits.map(|h| if h { "hit" } else { "miss" }),
            delivered.len() / 4,
            r_delivered_tokens,
        );
    }

    let n = QUESTIONS.len();
    eprintln!(
        "[TRACK-B SUMMARY] questions={n} aikoql_units={a_units}/{} rag_units={r_units}/{} \
         aikoql_tokens={} rag_tokens={}",
        n * 2,
        n * 2,
        a_tokens / n,
        r_tokens / n,
    );

    // ── Gates: pin the structural separation with headroom ────────────────
    // Expected (hand-verified per question): AikoQL 13/14 — the only miss
    // is the depth-2 probe's leaf fact (single-round boost, documented
    // ceiling); RAG 9/14 — it retrieves every unit whose words appear in
    // any ranked chunk, and none of the zero-overlap answer facts. A
    // regression in traversal, or a leak that hands RAG the zero-overlap
    // chunks, fails CI.
    assert!(
        a_units >= 12,
        "aikoql knowledge coverage regressed: {a_units}/{} (expected 13)",
        n * 2
    );
    assert!(
        r_units <= 10,
        "rag baseline covered knowledge it should not: {r_units}/{} (expected 9)",
        n * 2
    );
    assert!(
        a_units > r_units,
        "structural separation lost: aikoql {a_units} vs rag {r_units}"
    );
}
