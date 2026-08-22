//! G12 (HLD §45–48, LLM-002): token / latency / cost — the AikoQL context
//! compiler vs a flat RAG chunk baseline, same corpus, same budget, same
//! golden-dataset questions.
//!
//! Treatments, both fully mechanical (no LLM, CI-reproducible):
//! - **AikoQL**: the knowledge-graph compiler — every fixture's IR merged
//!   into one graph (A3), `compile_context` packs the task-relevant
//!   entities/facts/relations under the budget.
//! - **RAG**: the baseline retriever — lexically ranked chunks packed in
//!   rank order until the budget runs out (token estimate len/4, the same
//!   convention the compiler uses).
//!
//! Per query both treatments report the payload the agent actually
//! receives and what it costs:
//! - **AikoQL**: `render_context_markdown` of the package — the exact
//!   text handed to the LLM, entity mention text included (an earlier
//!   version judged only the stripped triple text, under-measuring what
//!   the agent gets). Both the rendered len/4 and the compiler's own
//!   `estimated_tokens` bill (which counts justification lines the render
//!   omits, so it over-bills) are printed.
//! - **RAG**: the packed chunk text itself.
//!
//! Per query: tokens packed, KO coverage (fraction of the dataset's
//! expected KOs present in the delivered text), precision (relevant
//! units / delivered units — KOs for AikoQL, chunks for RAG; the context
//! efficiency number), golden-answer token hit, and wall time.
//!
//! **Measured verdict (mock corpus, fairness-corrected 2026-08-22): the
//! chunk baseline still wins, but by much less than the pre-fix
//! measurement claimed** — 74.8 vs 136.1 mean delivered tokens (the
//! compiler's own bill is 208.0: it charges justification lines the
//! render omits), 0.867 vs 0.600 answer-hit, 0.867 vs 0.778 KO coverage,
//! and precision is a tie (0.405 vs 0.402 relevant units per delivered
//! unit). Root causes, visible in the per-query rows: (1) fact scoring
//! matches statement keywords corpus-wide — any "revenue" question
//! hoovers every revenue fact from every fixture (q-00 delivers 273
//! tokens with zero relevant KOs); (2) the mock IR carries no facts at
//! all for pure-text fixtures (plain-text, formulas) and facts not
//! attached to a scored entity cannot rank, so those answers only exist
//! in raw chunks (q-13 E = mc²: the compiler delivers nothing). Both
//! causes have fixes in flight — the entity-gate on fact scoring, then
//! real extraction. The instrument is the yardstick for those runs — the
//! §45–48 efficiency claim is measured, not assumed, exactly like the
//! §60 PR-P verdict.
//!
//! The gates pin the measured baselines with headroom (the PR-G
//! convention): a regression — token bloat, ranking loss, latency
//! pathology — fails CI; an improvement passes trivially. The comparative
//! verdict itself is printed, not enforced.
//!
//! Cost column: reference rates for a fixed public-model price point
//! (USD per 1M tokens) × packed context tokens + a fixed 100-token answer.

mod common;

use common::golden_dataset::{compile_fixture_irs, normalize, queries, GOLDEN};
use common::{chunk_text, corpus, rank, tokens};
use std::time::Instant;

/// Token budget both treatments must respect (the §20 minimization range).
const BUDGET: usize = 500;

/// Reference pricing for the cost column: USD per 1M tokens at a fixed
/// public-model rate, so the column stays comparable across runs; swap
/// when the product pins a real model.
const INPUT_PRICE_PER_M: f32 = 0.15;
const OUTPUT_PRICE_PER_M: f32 = 0.60;
/// Fixed assumed answer length for the output-cost term.
const ANSWER_TOKENS: usize = 100;

/// The unified dataset's textual questions, aligned structurally with the
/// query projection (zip, not index — the answer lives next to its
/// question, the golden lives next to both).
fn textual_golden() -> Vec<&'static common::golden_dataset::GoldenQuestion> {
    GOLDEN.iter().filter(|g| g.textual).collect()
}

#[test]
fn comparative_cost_benchmark() {
    let provider = aikoql_ingestion::MockEmbeddingProvider::new();
    let (corpus, _) = corpus(&aikoql_ingestion::RuleBoundaryDetector, &provider);

    // AikoQL compiles from the merged knowledge graph; the RAG baseline
    // packs raw chunks from the same fixture corpus.
    let irs: Vec<aikoql_ingestion::KnowledgeIr> = compile_fixture_irs().into_values().collect();
    let merged = aikoql_ingestion::merge_knowledge_ir(&irs);
    eprintln!(
        "[COMPARATIVE-STRUCTURE] corpus_chunks={} merged_entities={} merged_facts={} \
         merged_relations={} budget={BUDGET}",
        corpus.len(),
        merged.entities.len(),
        merged.facts.len(),
        merged.relations.len(),
    );

    // Warm both paths once so the timed loop measures steady state.
    let _ = aikoql_ingestion::compile_context("warmup", &merged, BUDGET);
    let _ = rank(&corpus, "warmup", &provider, false);

    let qs = queries();
    let goldens = textual_golden();
    assert_eq!(qs.len(), goldens.len(), "projections out of alignment");

    let mut sum_tokens = [0usize; 2]; // [aikoql, rag]
    let mut sum_est = 0usize; // aikoql's own bill (rendered + justification over-bill)
    let mut sum_ko = [0.0f32; 2];
    let mut sum_prec = [0.0f32; 2];
    let mut sum_answer = [0usize; 2];
    let mut sum_lat = [0u128; 2]; // µs

    for (qi, (q, g)) in qs.iter().zip(&goldens).enumerate() {
        // ── AikoQL treatment ────────────────────────────────────────────
        let t0 = Instant::now();
        let pkg = aikoql_ingestion::compile_context(q.text, &merged, BUDGET);
        let a_lat = t0.elapsed().as_micros();
        assert!(
            pkg.estimated_tokens <= BUDGET,
            "{}: aikoql package exceeded the budget: {} > {BUDGET}",
            g.id,
            pkg.estimated_tokens
        );
        // Fairness fix: judge the payload the agent actually receives —
        // the rendered markdown (entity mention text included), not the
        // stripped triple text. estimated_tokens also bills justification
        // lines the render omits, so both numbers are printed; the cost
        // and gate axes use the rendered payload.
        let aikoql_delivered = aikoql_ingestion::render_context_markdown(&pkg);
        let a_tokens = aikoql_delivered.len() / 4;
        let a_ko = delivered_ko_coverage(&aikoql_delivered, g);
        let a_prec = ko_precision(&pkg, g);
        let a_answer = answer_hit(&aikoql_delivered, g);

        // ── RAG baseline treatment ──────────────────────────────────────
        let t0 = Instant::now();
        let ranked = rank(&corpus, q.text, &provider, false);
        let mut packed_text = String::new();
        let mut packed: Vec<(&str, usize)> = Vec::new();
        for (f, i) in &ranked {
            let text = chunk_text(&corpus, f, *i);
            // +1 for the joining space; stop before the budget overflows.
            if (packed_text.len() + text.len() + 1) / 4 > BUDGET {
                break;
            }
            packed_text.push_str(text);
            packed_text.push(' ');
            packed.push((*f, *i));
        }
        let r_lat = t0.elapsed().as_micros();
        let r_tokens = packed_text.len() / 4;
        assert!(
            r_tokens <= BUDGET,
            "{}: rag pack exceeded the budget: {r_tokens} > {BUDGET}",
            g.id
        );
        let r_ko = delivered_ko_coverage(&packed_text, g);
        let r_prec = chunk_precision(&packed, g);
        let r_answer = answer_hit(&packed_text, g);

        sum_tokens[0] += a_tokens;
        sum_tokens[1] += r_tokens;
        sum_est += pkg.estimated_tokens;
        sum_ko[0] += a_ko;
        sum_ko[1] += r_ko;
        sum_prec[0] += a_prec;
        sum_prec[1] += r_prec;
        sum_answer[0] += usize::from(a_answer);
        sum_answer[1] += usize::from(r_answer);
        sum_lat[0] += a_lat;
        sum_lat[1] += r_lat;

        eprintln!(
            "[COST-Q {qi} {:?}] aikoql_tokens={a_tokens} aikoql_est={} rag_tokens={r_tokens} \
             aikoql_ko={a_ko:.2} rag_ko={r_ko:.2} aikoql_prec={a_prec:.2} rag_prec={r_prec:.2} \
             aikoql_answer={a_answer} rag_answer={r_answer} aikoql_us={a_lat} rag_us={r_lat}",
            q.text, pkg.estimated_tokens,
        );
    }

    let n = qs.len() as f32;
    let aikoql_tokens = sum_tokens[0] as f32 / n;
    let rag_tokens = sum_tokens[1] as f32 / n;
    let aikoql_est = sum_est as f32 / n;
    let aikoql_ko = sum_ko[0] / n;
    let rag_ko = sum_ko[1] / n;
    let aikoql_prec = sum_prec[0] / n;
    let rag_prec = sum_prec[1] / n;
    let aikoql_answer = sum_answer[0] as f32 / n;
    let rag_answer = sum_answer[1] as f32 / n;
    let aikoql_lat = sum_lat[0] as f32 / n;
    let rag_lat = sum_lat[1] as f32 / n;
    let aikoql_cost =
        aikoql_tokens / 1e6 * INPUT_PRICE_PER_M + ANSWER_TOKENS as f32 / 1e6 * OUTPUT_PRICE_PER_M;
    let rag_cost =
        rag_tokens / 1e6 * INPUT_PRICE_PER_M + ANSWER_TOKENS as f32 / 1e6 * OUTPUT_PRICE_PER_M;
    eprintln!(
        "[COMPARATIVE-SUMMARY] queries={n} aikoql_tokens={aikoql_tokens:.1} \
         aikoql_est={aikoql_est:.1} rag_tokens={rag_tokens:.1} \
         aikoql_ko={aikoql_ko:.3} rag_ko={rag_ko:.3} aikoql_prec={aikoql_prec:.3} \
         rag_prec={rag_prec:.3} aikoql_answer={aikoql_answer:.3} \
         rag_answer={rag_answer:.3} aikoql_us={aikoql_lat:.1} rag_us={rag_lat:.1} \
         aikoql_cost={aikoql_cost:.4} rag_cost={rag_cost:.4}"
    );

    // ── Gates ───────────────────────────────────────────────────────────
    // Pinned baselines + headroom, fairness-corrected and re-measured
    // 2026-08-22 on the deterministic mock corpus (delivered payload:
    // aikoql 136.1 rendered tokens [own bill 208.0], rag 74.8; answer-hit
    // 0.600 vs 0.867; KO coverage 0.778 vs 0.867; precision 0.402 vs
    // 0.405; latency 2.0ms vs 0.4ms/query). A regression fails, an
    // improvement passes trivially — the PR-G convention. The comparative
    // verdict is printed, not enforced: with the mock extraction IR the
    // chunk baseline wins the token and answer-hit axes (module docs),
    // and only the entity-gate fix + a real-extraction run may flip it.
    assert!(
        aikoql_tokens < 250.0,
        "aikoql token cost regressed: {aikoql_tokens:.1} tokens/query (baseline 136.1)"
    );
    assert!(
        rag_tokens < 150.0,
        "rag token cost regressed: {rag_tokens:.1} tokens/query (baseline 74.8)"
    );
    assert!(
        aikoql_ko > 0.60,
        "aikoql KO coverage regressed: {aikoql_ko:.3} (baseline 0.778)"
    );
    assert!(
        rag_ko > 0.75,
        "rag KO coverage regressed: {rag_ko:.3} (baseline 0.867)"
    );
    assert!(
        aikoql_prec > 0.30,
        "aikoql KO precision regressed: {aikoql_prec:.3} (baseline 0.402)"
    );
    assert!(
        rag_prec > 0.30,
        "rag chunk precision regressed: {rag_prec:.3} (baseline 0.405)"
    );
    assert!(
        aikoql_answer > 0.40,
        "aikoql answer-hit regressed: {aikoql_answer:.3} (baseline 0.600)"
    );
    assert!(
        rag_answer > 0.75,
        "rag answer-hit regressed: {rag_answer:.3} (baseline 0.867)"
    );
    // Latency sanity: both treatments are in-memory over a small corpus;
    // a pathological slowdown (accidental O(n^2), per-query IO) fails.
    assert!(
        aikoql_lat < 100_000.0,
        "aikoql context compilation regressed: {aikoql_lat:.1} µs/query"
    );
    assert!(
        rag_lat < 100_000.0,
        "rag baseline pack regressed: {rag_lat:.1} µs/query"
    );
}

/// Fraction of the dataset's expected KOs whose normalized names occur in
/// the delivered context text (containment — delivered text carries them,
/// whether as a structured name or a mention span). Applied to both
/// treatments so the comparison judges the same payload definition.
fn delivered_ko_coverage(delivered: &str, g: &common::golden_dataset::GoldenQuestion) -> f32 {
    if g.expected_entities.is_empty() {
        return 1.0;
    }
    let lower = delivered.to_lowercase();
    let hit = g
        .expected_entities
        .iter()
        .filter(|e| lower.contains(&normalize(e)))
        .count();
    hit as f32 / g.expected_entities.len() as f32
}

/// KO precision (context efficiency): relevant KOs delivered / KOs
/// delivered. An empty package delivers nothing, so precision is 0.
fn ko_precision(
    pkg: &aikoql_ingestion::ContextPackage,
    g: &common::golden_dataset::GoldenQuestion,
) -> f32 {
    if g.expected_entities.is_empty() {
        return 1.0;
    }
    if pkg.entities.is_empty() {
        return 0.0;
    }
    let hit = pkg
        .entities
        .iter()
        .filter(|e| {
            g.expected_entities
                .iter()
                .any(|x| normalize(x) == normalize(&e.name))
        })
        .count();
    hit as f32 / pkg.entities.len() as f32
}

/// Chunk precision (context efficiency): relevant chunks packed / chunks
/// packed — the RAG baseline's analogue of KO precision.
fn chunk_precision(packed: &[(&str, usize)], g: &common::golden_dataset::GoldenQuestion) -> f32 {
    if g.relevant.is_empty() {
        return 1.0;
    }
    if packed.is_empty() {
        return 0.0;
    }
    let hit = packed
        .iter()
        .filter(|(f, i)| g.relevant.iter().any(|(rf, ri)| rf == f && ri == i))
        .count();
    hit as f32 / packed.len() as f32
}

/// Whether the packed context carries every golden answer key token.
fn answer_hit(packed: &str, g: &common::golden_dataset::GoldenQuestion) -> bool {
    let packed_tokens = tokens(packed);
    tokens(g.expected_answer)
        .iter()
        .all(|t| packed_tokens.contains(t))
}
