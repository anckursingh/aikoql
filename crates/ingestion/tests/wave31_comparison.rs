//! Wave 3.1 (MVP-QA-003A) — W31-COMP-001: RAG vs Graph-RAG vs AIKOQL.
//!
//! Three mechanical treatments over the 148-task union corpus, judged by
//! the win-zone contract (delivered units / 2, unknown-probe inversion —
//! the Wave 3 frozen judge), rolled up per workload class. The frozen
//! holdout gets exactly one pass (w31_comp_002), printed, never asserted
//! here — its numbers are pinned into the evidence docs.
//!
//! Predefined acceptance (spec COMP-001 — declared BEFORE first
//! measurement, spec §4 TDD order; thresholds are never adjusted to fit
//! the measurement):
//! - ≥1 workload class with a Strong Fit: AIKOQL class fraction ≥0.75 AND
//!   strictly more delivered units than conventional RAG (the "predefined
//!   meaningful advantage").
//! - No class where AIKOQL falls behind conventional RAG by more than 2
//!   units (the "unacceptable correctness regression" bound).
//! - W1 (lookup) control class at full parity: if the control doesn't
//!   tie, the bench is rigged in someone's favor.
//!
//! Held constant (spec COMP-001): corpus, task set, budget, judge — and
//! the LLM/model/prompt/temperature row is held constant at *none*: this
//! is the mechanical slice (the G11/G12 convention) judging the payload
//! the LLM would receive. Tool calls and LLM calls are 0 for all three by
//! construction; the real-model leg is REAL-001's gated harness (#162).

mod common;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use aikoql_ingestion::{
    compile_context, merge_knowledge_ir, render_context_markdown, KnowledgeIr,
    MockEmbeddingProvider,
};
use common::trackb::{
    assert_integrity, corpus, units_hit, Doc, Question, MARKET_QUESTIONS, QUESTIONS,
};
use common::trackb31::MARKET_QUESTIONS_31;
use common::trackb31_docs::market_docs_31;
use common::trackb_holdout::{holdout_docs, HOLDOUT_QUESTIONS};

/// Token budget all treatments must respect (len/4 estimate — the G12
/// convention, same value `knowledge_bench.rs` and Wave 3 use).
const BUDGET: usize = 300;

/// G11 cost convention (comparative_chatbot_bench.rs) — identical so the
/// two benches' cost rows stay comparable.
const INPUT_PRICE_PER_M: f32 = 0.15;
const OUTPUT_PRICE_PER_M: f32 = 0.60;
const ANSWER_TOKENS: usize = 100;

/// Predefined acceptance thresholds (written before first measurement).
const MIN_STRONG_FIT: usize = 1;
const MAX_REGRESSION_UNITS: isize = 2;

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

/// Treatment Graph-RAG's expansion: every chunk naming an entity named by
/// a packed chunk is added, transitively (G11's `graph_expand`).
/// ponytail: expansion is transitive unbounded within the corpus; a real
/// Graph-RAG caps hops/top-N — the corpus bounds it here.
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

/// Ranked (fixture, index) pairs → corpus positions, in rank order.
fn rank_positions(
    corpus: &[common::CorpusChunk],
    q: &str,
    provider: &MockEmbeddingProvider,
) -> Vec<usize> {
    let pos: HashMap<(&str, usize), usize> = corpus
        .iter()
        .enumerate()
        .map(|(p, (f, i, _))| ((*f, *i), p))
        .collect();
    common::rank(corpus, q, provider, false)
        .iter()
        .map(|pair| pos[pair])
        .collect()
}

/// Pack chunk positions in order until the token budget is spent.
fn pack_budgeted(order: &[usize], corpus: &[common::CorpusChunk]) -> String {
    let mut out = String::new();
    for &p in order {
        let text = &corpus[p].2;
        if (out.len() + text.len() + 1) / 4 > BUDGET {
            break;
        }
        out.push_str(text);
        out.push(' ');
    }
    out
}

/// Entity name (lowercased) → corpus positions of chunks mentioning it —
/// the extracted graph's entity→chunk links (G11 convention).
fn entity_chunk_index(
    merged: &KnowledgeIr,
    corpus: &[common::CorpusChunk],
) -> Vec<(String, Vec<usize>)> {
    merged
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
        .collect()
}

/// One task, three treatments, one judge.
#[derive(Default)]
struct TaskRow {
    /// Win-zone units per treatment (0..2, unknown-probe inverted).
    a: usize,
    g: usize,
    r: usize,
    /// Delivered tokens per treatment (len/4).
    at: usize,
    gt: usize,
    rt: usize,
    /// Elapsed micros per treatment.
    am: u128,
    gm: u128,
    rm: u128,
    /// Payload cites at least one corpus doc id (groundedness proxy).
    ag: bool,
    gg: bool,
    rg: bool,
}

fn measure_task(
    q: &Question,
    corpus: &[common::CorpusChunk],
    index: &[(String, Vec<usize>)],
    merged: &KnowledgeIr,
    provider: &MockEmbeddingProvider,
    doc_ids: &HashSet<&str>,
) -> TaskRow {
    let invert = q.kind == "unknown-probe";
    let judge = |payload: &str| {
        let (h, _) = units_hit(payload, q);
        if invert {
            2 - h
        } else {
            h
        }
    };
    // Doc ids tokenize at '-', so a citation shows up as the id's first
    // token ("kb-payments" → "kb") — G11's convention, generalized.
    let cites = |payload: &str| {
        common::tokens(payload)
            .iter()
            .any(|t| doc_ids.contains(t.as_str()))
    };

    let mut row = TaskRow::default();

    let t0 = Instant::now();
    let pkg = compile_context(q.text, merged, BUDGET);
    let aikoql = render_context_markdown(&pkg);
    row.am = t0.elapsed().as_micros();
    row.a = judge(&aikoql);
    row.at = aikoql.len() / 4;
    row.ag = cites(&aikoql);

    let t0 = Instant::now();
    let graph = pack_budgeted(
        &graph_expand(&rank_positions(corpus, q.text, provider), corpus, index),
        corpus,
    );
    row.gm = t0.elapsed().as_micros();
    row.g = judge(&graph);
    row.gt = graph.len() / 4;
    row.gg = cites(&graph);

    let t0 = Instant::now();
    let rag = pack_budgeted(&rank_positions(corpus, q.text, provider), corpus);
    row.rm = t0.elapsed().as_micros();
    row.r = judge(&rag);
    row.rt = rag.len() / 4;
    row.rg = cites(&rag);

    row
}

#[derive(Default, Clone, Copy)]
struct Class {
    a: usize,
    g: usize,
    r: usize,
    max: usize,
    at: usize,
    gt: usize,
    rt: usize,
    n: usize,
}

#[derive(Default)]
struct Totals {
    units: [usize; 3],
    grounded: [usize; 3],
    tokens: [usize; 3],
    micros: [Vec<u128>; 3],
}

fn cost(tokens: usize, queries: usize) -> f32 {
    tokens as f32 / 1e6 * INPUT_PRICE_PER_M
        + (queries * ANSWER_TOKENS) as f32 / 1e6 * OUTPUT_PRICE_PER_M
}

/// p50/p95 of sorted micros (nearest-rank).
fn pct(micros: &[u128], p: f64) -> u128 {
    if micros.is_empty() {
        return 0;
    }
    micros[((micros.len() - 1) as f64 * p).round() as usize]
}

/// Run the three-way treatment + judge over a corpus, print the class
/// table and totals, and return the class rollup for assertions.
fn run_comparison(
    label: &str,
    docs: &[Doc],
    questions: &[&Question],
    assert_thresholds: bool,
) -> BTreeMap<&'static str, Class> {
    let provider = MockEmbeddingProvider::new();
    let corpus = corpus(docs);
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    assert_integrity(docs, &merged);
    let index = entity_chunk_index(&merged, &corpus);
    let doc_ids: HashSet<&str> = docs
        .iter()
        .map(|d| d.id.split('-').next().unwrap_or(d.id))
        .collect();

    let mut classes: BTreeMap<&'static str, Class> = BTreeMap::new();
    let mut totals = Totals::default();

    for (qi, q) in questions.iter().enumerate() {
        let row = measure_task(q, &corpus, &index, &merged, &provider, &doc_ids);
        eprintln!(
            "[W31-COMP {label} Q{qi} {} {}] aikoql={}/2 graphrag={}/2 rag={}/2 \
             tokens a/g/r={}/{}/{}",
            q.class, q.kind, row.a, row.g, row.r, row.at, row.gt, row.rt
        );
        let c = classes.entry(q.class).or_default();
        c.a += row.a;
        c.g += row.g;
        c.r += row.r;
        c.max += 2;
        c.at += row.at;
        c.gt += row.gt;
        c.rt += row.rt;
        c.n += 1;
        totals.units[0] += row.a;
        totals.units[1] += row.g;
        totals.units[2] += row.r;
        totals.grounded[0] += row.ag as usize;
        totals.grounded[1] += row.gg as usize;
        totals.grounded[2] += row.rg as usize;
        totals.tokens[0] += row.at;
        totals.tokens[1] += row.gt;
        totals.tokens[2] += row.rt;
        totals.micros[0].push(row.am);
        totals.micros[1].push(row.gm);
        totals.micros[2].push(row.rm);
    }

    let mut strong_fit = 0usize;
    let mut worst_regression = 0isize;
    eprintln!("[W31-COMP {label}] class table:");
    for (class, c) in &classes {
        let a_frac = c.a as f64 / c.max as f64;
        let verdict = if c.a > c.r {
            if a_frac >= 0.75 {
                strong_fit += 1;
                "Strong Fit"
            } else {
                "Good Fit"
            }
        } else if c.a == c.r {
            if c.max == 0 || c.a == 0 {
                "Unknown"
            } else {
                "Parity"
            }
        } else {
            "Poor Fit"
        };
        worst_regression = worst_regression.max(c.r as isize - c.a as isize);
        eprintln!(
            "  {class}: aikoql {}/{max} ({frac:.2}) graphrag {g}/{max} rag {r}/{max} \
             — Δa-r {delta} — tokens a/g/r {at}/{gt}/{rt} per {n}q — {verdict}",
            c.a,
            frac = a_frac,
            max = c.max,
            g = c.g,
            r = c.r,
            delta = c.a as isize - c.r as isize,
            at = c.at / c.n,
            gt = c.gt / c.n,
            rt = c.rt / c.n,
            n = c.n,
        );
    }
    let sorted: Vec<Vec<u128>> = totals
        .micros
        .into_iter()
        .map(|mut v| {
            v.sort();
            v
        })
        .collect();
    for (t, name) in ["aikoql", "graphrag", "rag"].iter().enumerate() {
        eprintln!(
            "[W31-COMP {label}] {name}: units {}/{} grounded {}/{} tokens {} p50 {}µs p95 {}µs cost ${:.4}",
            totals.units[t],
            questions.len() * 2,
            totals.grounded[t],
            totals.units[t],
            totals.tokens[t],
            pct(&sorted[t], 0.50),
            pct(&sorted[t], 0.95),
            cost(totals.tokens[t], questions.len()),
        );
    }
    eprintln!(
        "[W31-COMP {label}] tool calls / LLM calls: 0/0 all treatments \
         (mechanical proxy, no generative step — real-model leg is REAL-001 #162)"
    );

    if assert_thresholds {
        // Control: W1 is lookup — lexical retrieval must suffice, so both
        // treatments must tie at full marks or the bench is rigged.
        let w1 = classes.get("W1").copied().unwrap_or_default();
        assert_eq!(
            (w1.a, w1.r),
            (w1.max, w1.max),
            "W1 control class must be full parity, got aikoql {} rag {}",
            w1.a,
            w1.r
        );
        // Predefined advantage: ≥1 Strong Fit class (spec COMP-001).
        assert!(
            strong_fit >= MIN_STRONG_FIT,
            "W31-COMP-001 FAILED: no Strong Fit class (strong_fit={strong_fit})"
        );
        // Predefined regression bound (spec COMP-001).
        assert!(
            worst_regression <= MAX_REGRESSION_UNITS,
            "W31-COMP-001 FAILED: a class regresses {worst_regression} units vs RAG (bound {MAX_REGRESSION_UNITS})"
        );
        eprintln!(
            "[W31-COMP-001] verdict: strong_fit={strong_fit} worst_regression={worst_regression} \
             control_parity=true"
        );
    }

    classes
}

/// W31-COMP-001 — three-way comparison on the 148-task union corpus with
/// the predefined acceptance thresholds above.
#[test]
fn w31_comp_001_three_way_comparison() {
    let docs = union_docs();
    let questions = union_questions();
    assert!(
        questions.len() >= 100,
        "comparison corpus has {} tasks, need ≥100",
        questions.len()
    );
    run_comparison("001", &docs, &questions, true);
}

/// W31-COMP-002 — the frozen holdout pass (spec §7): same machinery, same
/// judge, printed once and pinned into the evidence docs. No scoring
/// threshold may ever live here — that would leak holdout signal into
/// development.
#[test]
fn w31_comp_002_holdout_evaluation() {
    let docs = holdout_docs();
    let questions: Vec<&Question> = HOLDOUT_QUESTIONS.iter().collect();
    run_comparison("HO", &docs, &questions, false);
    eprintln!(
        "[W31-HO] holdout pass complete: {} docs, {} tasks",
        docs.len(),
        questions.len()
    );
}
