//! G11 (TESTING-PLAN, chatbot suite §52): the ultimate comparative
//! experiment — four treatments over the same corpus, questions, budget,
//! and judge, with a measured-results-only table:
//!
//! - **A: LLM only** — no retrieval; the agent receives the question alone.
//! - **B: LLM + RAG** — `common::rank` lexical retrieval packs chunks.
//! - **C: LLM + Graph-RAG** — the mechanical Graph-RAG flavor: the lexical
//!   rank seeds the pack, then the entity graph expands it — every chunk
//!   naming an entity named by a packed chunk is added, transitively
//!   (entity→chunk links = name mentions, the extracted graph's link
//!   basis).
//! - **D: LLM + AIKOQL** — the merged knowledge IR compiled for the task
//!   (entity gate + relation boost + semantic floor), rendered markdown.
//!
//! Corpus + questions: `common::trackb` (Track-B), plus one COMP-005
//! provenance question — "Where does the retry limit come from?" — whose
//! second unit is the source document id, which only a payload that
//! *cites its sources* can deliver.
//!
//! Mechanical slice (the G12 convention — CI-reproducible, no live model):
//! all four treatments are judged on the payload the LLM would receive, so
//! the rows that need a generated answer are honest about their proxy:
//! - Accuracy = delivered evidence units / 16 (token containment).
//! - Groundedness = fraction of delivered units whose payload carries a
//!   source citation (doc-id token) — B/C copy raw chunks with no doc id,
//!   D renders [`doc`] per entity.
//! - Hallucination rate = 0.0 for all four *by construction*: every
//!   treatment copies corpus text verbatim, there is no generative step.
//!   The real-model pass for this row is the `e2e_answer_quality` harness
//!   (answer_gen seam, §53).
//! - Provenance accuracy = the COMP-005 question (answer + its source).
//! - Temporal accuracy = the Q3 probe: current claim present AND stale
//!   claim absent. Measured 0.0 for every treatment — none suppresses the
//!   stale claim (the open temporal-policy item, Track-B documented).
//! - Multi-hop accuracy = units over the hop / cross-doc / depth-2
//!   questions.
//! - Memory continuity / Action safety = "—" here: they need the live
//!   chatbot stack; measured by the G5 §51 MCP scenarios (TP-3b) instead.
//! - LLM calls = the proxy the plan documents (SEM-003): A/B/C need one
//!   LLM turn to answer; D resolves deterministically via the compile
//!   path, no call.
//! - Input tokens / Latency / Cost = measured per treatment (G12 rates:
//!   USD 0.15/1M input + 0.60/1M output × 100 assumed answer tokens).
//!
//! Expected treatment separation (hand-verified per question, see the
//! rows): A 0/16 — retrieval-free; B 10/16 — every unit whose words appear
//! in a ranked chunk, none of the zero-overlap answer facts; C 13/16 — B
//! plus the graph expansion reaches Q2's RepairVendor chunk and Q6's full
//! A→B→C chain (transitive expansion walks it where the compiler's
//! single-round boost stops); D 15/16 — C's reach plus the source-cited
//! provenance unit; the only miss is the depth-2 leaf fact (the documented
//! single-round boost ceiling).
//!
//! Gates pin the separation with headroom: D ≥ 14, B ≤ 11, C > B (the
//! Graph-RAG structural result), D > C, and D's provenance citation
//! pinned at 2/2 — a render regression that drops the [`doc`] citation
//! fails CI.

mod common;

use aikoql_ingestion::{
    compile_context, merge_knowledge_ir, render_context_markdown, KnowledgeIr,
    MockEmbeddingProvider,
};
use common::trackb::{assert_integrity, corpus, docs, units_hit, Question};
use std::collections::HashSet;
use std::time::Instant;

/// Token budget all treatments respect (len/4 estimate, the Track-B/G12
/// convention); generous so retrieval misses come from ranking, not the
/// budget.
const BUDGET: usize = 300;
/// G12 reference rates (USD per 1M tokens) + assumed answer length.
const INPUT_PRICE_PER_M: f32 = 0.15;
const OUTPUT_PRICE_PER_M: f32 = 0.60;
const ANSWER_TOKENS: usize = 100;

/// Per-question run costs, summed per treatment.
struct Run {
    tokens: usize,
    retrieval: usize,
    calls: usize,
    micros: u128,
}

#[derive(Default)]
struct Stats {
    units: usize,
    sourced: usize,
    temporal: usize,
    multihop: usize,
    prov: usize,
    tokens: usize,
    retrieval: usize,
    calls: usize,
    micros: u128,
}

impl Stats {
    fn record(&mut self, q: &Question, hits: [bool; 2], payload: &str, run: Run) {
        let n = hits.iter().filter(|h| **h).count();
        self.units += n;
        let sourced = common::tokens(payload).contains("kb");
        self.sourced += hits.iter().filter(|h| **h && sourced).count();
        // Temporal accuracy: the CURRENT claim present and the STALE one
        // absent (Q3's units are ordered [current, stale]).
        if q.kind == "temporal-probe" && hits[0] && !hits[1] {
            self.temporal += 1;
        }
        if matches!(q.kind, "hop" | "cross-doc" | "depth-2-probe") {
            self.multihop += n;
        }
        if q.kind == "provenance" {
            self.prov += n;
        }
        self.tokens += run.tokens;
        self.retrieval += run.retrieval;
        self.calls += run.calls;
        self.micros += run.micros;
    }

    fn cost(&self, queries: usize) -> f32 {
        self.tokens as f32 / 1e6 * INPUT_PRICE_PER_M
            + (queries * ANSWER_TOKENS) as f32 / 1e6 * OUTPUT_PRICE_PER_M
    }
}

/// Treatment C's graph expansion: every chunk naming an entity named by a
/// packed chunk is added, transitively, until no new chunk arrives.
/// `index` maps each (lowercased) entity name to the corpus positions of
/// the chunks that mention it — the extracted graph's entity→chunk links.
/// ponytail: expansion is transitive unbounded within the corpus; a real
/// Graph-RAG caps hops/top-N, the corpus here is 20 chunks.
fn graph_expand(
    seed: &[usize],
    corpus: &[common::CorpusChunk],
    index: &[(String, Vec<usize>)],
) -> Vec<usize> {
    let mut order = seed.to_vec();
    let mut packed: HashSet<usize> = seed.iter().copied().collect();
    loop {
        let texts: Vec<String> = order.iter().map(|&p| corpus[p].2.to_lowercase()).collect();
        let mut added = false;
        for (name, chunks) in index {
            if !texts.iter().any(|t| t.contains(name)) {
                continue;
            }
            for &p in chunks {
                if packed.insert(p) {
                    order.push(p);
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
    }
    order
}

#[test]
fn comparative_chatbot_bench() {
    let provider = MockEmbeddingProvider::new();
    let docs = docs();
    let corpus = corpus(&docs);
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    assert_integrity(&docs, &merged);

    // COMP-005 provenance question appended to the shared Track-B set.
    let mut questions: Vec<Question> = common::trackb::QUESTIONS.to_vec();
    questions.push(Question {
        kind: "provenance",
        class: "W7",
        text: "Where does the retry limit come from?",
        units: ["Retry limit is 3 attempts.", "kb-payments"],
    });

    // Entity→chunk links for treatment C, from the merged graph.
    let index: Vec<(String, Vec<usize>)> = merged
        .entities
        .iter()
        .map(|e| e.name.to_lowercase())
        .map(|n| {
            let chunks: Vec<usize> = corpus
                .iter()
                .enumerate()
                .filter(|(_, (_, _, t))| t.to_lowercase().contains(&n))
                .map(|(p, _)| p)
                .collect();
            (n, chunks)
        })
        .collect();
    // Ranked (fixture, index) → corpus position.
    let pos: std::collections::HashMap<(&str, usize), usize> = corpus
        .iter()
        .enumerate()
        .map(|(p, (f, i, _))| ((*f, *i), p))
        .collect();

    let mut a = Stats::default();
    let mut b = Stats::default();
    let mut c = Stats::default();
    let mut d = Stats::default();

    for (qi, q) in questions.iter().enumerate() {
        // ── A: no retrieval — the LLM receives the question alone ────────
        let t0 = Instant::now();
        let (ah, a_hits) = units_hit("", q);
        a.record(
            q,
            a_hits,
            "",
            Run {
                tokens: q.text.len() / 4,
                retrieval: 0,
                calls: 1,
                micros: t0.elapsed().as_micros(),
            },
        );

        // ── B: lexical RAG ────────────────────────────────────────────────
        let t0 = Instant::now();
        let ranked = common::rank(&corpus, q.text, &provider, false);
        let mut b_packed = String::new();
        let mut b_chunks = 0usize;
        for (f, i) in &ranked {
            let text = common::chunk_text(&corpus, f, *i);
            if (b_packed.len() + text.len() + 1) / 4 > BUDGET {
                break;
            }
            b_packed.push_str(text);
            b_packed.push(' ');
            b_chunks += 1;
        }
        let (bh, b_hits) = units_hit(&b_packed, q);
        b.record(
            q,
            b_hits,
            &b_packed,
            Run {
                tokens: b_packed.len() / 4,
                retrieval: b_chunks,
                calls: 1,
                micros: t0.elapsed().as_micros(),
            },
        );

        // ── C: lexical seed + entity-graph expansion ──────────────────────
        let t0 = Instant::now();
        let seed: Vec<usize> = ranked.iter().map(|(f, i)| pos[&(*f, *i)]).collect();
        let expanded = graph_expand(&seed, &corpus, &index);
        let mut c_packed = String::new();
        let mut c_chunks = 0usize;
        for &p in &expanded {
            let text = &corpus[p].2;
            if (c_packed.len() + text.len() + 1) / 4 > BUDGET {
                break;
            }
            c_packed.push_str(text);
            c_packed.push(' ');
            c_chunks += 1;
        }
        let (ch, c_hits) = units_hit(&c_packed, q);
        c.record(
            q,
            c_hits,
            &c_packed,
            Run {
                tokens: c_packed.len() / 4,
                retrieval: c_chunks,
                calls: 1,
                micros: t0.elapsed().as_micros(),
            },
        );

        // ── D: AIKOQL — deterministic compile path, no LLM call ──────────
        let t0 = Instant::now();
        let pkg = compile_context(q.text, &merged, BUDGET);
        assert!(
            pkg.estimated_tokens <= BUDGET,
            "{}: aikoql package exceeded the budget: {} > {BUDGET}",
            q.text,
            pkg.estimated_tokens
        );
        let delivered = render_context_markdown(&pkg);
        let d_retrieval = pkg.entities.len() + pkg.facts.len() + pkg.relations.len();
        let (dh, d_hits) = units_hit(&delivered, q);
        d.record(
            q,
            d_hits,
            &delivered,
            Run {
                tokens: delivered.len() / 4,
                retrieval: d_retrieval,
                calls: 0,
                micros: t0.elapsed().as_micros(),
            },
        );

        eprintln!(
            "[G11 Q{qi} {} {:?}] A={ah}/2 {:?} B={bh}/2 {:?} C={ch}/2 {:?} D={dh}/2 {:?} \
             tokens={}/{}/{}/{} retrieval={}/{}/{}/{}",
            q.kind,
            q.text,
            a_hits.map(|h| if h { "hit" } else { "miss" }),
            b_hits.map(|h| if h { "hit" } else { "miss" }),
            c_hits.map(|h| if h { "hit" } else { "miss" }),
            d_hits.map(|h| if h { "hit" } else { "miss" }),
            q.text.len() / 4,
            b_packed.len() / 4,
            c_packed.len() / 4,
            delivered.len() / 4,
            0,
            b_chunks,
            c_chunks,
            d_retrieval,
        );
    }

    let nq = questions.len();
    let total_units = nq * 2;
    let multihop_units = 8; // Q0, Q1, Q2, Q6
    let prov_units = 2;
    let table = [&a, &b, &c, &d];
    eprintln!(
        "[G11-§52 SUMMARY] questions={nq} budget={BUDGET} units a={} b={} c={} d={}",
        a.units, b.units, c.units, d.units
    );
    eprintln!("[G11-§52 TABLE] metric | A: LLM only | B: LLM + RAG | C: LLM + Graph-RAG | D: LLM + AIKOQL");
    for (label, cells) in [
        (
            "Accuracy",
            table.map(|s| format!("{:.3}", s.units as f32 / total_units as f32)),
        ),
        (
            "Groundedness",
            table.map(|s| {
                format!(
                    "{:.3}",
                    if s.units == 0 {
                        0.0
                    } else {
                        s.sourced as f32 / s.units as f32
                    }
                )
            }),
        ),
        (
            "Hallucination rate (mechanical)",
            table.map(|_| "0.000".to_string()),
        ),
        (
            "Provenance accuracy",
            table.map(|s| format!("{:.3}", s.prov as f32 / prov_units as f32)),
        ),
        (
            "Temporal accuracy",
            table.map(|s| format!("{:.3}", s.temporal as f32)),
        ),
        (
            "Multi-hop accuracy",
            table.map(|s| format!("{:.3}", s.multihop as f32 / multihop_units as f32)),
        ),
        (
            "Memory continuity",
            table.map(|_| "n/a (G5 §51 MCP scenarios)".to_string()),
        ),
        (
            "Input tokens (mean)",
            table.map(|s| format!("{:.1}", s.tokens as f32 / nq as f32)),
        ),
        (
            "LLM calls (SEM-003 proxy)",
            table.map(|s| format!("{:.0}", s.calls as f32 / nq as f32)),
        ),
        (
            "Latency µs/query",
            table.map(|s| format!("{:.0}", s.micros as f32 / nq as f32)),
        ),
        (
            "Cost USD/query",
            table.map(|s| format!("{:.6}", s.cost(nq) / nq as f32)),
        ),
        (
            "Action safety",
            table.map(|_| "n/a (G5 §51 MCP scenarios)".to_string()),
        ),
    ] {
        eprintln!(
            "[G11-§52 TABLE] {label} | {} | {} | {} | {}",
            cells[0], cells[1], cells[2], cells[3]
        );
    }

    // ── Gates: pin the treatment separation with headroom ────────────────
    // Expected: A 0/16 (retrieval-free), B 10/16, C 13/16 (graph expansion
    // reaches Q2's RepairVendor chunk and walks Q6's full chain), D 15/16
    // (the only miss is the depth-2 leaf fact — single-round boost).
    assert_eq!(
        a.units, 0,
        "treatment A must be retrieval-free: {}",
        a.units
    );
    assert!(
        d.units >= 14,
        "aikoql coverage regressed: {}/{} (expected 15)",
        d.units,
        total_units
    );
    assert!(
        b.units <= 11,
        "rag baseline covered knowledge it should not: {}/{} (expected 10)",
        b.units,
        total_units
    );
    assert!(
        c.units > b.units,
        "graph-rag expansion stopped working: C {} vs B {}",
        c.units,
        b.units
    );
    assert!(
        d.units > c.units,
        "structural separation lost: D {} vs C {}",
        d.units,
        c.units
    );
    assert_eq!(
        d.prov, 2,
        "aikoql must cite the source of the retry limit: prov units {}/2",
        d.prov
    );
}
