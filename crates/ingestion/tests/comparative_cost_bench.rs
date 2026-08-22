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
//! Per query both treatments report tokens packed, KO coverage (fraction
//! of the dataset's expected KOs present in the packed context), whether
//! the packed context carries the golden answer's key tokens, and wall
//! time.
//!
//! **Measured verdict (mock corpus, 2026-08-22): the chunk baseline
//! currently wins both axes** — 74.8 vs 207.9 mean tokens and 0.867 vs
//! 0.467 answer-hit, at equal KO coverage. Root causes, visible in the
//! per-query rows: (1) fact scoring matches statement keywords corpus-wide
//! — any "revenue" question hoovers every revenue fact from every fixture
//! (q-00 packs 439 tokens with zero relevant KOs); (2) the mock IR carries
//! no facts at all for pure-text fixtures (plain-text, formulas) and
//! facts not attached to a scored entity cannot rank, so those answers
//! only exist in raw chunks. The instrument is the yardstick for the
//! real-extraction/real-model runs — the §45–48 efficiency claim is
//! measured, not assumed, exactly like the §60 PR-P verdict.
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
    let mut sum_ko = [0.0f32; 2];
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
        let pkg_names: Vec<String> = pkg.entities.iter().map(|e| normalize(&e.name)).collect();
        let mut pkg_text = String::new();
        for e in &pkg.entities {
            pkg_text.push_str(&e.name);
            pkg_text.push(' ');
        }
        for f in &pkg.facts {
            pkg_text.push_str(&f.statement);
            pkg_text.push(' ');
        }
        for r in &pkg.relations {
            pkg_text.push_str(&r.subject);
            pkg_text.push(' ');
            pkg_text.push_str(&r.object);
            pkg_text.push(' ');
        }
        let a_ko = ko_coverage(&pkg_names, g);
        let a_answer = answer_hit(&pkg_text, g);

        // ── RAG baseline treatment ──────────────────────────────────────
        let t0 = Instant::now();
        let ranked = rank(&corpus, q.text, &provider, false);
        let mut packed_text = String::new();
        for (f, i) in &ranked {
            let text = chunk_text(&corpus, f, *i);
            // +1 for the joining space; stop before the budget overflows.
            if (packed_text.len() + text.len() + 1) / 4 > BUDGET {
                break;
            }
            packed_text.push_str(text);
            packed_text.push(' ');
        }
        let r_lat = t0.elapsed().as_micros();
        let r_tokens = packed_text.len() / 4;
        assert!(
            r_tokens <= BUDGET,
            "{}: rag pack exceeded the budget: {r_tokens} > {BUDGET}",
            g.id
        );
        let r_ko = chunk_ko_coverage(&packed_text, g);
        let r_answer = answer_hit(&packed_text, g);

        sum_tokens[0] += pkg.estimated_tokens;
        sum_tokens[1] += r_tokens;
        sum_ko[0] += a_ko;
        sum_ko[1] += r_ko;
        sum_answer[0] += usize::from(a_answer);
        sum_answer[1] += usize::from(r_answer);
        sum_lat[0] += a_lat;
        sum_lat[1] += r_lat;

        eprintln!(
            "[COST-Q {qi} {:?}] aikoql_tokens={} rag_tokens={r_tokens} aikoql_ko={a_ko:.2} \
             rag_ko={r_ko:.2} aikoql_answer={a_answer} rag_answer={r_answer} \
             aikoql_us={a_lat} rag_us={r_lat}",
            q.text, pkg.estimated_tokens,
        );
    }

    let n = qs.len() as f32;
    let aikoql_tokens = sum_tokens[0] as f32 / n;
    let rag_tokens = sum_tokens[1] as f32 / n;
    let aikoql_ko = sum_ko[0] / n;
    let rag_ko = sum_ko[1] / n;
    let aikoql_answer = sum_answer[0] as f32 / n;
    let rag_answer = sum_answer[1] as f32 / n;
    let aikoql_lat = sum_lat[0] as f32 / n;
    let rag_lat = sum_lat[1] as f32 / n;
    let aikoql_cost =
        aikoql_tokens / 1e6 * INPUT_PRICE_PER_M + ANSWER_TOKENS as f32 / 1e6 * OUTPUT_PRICE_PER_M;
    let rag_cost =
        rag_tokens / 1e6 * INPUT_PRICE_PER_M + ANSWER_TOKENS as f32 / 1e6 * OUTPUT_PRICE_PER_M;
    eprintln!(
        "[COMPARATIVE-SUMMARY] queries={n} aikoql_tokens={aikoql_tokens:.1} rag_tokens={rag_tokens:.1} \
         aikoql_ko={aikoql_ko:.3} rag_ko={rag_ko:.3} aikoql_answer={aikoql_answer:.3} \
         rag_answer={rag_answer:.3} aikoql_us={aikoql_lat:.1} rag_us={rag_lat:.1} \
         aikoql_cost={aikoql_cost:.4} rag_cost={rag_cost:.4}"
    );

    // ── Gates ───────────────────────────────────────────────────────────
    // Pinned baselines + headroom, measured 2026-08-22 on the deterministic
    // mock corpus (aikoql 207.9 tokens/query, rag 74.8; answer-hit 0.467 vs
    // 0.867; KO coverage 0.778 vs 0.867; latency 2.8ms vs 0.4ms/query). A
    // regression fails, an improvement passes trivially — the PR-G
    // convention. The comparative verdict is printed, not enforced: with
    // the mock extraction IR the chunk baseline wins both axes (module
    // docs), and only a real-extraction/real-model run may flip it.
    assert!(
        aikoql_tokens < 300.0,
        "aikoql token cost regressed: {aikoql_tokens:.1} tokens/query (baseline 207.9)"
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
        aikoql_answer > 0.35,
        "aikoql answer-hit regressed: {aikoql_answer:.3} (baseline 0.467)"
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

/// Fraction of the dataset's expected KOs whose normalized names appear in
/// the package's entity list (exact normalized match — entities are what
/// the compiler packs as KOs).
fn ko_coverage(pkg_names: &[String], g: &common::golden_dataset::GoldenQuestion) -> f32 {
    if g.expected_entities.is_empty() {
        return 1.0;
    }
    let hit = g
        .expected_entities
        .iter()
        .filter(|e| pkg_names.contains(&normalize(e)))
        .count();
    hit as f32 / g.expected_entities.len() as f32
}

/// Fraction of the dataset's expected KOs whose normalized names occur in
/// the packed chunk text (containment — chunks carry the raw text).
fn chunk_ko_coverage(packed: &str, g: &common::golden_dataset::GoldenQuestion) -> f32 {
    if g.expected_entities.is_empty() {
        return 1.0;
    }
    let lower = packed.to_lowercase();
    let hit = g
        .expected_entities
        .iter()
        .filter(|e| lower.contains(&normalize(e)))
        .count();
    hit as f32 / g.expected_entities.len() as f32
}

/// Whether the packed context carries every golden answer key token.
fn answer_hit(packed: &str, g: &common::golden_dataset::GoldenQuestion) -> bool {
    let packed_tokens = tokens(packed);
    tokens(g.expected_answer)
        .iter()
        .all(|t| packed_tokens.contains(t))
}
