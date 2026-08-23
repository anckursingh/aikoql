//! G10 (TESTING-PLAN flagship, agent suite §31/AGENT-001..005): the agent
//! efficacy benchmark — four agent treatments over the same repo-derived
//! corpus and task set, measured success / tokens / calls / latency / cost:
//!
//! - **A: repo-only agent** — the LLM gets the question alone (no memory,
//!   no retrieval); answers from its own weights.
//! - **B: LLM + RAG memory** — lexical top chunks (`common::rank`, the
//!   G12/G11 baseline) as numbered evidence, packed to the evidence budget.
//! - **C: LLM + code graph** — the mechanical Graph-RAG flavor: the
//!   lexical top-3 seeds entity-mention chunk expansion (the extracted
//!   graph's entity→chunk links, the G11 treatment C), seed + expansion
//!   packed to the evidence budget.
//! - **D: AIKOQL** — the merged knowledge IR compiled for the task and
//!   rendered; the deterministic path answers without an LLM call
//!   (SEM-003).
//!
//! Corpus: every `docs/*.md` compiled through the real Markdown pipeline
//! (`compile_markdown_file`, A1 — parse → AST → MarkdownSemanticAnalyzer →
//! KnowledgeIr), merged into one graph (A3). The RAG treatments read the
//! same documents as heading-split chunks (a deterministic, naive section
//! chunker — the baseline a keyword retriever would serve).
//!
//! Tasks: 20 engineering questions across the AGENT-001 (where-to-
//! implement), AGENT-002 (change impact), and AGENT-004 (historical
//! explanation) shapes, each with one short golden answer phrase. Corpus
//! integrity asserts every golden phrase is verbatim in a chunk of some
//! document — A/B/C can in principle deliver it; D's reach depends on
//! extraction (that dependence is the measurement).
//!
//! Judge (mechanical, the PR-R convention — no LLM judge): the agent
//! output contains the golden phrase's tokens with at most one missing
//! (small local models paraphrase; exact match would measure phrasing).
//! Hardened for short goldens: 1-2 token goldens require every token.
//! For D the output is the compiled context itself — the deterministic
//! MCP answer path. Generation failures score 0 and are counted, never
//! silently guessed (§58).
//!
//! Live model (the answer_gen convention): `AIKOQL_ANSWER_MODEL` +
//! optional `AIKOQL_ANSWER_ENDPOINT` (Ollama-compatible). Without the
//! model the test SKIPs. Score gates are NOT pinned — a model's answers
//! are what they are, and CI never runs a model; the verdict is printed,
//! not enforced (PR-R). Structural asserts (budget, corpus integrity)
//! run unconditionally.
//!
//! Scope note: v1 measures the knowledge tier of the agent stack over the
//! docs corpus. The full code-tree ingest (v0.3 dogfood) and the
//! AGENT-003 (implement a feature) / AGENT-005 (safe execution) scenarios
//! need agent loops + program execution — deferred to a follow-up corpus
//! swap and harness extension.

#![cfg(feature = "answer_gen")]

mod common;

use aikoql_ingestion::{
    compile_context, compile_markdown_file, merge_knowledge_ir, render_context_markdown,
    KnowledgeIr, MockEmbeddingProvider,
};
use std::collections::HashSet;
use std::time::Instant;

/// The repo's documentation tree — the agent's knowledge corpus.
const DOCS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs");
/// Token budget the context treatments respect (len/4 estimate, the G12
/// convention).
const BUDGET: usize = 500;
/// C's expansion: seed + graph-reached chunks, ordered and then packed to
/// the evidence budget (below).
const SEED_K: usize = 3;
const EXPAND_CAP: usize = 6;
/// Hard cap per chunk: the heading splitter keeps whole sections, which in
/// these docs can run to tens of KB — a prompt must not drown the model.
const MAX_CHUNK_CHARS: usize = 1500;
/// Total evidence characters handed to B/C (≈1500 tokens at len/4) — fits
/// comfortably inside the local model's 4096 context with the question.
const EVIDENCE_BUDGET_CHARS: usize = 6000;
/// G12 reference rates (USD per 1M tokens) + assumed answer length.
const INPUT_PRICE_PER_M: f32 = 0.15;
const OUTPUT_PRICE_PER_M: f32 = 0.60;
const ANSWER_TOKENS: usize = 100;

struct Task {
    kind: &'static str,
    question: &'static str,
    golden: &'static str,
}

/// 20 engineering tasks, AGENT-001/002/004 shapes. Every golden phrase is
/// verbatim in the docs corpus (asserted below); short phrases keep the
/// paraphrase-tolerant judge meaningful.
const TASKS: &[Task] = &[
    Task {
        kind: "where",
        question: "Which benchmark measures token, latency and cost against a RAG baseline?",
        golden: "comparative_cost_bench.rs",
    },
    Task {
        kind: "where",
        question: "Which bench poses questions where a chunk retriever cannot win structurally?",
        golden: "knowledge_bench.rs",
    },
    Task {
        kind: "where",
        question: "Which test file runs the critical end-to-end scenario for chatbot memory?",
        golden: "mcp_real_world.rs",
    },
    Task {
        kind: "where",
        question: "Which harness measures answer, citation and evidence correctness end to end?",
        golden: "e2e_answer_quality.rs",
    },
    Task {
        kind: "where",
        question: "Which crate package owns the MCP tool definitions?",
        golden: "aikoql-mcp",
    },
    Task {
        kind: "why",
        question: "What does the entity gate require anchored facts to have?",
        golden: "ranked entity",
    },
    Task {
        kind: "why",
        question: "What did keyword scoring leak that made the entity gate a no-op?",
        golden: "question stopwords",
    },
    Task {
        kind: "why",
        question: "What ceiling does the relation boost hit on the depth-2 probe?",
        golden: "single-round boost",
    },
    Task {
        kind: "why",
        question: "What must a Knowledge Object never become?",
        golden: "lossy representation",
    },
    Task {
        kind: "why",
        question: "What does SEM-003 claim about the deterministic answer path?",
        golden: "LLM calls 0",
    },
    Task {
        kind: "why",
        question: "What does AGENT-004 require the answer to cite?",
        golden: "history evidence",
    },
    Task {
        kind: "impact",
        question:
            "If a service module changes, which artifact does the impact chain name after code?",
        golden: "dependencies",
    },
    Task {
        kind: "impact",
        question: "Which table is allowed to contain only measured results?",
        golden: "measured results only",
    },
    Task {
        kind: "impact",
        question: "How many evidence units does each Track-B question require?",
        golden: "2 required evidence units",
    },
    Task {
        kind: "impact",
        question: "What corpus do the four comparative treatments share?",
        golden: "Track-B corpus",
    },
    Task {
        kind: "impact",
        question: "What do the comparative experiment's memory continuity rows point to?",
        golden: "G5 §51 MCP scenarios",
    },
    Task {
        kind: "impact",
        question: "What does the G12 cost column charge per million input tokens?",
        golden: "0.15",
    },
    Task {
        kind: "impact",
        question: "Which two checks surround program execution in AGENT-005?",
        golden: "preconditions",
    },
    Task {
        kind: "impact",
        question: "What does the Graph-RAG treatment expand the lexical seed with?",
        golden: "entity-mention chunk expansion",
    },
    Task {
        kind: "impact",
        question: "What is the code-graph treatment's chunk cap after expansion?",
        golden: "6",
    },
];

/// The PR-R §53 answer-correctness judge: golden key tokens present with at
/// most one missing. Hardened for short goldens (2026-08-23, G10): 1-2
/// token goldens require every token — the ≤1-missing allowance let
/// "history evidence" pass on "history" alone and credited A's
/// token-overlap guess passes. 3+ tokens keep the allowance for paraphrase
/// tolerance. Copied from `e2e_answer_quality.rs` (its unit tests pin the
/// original semantics there); shared-module extraction deferred until a
/// third consumer.
fn answer_correct(answer: &str, golden: &str) -> bool {
    let golden_tokens = common::tokens(golden);
    let answer_tokens = common::tokens(answer);
    let hits = golden_tokens
        .iter()
        .filter(|t| answer_tokens.contains(*t))
        .count();
    let needed = if golden_tokens.len() < 3 {
        golden_tokens.len().max(1)
    } else {
        golden_tokens.len() - 1
    };
    hits >= needed
}

#[test]
fn answer_correct_is_strict_on_short_goldens() {
    // 2-token golden: both tokens required.
    assert!(answer_correct(
        "the history evidence row",
        "history evidence"
    ));
    assert!(!answer_correct("the history row", "history evidence"));
    assert!(!answer_correct("the evidence row", "history evidence"));
    // 1-token golden: the token is required (empty answers never pass).
    assert!(!answer_correct("validation only", "preconditions"));
    assert!(!answer_correct("", "preconditions"));
    // 3+ tokens: at most one missing (paraphrase tolerance kept).
    assert!(answer_correct(
        "2 evidence units required",
        "2 required evidence units"
    ));
    assert!(!answer_correct("2 units", "2 required evidence units"));
}

/// One generation against an Ollama-compatible `/api/chat` endpoint.
/// `Ok(None)` when no answer came back (transport/parse failure). Copied
/// from `e2e_answer_quality.rs` — same 180s stuck-call guard.
fn generate(endpoint: &str, model: &str, system: &str, user: &str) -> Option<String> {
    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "options": { "temperature": 0 },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(180)))
        .build()
        .into();
    agent
        .post(&format!("{}/api/chat", endpoint))
        .header("Content-Type", "application/json")
        .send_json(body)
        .ok()
        .and_then(|resp| resp.into_body().read_json::<serde_json::Value>().ok())
        .and_then(|v| v["message"]["content"].as_str().map(str::to_string))
        .filter(|s| !s.trim().is_empty())
}

/// A naive deterministic section chunker: split on ATX heading lines, then
/// accumulate lines into chunks capped at `MAX_CHUNK_CHARS` so no chunk can
/// drown a context window (tables have no blank lines — paragraph splits
/// can't bound them, line accumulation can). ponytail: no overlap/sliding
/// windows — the point is a baseline a keyword retriever would plausibly
/// serve.
fn split_sections(text: &str) -> Vec<String> {
    let mut sections = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let heading = line.starts_with('#');
        if heading && !current.trim().is_empty() {
            sections.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        sections.push(current);
    }
    let mut chunks = Vec::new();
    for s in sections {
        let mut part = String::new();
        for line in s.lines() {
            if !part.is_empty() && part.len() + line.len() + 1 > MAX_CHUNK_CHARS {
                chunks.push(std::mem::take(&mut part));
            }
            part.push_str(line);
            part.push('\n');
        }
        if !part.trim().is_empty() {
            chunks.push(part);
        }
    }
    chunks
}

/// Pack ranked chunk positions in order until the character budget is
/// spent (always at least the first chunk — a lone oversized chunk is
/// better than an empty prompt).
fn pack_evidence(
    order: &[usize],
    corpus: &[common::CorpusChunk],
    budget_chars: usize,
) -> Vec<String> {
    let mut packed = Vec::new();
    let mut used = 0;
    for &p in order {
        let text = corpus[p].2.clone();
        if !packed.is_empty() && used + text.len() > budget_chars {
            break;
        }
        used += text.len();
        packed.push(text);
    }
    packed
}

/// G11's transitive entity-mention expansion (copied — see
/// `comparative_chatbot_bench.rs`): every chunk naming an entity named by
/// a packed chunk is added until no new chunk arrives.
fn graph_expand(
    seed: &[usize],
    corpus: &[common::CorpusChunk],
    index: &[(String, Vec<usize>)],
    cap: usize,
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
        if !added || order.len() >= cap {
            break;
        }
    }
    order
}

fn render_evidence(evidence: &[String]) -> String {
    evidence
        .iter()
        .enumerate()
        .map(|(i, text)| format!("[{}] {}", i + 1, text.replace('\n', " ")))
        .collect::<Vec<_>>()
        .join("\n\n")
}

struct Stats {
    correct: usize,
    failed: usize,
    tokens: usize,
    calls: usize,
    tools: usize,
    micros: u128,
}

#[test]
fn agent_efficacy_bench() {
    // ── Corpus: every docs/*.md through the real Markdown pipeline ───────
    let mut ids: Vec<String> = std::fs::read_dir(DOCS_DIR)
        .unwrap_or_else(|e| panic!("read {DOCS_DIR}: {e}"))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    ids.sort();
    assert!(!ids.is_empty(), "no markdown docs in {DOCS_DIR}");

    let mut irs: Vec<KnowledgeIr> = Vec::new();
    // (id, chunks) for the RAG treatments; ids leak to 'static (test
    // process — the corpus type wants &'static str).
    let mut doc_chunks: Vec<(&'static str, Vec<String>)> = Vec::new();
    for id in &ids {
        let path = format!("{DOCS_DIR}/{id}");
        let ir = compile_markdown_file(&path, Some(id.clone()), None)
            .unwrap_or_else(|e| panic!("compile {id}: {e}"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        doc_chunks.push((
            Box::leak(id.clone().into_boxed_str()),
            split_sections(&text),
        ));
        irs.push(ir);
    }
    let merged = merge_knowledge_ir(&irs);
    let corpus: Vec<common::CorpusChunk<'static>> = doc_chunks
        .iter()
        .flat_map(|(id, chunks)| chunks.iter().enumerate().map(|(i, c)| (*id, i, c.clone())))
        .collect();
    let max_chunk = corpus.iter().map(|(_, _, c)| c.len()).max().unwrap_or(0);
    eprintln!(
        "[G10 STRUCTURE] docs={} chunks={} max_chunk_chars={max_chunk} merged_entities={} merged_facts={} merged_relations={} budget={BUDGET}",
        ids.len(),
        corpus.len(),
        merged.entities.len(),
        merged.facts.len(),
        merged.relations.len(),
    );

    // ── Corpus integrity: every golden phrase is verbatim in one chunk —
    // A/B/C can in principle retrieve it from the same text.
    for t in TASKS {
        let g = common::tokens(t.golden);
        let chunk_hits = |c: &str| {
            let ct = common::tokens(c);
            g.iter().all(|tok| ct.contains(tok))
        };
        assert!(
            corpus.iter().any(|(_, _, c)| chunk_hits(c))
                || corpus
                    .windows(2)
                    .any(|w| chunk_hits(&format!("{} {}", w[0].2, w[1].2))),
            "task '{}' golden '{:?}' is not verbatim in any chunk (or adjacent pair)",
            t.question,
            t.golden
        );
    }

    // ── AIKOQL_G10_DEBUG=1: D-pack diagnosis (no model needed) ──────────
    // Per task: does the rendered D pack contain the golden token, which
    // facts ranked top (score + justification), and how many IR facts
    // contain the golden at all — separating corpus gaps from
    // entity-gate/ranking misses. Runs before the model check so it
    // works without Ollama.
    if std::env::var("AIKOQL_G10_DEBUG").is_ok() {
        for (qi, t) in TASKS.iter().enumerate() {
            let pkg = compile_context(t.question, &merged, BUDGET);
            let rendered = render_context_markdown(&pkg);
            let hit = answer_correct(&rendered, t.golden);
            let g = common::tokens(t.golden);
            let in_ir = merged
                .facts
                .iter()
                .filter(|f| {
                    let fs = common::tokens(&f.statement);
                    g.iter().all(|tok| fs.contains(tok))
                })
                .count();
            eprintln!(
                "[G10 DEBUG T{qi}] golden={:?} hit={hit} ir_facts_with_golden={in_ir} trimmed={} \
                 ents={} facts={} rels={} est={}",
                t.golden,
                pkg.trimmed,
                pkg.entities.len(),
                pkg.facts.len(),
                pkg.relations.len(),
                pkg.estimated_tokens
            );
            for rf in pkg.facts.iter().take(4) {
                let stmt: String = rf.statement.chars().take(90).collect();
                eprintln!("  {:>4.1} {} [{}]", rf.score, stmt, rf.justification);
            }
            // Where does each golden-carrying fact sit? est tokens, entity
            // boost, and exact question-token overlap (≈ its stmt score).
            let q_tokens: HashSet<String> = common::tokens(t.question);
            for f in merged
                .facts
                .iter()
                .filter(|f| {
                    let fs = common::tokens(&f.statement);
                    g.iter().all(|tok| fs.contains(tok))
                })
                .take(3)
            {
                let fs = common::tokens(&f.statement);
                let overlap = q_tokens
                    .iter()
                    .filter(|w| w.len() >= 3 && fs.contains(*w))
                    .count();
                let boost: f32 = f
                    .entities
                    .iter()
                    .map(|en| {
                        pkg.entities
                            .iter()
                            .find(|e| e.name == *en)
                            .map(|e| e.score * 0.3)
                            .unwrap_or(0.0)
                    })
                    .sum();
                let est =
                    f.statement.len() / 4 + f.entities.iter().map(|e| e.len() / 4).sum::<usize>();
                eprintln!("  golden-fact est_tokens≈{est} overlap={overlap} boost={boost:.2}");
            }
            if !hit {
                for f in merged
                    .facts
                    .iter()
                    .filter(|f| {
                        let fs = common::tokens(&f.statement);
                        g.iter().all(|tok| fs.contains(tok))
                    })
                    .take(4)
                {
                    let stmt: String = f.statement.chars().take(90).collect();
                    eprintln!("  IR-fact '{}' entities={:?}", stmt, f.entities);
                }
            }
        }
        return;
    }

    // ── Live model required for the answer side ──────────────────────────
    let Some(model) = std::env::var("AIKOQL_ANSWER_MODEL").ok() else {
        eprintln!(
            "[G10] SKIP — set AIKOQL_ANSWER_MODEL (and optionally AIKOQL_ANSWER_ENDPOINT) to \
             run the agent efficacy benchmark against a local model"
        );
        return;
    };
    let endpoint = std::env::var("AIKOQL_ANSWER_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());

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
    let pos: std::collections::HashMap<(&str, usize), usize> = corpus
        .iter()
        .enumerate()
        .map(|(p, (f, i, _))| ((*f, *i), p))
        .collect();

    let provider = MockEmbeddingProvider::new();
    let system_with =
        "Answer the question using ONLY the numbered source excerpts below. Cite every \
         source you use with its [n] number. If the sources do not answer the question, \
         answer exactly: not in sources.";
    let system_without = "Answer the question briefly from your own knowledge.";

    let mut a = Stats {
        correct: 0,
        failed: 0,
        tokens: 0,
        calls: 0,
        tools: 0,
        micros: 0,
    };
    let mut b = Stats {
        correct: 0,
        failed: 0,
        tokens: 0,
        calls: 0,
        tools: 0,
        micros: 0,
    };
    let mut c = Stats {
        correct: 0,
        failed: 0,
        tokens: 0,
        calls: 0,
        tools: 0,
        micros: 0,
    };
    let mut d = Stats {
        correct: 0,
        failed: 0,
        tokens: 0,
        calls: 0,
        tools: 0,
        micros: 0,
    };

    let started = Instant::now();
    for (qi, t) in TASKS.iter().enumerate() {
        // ── A: repo-only — the question alone ─────────────────────────────
        let t0 = Instant::now();
        let a_answer = generate(&endpoint, &model, system_without, t.question);
        let a_hit = a_answer
            .as_deref()
            .map(|s| answer_correct(s, t.golden))
            .unwrap_or(false);
        a.correct += usize::from(a_hit);
        a.failed += usize::from(a_answer.is_none());
        a.tokens += (system_without.len() + t.question.len()) / 4;
        a.calls += 1;
        a.micros += t0.elapsed().as_micros();

        // ── B: RAG memory — lexical top-K evidence ────────────────────────
        let t0 = Instant::now();
        let ranked = common::rank(&corpus, t.question, &provider, false);
        let order_b: Vec<usize> = ranked.iter().map(|(f, i)| pos[&(*f, *i)]).collect();
        let evidence = pack_evidence(&order_b, &corpus, EVIDENCE_BUDGET_CHARS);
        let user_b = format!(
            "Question: {}\n\nSources:\n{}",
            t.question,
            render_evidence(&evidence)
        );
        let b_answer = generate(&endpoint, &model, system_with, &user_b);
        let b_hit = b_answer
            .as_deref()
            .map(|s| answer_correct(s, t.golden))
            .unwrap_or(false);
        b.correct += usize::from(b_hit);
        b.failed += usize::from(b_answer.is_none());
        b.tokens += (system_with.len() + user_b.len()) / 4;
        b.calls += 1;
        b.micros += t0.elapsed().as_micros();

        // ── C: code graph — seed + entity-mention expansion ──────────────
        let t0 = Instant::now();
        let seed: Vec<usize> = ranked
            .iter()
            .take(SEED_K)
            .map(|(f, i)| pos[&(*f, *i)])
            .collect();
        let expanded = graph_expand(&seed, &corpus, &index, EXPAND_CAP);
        let evidence = pack_evidence(&expanded, &corpus, EVIDENCE_BUDGET_CHARS);
        let user_c = format!(
            "Question: {}\n\nSources:\n{}",
            t.question,
            render_evidence(&evidence)
        );
        let c_answer = generate(&endpoint, &model, system_with, &user_c);
        let c_hit = c_answer
            .as_deref()
            .map(|s| answer_correct(s, t.golden))
            .unwrap_or(false);
        c.correct += usize::from(c_hit);
        c.failed += usize::from(c_answer.is_none());
        c.tokens += (system_with.len() + user_c.len()) / 4;
        c.calls += 1;
        c.micros += t0.elapsed().as_micros();

        // ── D: AIKOQL — deterministic compile, no LLM call ───────────────
        let t0 = Instant::now();
        let pkg = compile_context(t.question, &merged, BUDGET);
        assert!(
            pkg.estimated_tokens <= BUDGET,
            "{}: aikoql package exceeded the budget: {} > {BUDGET}",
            t.question,
            pkg.estimated_tokens
        );
        let delivered = render_context_markdown(&pkg);
        let d_hit = answer_correct(&delivered, t.golden);
        d.correct += usize::from(d_hit);
        d.tokens += delivered.len() / 4;
        d.tools += 1;
        d.micros += t0.elapsed().as_micros();

        eprintln!(
            "[G10 T{qi} {} {:?} golden={:?}] A={} ({:?}) B={} ({:?}) C={} ({:?}) D={} tokens={}/{}/{}/{}",
            t.kind,
            t.question,
            t.golden,
            a_hit,
            a_answer.as_deref().map(|s| s.replace('\n', " ")),
            b_hit,
            b_answer.as_deref().map(|s| s.replace('\n', " ")),
            c_hit,
            c_answer.as_deref().map(|s| s.replace('\n', " ")),
            d_hit,
            (system_without.len() + t.question.len()) / 4,
            (system_with.len() + user_b.len()) / 4,
            (system_with.len() + user_c.len()) / 4,
            delivered.len() / 4,
        );
    }

    let n = TASKS.len();
    let table = [&a, &b, &c, &d];
    eprintln!(
        "[G10-AGENT SUMMARY] model={model} tasks={n} wall={:.1}s",
        started.elapsed().as_secs_f32()
    );
    eprintln!(
        "[G10-AGENT TABLE] metric | A: repo-only | B: LLM + RAG | C: LLM + code graph | D: AIKOQL"
    );
    for (label, cells) in [
        (
            "Success rate",
            table.map(|s| format!("{:.3}", s.correct as f32 / n as f32)),
        ),
        (
            "Input tokens (mean)",
            table.map(|s| format!("{:.1}", s.tokens as f32 / n as f32)),
        ),
        ("LLM calls", table.map(|s| format!("{}", s.calls))),
        ("Tool calls", table.map(|s| format!("{}", s.tools))),
        (
            "Latency s/query",
            table.map(|s| format!("{:.1}", s.micros as f32 / n as f32 / 1e6)),
        ),
        (
            "Cost USD/query",
            table.map(|s| {
                format!(
                    "{:.6}",
                    s.tokens as f32 / 1e6 * INPUT_PRICE_PER_M
                        + (s.calls * ANSWER_TOKENS) as f32 / 1e6 * OUTPUT_PRICE_PER_M
                )
            }),
        ),
        ("Failed generations", table.map(|s| format!("{}", s.failed))),
    ] {
        eprintln!(
            "[G10-AGENT TABLE] {label} | {} | {} | {} | {}",
            cells[0], cells[1], cells[2], cells[3]
        );
    }

    // ── Structural gates only: the corpus must be usable and the
    // deterministic treatment honest. Answer scores are printed, not
    // enforced (a model's answers are what they are; CI never runs one).
    assert!(
        merged.entities.len() >= 100,
        "merged graph too small to be a real corpus"
    );
}
