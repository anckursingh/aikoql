//! Wave 3 (AIKOQL_Wave3_Market_Reality_TDD_Test_Plan_v2) — market-reality
//! experiments, deterministic (no LLM, CI-reproducible, the G12 convention).
//! Wave 3 is product-evidence work, NOT new substrate features: every
//! experiment runs the existing kernel/compiler machinery over the
//! market-extended corpus and measures the product claims.
//!
//! Instruments in this file:
//! - `w3_mkt_001_market_corpus_integrity` — W3-G02: the market corpus is
//!   versioned (git), fair (every answer unit verbatim-backed), and labeled
//!   with the §8 workload classes.
//! - `w3_win_001_workload_classification` — W3-WIN-001: both mechanical
//!   treatments (AikoQL compile+render vs lexical RAG pack, the
//!   `knowledge_bench.rs` machinery) over the Track-B + market union,
//!   rolled up per workload class into Strong Fit / Good Fit / Parity /
//!   Poor Fit / Unknown. W3-G04 (≥1 repeatable strong-fit class) is
//!   asserted.
//!
//! - `w3_temp_001_temporal_market_reality` — W3-TEMP-001: the full kernel
//!   timeline (assert → supersede → supersede) fed to the compiler's
//!   validity boundary vs the stateless RAG pack.
//! - `w3_unk_001_unknown_handling_classification` — W3-UNK-001: known /
//!   unknown (healthy empty) / conflicting (KNOW-007) / historical-only,
//!   plus the measured false-confidence probe.
//! - `w3_conf_001_contradiction_value` — W3-CONF-001: policy supersedes the
//!   unsafe issue-note and outdated ADR; the pack carries only the policy.
//! - `w3_long_001_longitudinal_value` — W3-LONG-001: 90-day capacity
//!   evolution under three treatments (stateless RAG, conversation history,
//!   AikoQL validity boundary).
//! - `w3_debug_001_observability_root_cause` — W3-DEBUG-001: five injected
//!   failures, each surfaced by a deterministic kernel read.
//!
//! The kernel-state experiments live here, not in
//! `crates/kernel/tests/`, because they need kernel ops (supersede,
//! relate, explain) AND the compiler in one binary — kernel tests cannot
//! depend on `aikoql-ingestion`.
//!
//! Negative evidence is MANDATORY (plan §29): probes whose measured outcome
//! is parity or a loss are kept and reported, never dropped.

mod common;

use aikoql_ingestion::{
    compile_context, compile_context_with_validity, merge_knowledge_ir, render_context_markdown,
    ContextPackage, EntityCandidate, FactCandidate, KnowledgeIr, MockEmbeddingProvider,
    RetrievalStatus,
};
use common::trackb::{
    assert_integrity, corpus, docs, market_docs, units_hit, Doc, Question, MARKET_QUESTIONS,
    QUESTIONS,
};

/// Token budget both treatments must respect (len/4 estimate — the G12
/// convention; same value `knowledge_bench.rs` uses).
const BUDGET: usize = 300;

/// The union question set, in a fixed order: the pinned Track-B questions
/// first, then the market extension.
fn all_questions() -> Vec<&'static Question> {
    QUESTIONS.iter().chain(MARKET_QUESTIONS.iter()).collect()
}

fn all_docs() -> Vec<Doc> {
    docs().into_iter().chain(market_docs()).collect()
}

/// W3-MKT-001 / W3-G02 — market corpus integrity. Fairness (RAG could in
/// principle retrieve every unit — verbatim backing) + the workload-class
/// labels the win-zone rollup depends on. Document-id units (the W7
/// provenance question) are backed by IR evidence ids instead — they exist
/// precisely because no chunk text carries them.
#[test]
fn w3_mkt_001_market_corpus_integrity() {
    let docs = all_docs();
    let chunks = corpus(&docs);
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    assert_integrity(&docs, &merged);

    let doc_ids: Vec<&str> = docs.iter().map(|d| d.id).collect();
    let all_chunks: String = chunks
        .iter()
        .map(|(_, _, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for (i, q) in all_questions().iter().enumerate() {
        for unit in q.units {
            if doc_ids.contains(&unit) {
                // A doc-id unit must be backed by evidence, not chunk text.
                assert!(
                    merged
                        .entities
                        .iter()
                        .map(|e| &e.evidence)
                        .chain(merged.facts.iter().map(|f| &f.evidence))
                        .chain(merged.relations.iter().map(|r| &r.evidence))
                        .any(|ev| ev.document_id.as_deref() == Some(unit)),
                    "provenance unit '{unit}' has no IR evidence backing (question {i})"
                );
            } else {
                assert!(
                    chunk_tokens_back(&all_chunks, unit),
                    "unit '{unit}' of question {i} has no verbatim backing chunk — \
                     expected evidence missing"
                );
            }
        }
        assert!(
            q.class.starts_with('W')
                && q.class[1..]
                    .parse::<u8>()
                    .is_ok_and(|n| (1..=12).contains(&n)),
            "question {i} lacks a W1-W12 workload class label"
        );
    }

    // Class coverage: every static-corpus class the plan's taxonomy (§8)
    // can carry must be present. W8 (personal memory) is the §32 memory
    // bench, W10 (agent planning) needs agent loops (out of substrate
    // scope), W12 (longitudinal) is the kernel W3-LONG-001 experiment.
    let covered: std::collections::BTreeSet<&str> =
        all_questions().iter().map(|q| q.class).collect();
    for needed in ["W1", "W2", "W3", "W4", "W5", "W6", "W7", "W9", "W11"] {
        assert!(
            covered.contains(needed),
            "workload class {needed} has no market question"
        );
    }
    assert!(
        all_questions().len() >= 10,
        "market question set too small: {}",
        all_questions().len()
    );
    eprintln!(
        "[W3-MKT-001] market corpus: {} docs / {} chunks / {} questions across classes {:?}",
        docs.len(),
        chunks.len(),
        all_questions().len(),
        covered,
    );
}

/// Token-containment backing check: every content token of `unit` appears
/// in `all_chunks` (the same verbatim-backing rule assert_integrity uses).
fn chunk_tokens_back(all_chunks: &str, unit: &str) -> bool {
    let pool = common::tokens(all_chunks);
    common::tokens(unit).iter().all(|t| pool.contains(t))
}

/// W3-WIN-001 — per-workload-class comparison and classification.
/// Treatments: AikoQL (merged-IR compile + render) vs the lexical RAG pack,
/// exactly `knowledge_bench.rs`. Per class: Σ delivered units, mean
/// delivered tokens, Δ, and the Strong Fit / Good Fit / Parity / Poor Fit /
/// Unknown verdict. W3-G04 asserted: at least one Strong Fit class.
#[test]
fn w3_win_001_workload_classification() {
    let provider = MockEmbeddingProvider::new();
    let docs = all_docs();
    let corpus = corpus(&docs);
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    assert_integrity(&docs, &merged);
    let questions = all_questions();

    // Per-class rollups: (aikoql score, rag score, max, aikoql tokens, rag
    // tokens, n questions). BTreeMap = deterministic iteration order.
    let mut classes: std::collections::BTreeMap<&str, (usize, usize, usize, usize, usize, usize)> =
        std::collections::BTreeMap::new();

    for (qi, q) in questions.iter().enumerate() {
        // ── AikoQL treatment ──────────────────────────────────────────────
        let pkg = compile_context(q.text, &merged, BUDGET);
        let delivered = render_context_markdown(&pkg);
        let a_tokens = delivered.len() / 4;

        // ── RAG baseline treatment ────────────────────────────────────────
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
        let r_tokens = packed_text.len() / 4;

        // ── Judge (the knowledge_bench token-containment judge) ──────────
        let (a_hits, _) = units_hit(&delivered, q);
        let (r_hits, _) = units_hit(&packed_text, q);
        // unknown-probe inverts: the units are traps — delivering them is
        // false confidence, so the correct payload scores 2/2.
        let (a_score, r_score) = if q.kind == "unknown-probe" {
            (2 - a_hits, 2 - r_hits)
        } else {
            (a_hits, r_hits)
        };

        let e = classes.entry(q.class).or_insert((0, 0, 0, 0, 0, 0));
        e.0 += a_score;
        e.1 += r_score;
        e.2 += 2;
        e.3 += a_tokens;
        e.4 += r_tokens;
        e.5 += 1;

        eprintln!(
            "[W3-WIN Q{qi} {class} {kind}] aikoql={a_score}/2 rag={r_score}/2 \
             aikoql_tokens={a_tokens} rag_tokens={r_tokens}",
            class = q.class,
            kind = q.kind,
        );
    }

    // The control question must tie (Q5: both treatments deliver both
    // units) or the bench is rigged in AikoQL's favor.
    if let Some((a, r, mx, ..)) = classes.get("W1") {
        assert_eq!(
            (*a, *r),
            (*mx, *mx),
            "control class W1 must be full parity, got aikoql {a} rag {r}"
        );
    }

    // ── Classification + verdict ─────────────────────────────────────────
    let mut strong_fit = 0usize;
    eprintln!("[W3-WIN-001] workload class table:");
    for (class, (a, r, mx, at, rt, n)) in &classes {
        let a_frac = *a as f64 / *mx as f64;
        let verdict = if a > r {
            if a_frac >= 0.75 {
                strong_fit += 1;
                "Strong Fit"
            } else {
                "Good Fit"
            }
        } else if a == r {
            if *mx == 0 || *a == 0 {
                "Unknown"
            } else {
                "Parity"
            }
        } else {
            "Poor Fit"
        };
        eprintln!(
            "  {class}: aikoql {a}/{mx} ({:.2}) vs rag {r}/{mx} — Δ {} — \
             tokens {}/{n}q vs {}/{n}q — {verdict}",
            a_frac,
            *a as isize - *r as isize,
            at / n,
            rt / n,
        );
    }

    // W3-G04 — at least one important workload class must show a repeatable
    // advantage. The multi-hop class is the structural one (zero-overlap
    // answer facts the chunk retriever cannot rank); if this fails, the
    // product thesis is broken and the release gate must block.
    assert!(
        strong_fit >= 1,
        "W3-G04 FAILED: no workload class shows a repeatable AikoQL advantage \
         (strong_fit={strong_fit})"
    );
}

// ---------------------------------------------------------------------------
// W3-P0/P1 kernel-state experiments.
//
// These need real kernel ops (assert/supersede/contradict/relate/explain)
// AND the compiler's validity boundary — hence this file: crates/kernel
// tests cannot depend on aikoql-ingestion.
// ---------------------------------------------------------------------------

use aikoql_graph::{GraphEngineApi, RelateRequest};
use aikoql_kernel::*;
use std::collections::HashSet;
use std::sync::Arc;

/// Deterministic kernel + its manual clock (the experiments drive time).
fn mk() -> (Kernel, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(0));
    let kernel = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0xC0FFEE).unwrap();
    (kernel, clock)
}

fn alice() -> KnowledgeContext {
    KnowledgeContext::new(Subject::new("alice"))
}

fn ev(src: &str) -> Evidence {
    Evidence::new(src, EvidenceMethod::DocExtraction)
}

fn props(pairs: &[(&str, &str)]) -> PropertyMap {
    let mut m = PropertyMap::new();
    for (k, v) in pairs {
        m.insert((*k).into(), Value::Text((*v).into()));
    }
    m
}

/// Assert `properties` on explicit `authority`, return the claim KOID.
fn assert_claim(
    k: &Kernel,
    type_name: &str,
    properties: PropertyMap,
    authority: &str,
    src: &str,
) -> KOID {
    let mut req = AssertionRequest::new(alice(), type_name);
    req.properties = properties;
    req.authority = Some(authority.into());
    req.evidence = vec![ev(src)];
    k.assert_knowledge(req).unwrap().koid
}

/// Supersede `old` with a new-generation claim, return the new KOID.
fn supersede_claim(
    k: &Kernel,
    old: KOID,
    properties: PropertyMap,
    reason: &str,
    src: &str,
) -> KOID {
    let mut req = SupersedeRequest::new(alice(), old, "Claim");
    req.properties = properties;
    req.reason = Some(reason.into());
    req.evidence = vec![ev(src)];
    k.supersede(req).unwrap().new
}

/// One hand-built corpus chunk (the kernel-experiment corpora).
fn chunk<'a>(fixture: &'a str, index: usize, text: &str) -> common::CorpusChunk<'a> {
    (fixture, index, text.to_string())
}

/// The kernel-computed stale set: every claim whose KO is superseded
/// (valid_to set) contributes its fact statement key — the contract
/// `compile_context_with_validity` consumes. Statements come from the
/// caller because they are the claim's rendered text.
fn kernel_stale(k: &Kernel, claims: &[(KOID, &str)]) -> HashSet<String> {
    let mut stale = HashSet::new();
    for (koid, statement) in claims {
        if k.get(alice(), koid).unwrap().valid_to().is_some() {
            stale.insert(format!("f:{statement}"));
        }
    }
    stale
}

fn entity(name: &str, type_hint: &str, mentions: &[&str]) -> EntityCandidate {
    EntityCandidate {
        name: name.into(),
        type_hint: Some(type_hint.into()),
        mentions: mentions.iter().map(|m| (*m).into()).collect(),
        confidence: 0.8,
        // The IR evidence type (distinct from the kernel's — both are in
        // scope via the kernel glob).
        evidence: aikoql_ingestion::Evidence::default(),
    }
}

fn fact(statement: &str, entities: &[&str]) -> FactCandidate {
    FactCandidate {
        statement: statement.into(),
        entities: entities.iter().map(|e| (*e).into()).collect(),
        confidence: 0.8,
        evidence: aikoql_ingestion::Evidence::default(),
        snippet: None,
    }
}

/// The RAG baseline: rank `chunks` lexically for `query`, pack until BUDGET.
fn rag_pack(
    query: &str,
    chunks: &[common::CorpusChunk],
    provider: &MockEmbeddingProvider,
) -> String {
    let mut packed = String::new();
    for (f, i) in common::rank(chunks, query, provider, false) {
        let text = common::chunk_text(chunks, f, i);
        if (packed.len() + text.len() + 1) / 4 > BUDGET {
            break;
        }
        packed.push_str(text);
        packed.push(' ');
    }
    packed
}

/// The bench judge: every content token of `needle` appears in `payload`.
fn payload_has(payload: &str, needle: &str) -> bool {
    let pool = common::tokens(payload);
    common::tokens(needle).iter().all(|t| pool.contains(t))
}

/// Statements delivered by a compiled package.
fn pkg_facts(pkg: &ContextPackage) -> Vec<&str> {
    pkg.facts.iter().map(|f| f.statement.as_str()).collect()
}

/// W3-TEMP-001 — temporal market reality. G11 measured temporal accuracy
/// 0.0 for every treatment: no retrieval path suppresses superseded
/// claims. Here the full kernel timeline runs (assert → supersede →
/// supersede) and the compiler validity boundary is the treatment. The
/// historical incident fact must SURVIVE (still-true history about a
/// superseded entity); the superseded claims must NOT.
#[test]
fn w3_temp_001_temporal_market_reality() {
    let (k, clock) = mk();
    let day = 86_400_000u64;
    let t0 = 1_767_225_600_000u64; // 2026-01-01T00:00:00Z
    let feb15 = t0 + 45 * day;
    let mar1 = t0 + 59 * day;
    let jun1 = t0 + 151 * day;

    clock.set(t0);
    let v1 = assert_claim(
        &k,
        "Claim",
        props(&[("architecture", "ArchV1")]),
        "architecture_decision",
        "kb/arch.md",
    );
    clock.set(feb15);
    let incident = assert_claim(
        &k,
        "Incident",
        props(&[
            ("incident", "FebruaryOutage"),
            ("architecture", "ArchV1"),
            ("summary", "payments outage"),
        ]),
        "deployment_observed",
        "kb/incident.md",
    );
    clock.set(mar1);
    let v2 = supersede_claim(
        &k,
        v1,
        props(&[("architecture", "ArchV2")]),
        "capacity migration",
        "kb/arch.md",
    );
    clock.set(jun1);
    let v3 = supersede_claim(
        &k,
        v2,
        props(&[("architecture", "ArchV3")]),
        "multi-region move",
        "kb/arch.md",
    );

    // Kernel history: each superseded generation is stamped valid_to, the
    // current one is open, the incident record is untouched.
    assert_eq!(k.get(alice(), &v1).unwrap().valid_to(), Some(mar1));
    assert_eq!(k.get(alice(), &v2).unwrap().valid_to(), Some(jun1));
    assert_eq!(k.get(alice(), &v3).unwrap().valid_to(), None);
    assert_eq!(k.get(alice(), &incident).unwrap().valid_to(), None);
    assert!(!k.trace(alice(), &v1).unwrap().versions.is_empty());

    let ir = KnowledgeIr {
        entities: vec![
            entity("ArchV1", "Architecture", &[]),
            entity("ArchV2", "Architecture", &[]),
            entity("ArchV3", "Architecture", &[]),
            entity("FebruaryOutage", "Incident", &["payments outage"]),
        ],
        facts: vec![
            fact("ArchV1 is the deployed architecture.", &["ArchV1"]),
            fact("ArchV2 is the deployed architecture.", &["ArchV2"]),
            fact("ArchV3 is the deployed architecture.", &["ArchV3"]),
            fact(
                "The FebruaryOutage happened while ArchV1 was deployed.",
                &["FebruaryOutage", "ArchV1"],
            ),
        ],
        ..Default::default()
    };

    // Stale set computed from kernel state: superseded claim KOs key their
    // statements + the entities they introduced.
    let mut stale = kernel_stale(
        &k,
        &[
            (v1, "ArchV1 is the deployed architecture."),
            (v2, "ArchV2 is the deployed architecture."),
        ],
    );
    stale.insert("e:ArchV1".to_string());
    stale.insert("e:ArchV2".to_string());

    let task = "Which architecture was deployed during the FebruaryOutage?";
    let pkg = compile_context_with_validity(task, &ir, BUDGET, None, &stale);
    assert_eq!(pkg.status, RetrievalStatus::Healthy);
    let facts = pkg_facts(&pkg);
    let names: Vec<&str> = pkg.entities.iter().map(|e| e.name.as_str()).collect();

    // The incident fact survives (history about a superseded entity is
    // still true) and the current claim is delivered...
    assert!(
        facts.contains(&"The FebruaryOutage happened while ArchV1 was deployed."),
        "historical incident fact must survive: {facts:?}"
    );
    assert!(
        facts.contains(&"ArchV3 is the deployed architecture."),
        "current claim missing: {facts:?}"
    );
    // ...while both superseded claims are suppressed.
    assert!(
        !facts.contains(&"ArchV1 is the deployed architecture."),
        "superseded v1 claim leaked: {facts:?}"
    );
    assert!(
        !facts.contains(&"ArchV2 is the deployed architecture."),
        "superseded v2 claim leaked: {facts:?}"
    );
    assert!(
        names.contains(&"ArchV3")
            && names.contains(&"FebruaryOutage")
            && !names.contains(&"ArchV1")
            && !names.contains(&"ArchV2"),
        "entity boundary wrong: {names:?}"
    );

    // RAG baseline: the accumulated doc chunks all rank (shared tokens) —
    // the stateless retriever packs the superseded claims. Temporal
    // accuracy 0, the G11 measured row reproduced here.
    let provider = MockEmbeddingProvider::new();
    let chunks: Vec<common::CorpusChunk> = vec![
        chunk("kb/arch.md", 0, "ArchV1 is the deployed architecture."),
        chunk("kb/arch.md", 1, "ArchV2 is the deployed architecture."),
        chunk("kb/arch.md", 2, "ArchV3 is the deployed architecture."),
        chunk(
            "kb/incident.md",
            0,
            "The FebruaryOutage happened while ArchV1 was deployed.",
        ),
    ];
    let rag = rag_pack(task, &chunks, &provider);
    let rag_stale = [
        payload_has(&rag, "ArchV1 is the deployed architecture."),
        payload_has(&rag, "ArchV2 is the deployed architecture."),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    assert!(
        rag_stale == 2,
        "rag baseline must pack both superseded claims (temporal confusion): {rag:?}"
    );

    eprintln!(
        "[W3-TEMP-001] aikoql: current + incident delivered, 2/2 superseded claims \
         suppressed; rag: {rag_stale}/2 superseded claims delivered (temporal accuracy 0)"
    );
}

/// W3-UNK-001 — unknown handling classification. Four classes measured:
/// known (non-empty pack containing the answer fact), unknown (healthy
/// EMPTY pack = "no authoritative knowledge" — the ContextPackage.status
/// contract), conflicting (KNOW-007: both current claims delivered, no
/// silent pick), historical-only (the stale boundary drops the only
/// candidate → empty pack, while the kernel still serves history via get).
/// Plus the measured false-confidence probe (vocabulary overlap → non-empty
/// pack on an absent answer) — reported, not asserted (IR-level false
/// confidence, the honest W11 measurement from the win-zone bench).
#[test]
fn w3_unk_001_unknown_handling_classification() {
    let (k, clock) = mk();
    let day = 86_400_000u64;
    let t0 = 1_767_225_600_000u64;

    let stmt_k = "The rollback procedure is to redeploy the previous version.";
    let stmt_a = "Rollback is immediate.";
    let stmt_b = "Rollback is scheduled.";
    let stmt_old = "Old deploys used blue-green switching.";
    let stmt_new = "Deploys use canary releases.";

    // known claim K (current).
    clock.set(t0);
    let known = assert_claim(
        &k,
        "Claim",
        props(&[
            ("policy", "rollback"),
            ("answer", "redeploy previous version"),
        ]),
        "human_approved",
        "kb/policy.md",
    );
    // conflicting pair A/B — both current until resolved (KNOW-007).
    let a = assert_claim(
        &k,
        "Claim",
        props(&[("policy", "rollback"), ("answer", "immediate")]),
        "human_approved",
        "kb/policy.md",
    );
    let mut contra = ContradictionRequest::new(alice(), a);
    contra.counter_props = props(&[("policy", "rollback"), ("answer", "scheduled")]);
    contra.authority = Some("documentation".into());
    contra.evidence = vec![ev("chat/incident.md")];
    let conflict = k.contradict(contra).unwrap();
    // historical-only: old claim superseded → its statement goes stale, the
    // KO stays readable (history is a kernel concern).
    clock.set(t0 + day);
    let old = assert_claim(
        &k,
        "Claim",
        props(&[("switch", "blue-green")]),
        "architecture_decision",
        "kb/arch.md",
    );
    let replacement = supersede_claim(
        &k,
        old,
        props(&[("switch", "canary")]),
        "deployment strategy change",
        "kb/arch.md",
    );

    // Kernel boundary: A and its counter are both current; the conflict is
    // persisted and symmetric.
    assert_eq!(k.get(alice(), &known).unwrap().valid_to(), None);
    assert_eq!(k.get(alice(), &a).unwrap().valid_to(), None);
    assert_eq!(k.get(alice(), &conflict.counter).unwrap().valid_to(), None);
    assert_eq!(
        k.get(alice(), &conflict.conflict)
            .unwrap()
            .metadata
            .type_name,
        "aikoql:conflict"
    );
    assert!(k.get(alice(), &old).unwrap().valid_to().is_some());
    assert_eq!(k.get(alice(), &replacement).unwrap().valid_to(), None);

    let ir = KnowledgeIr {
        entities: vec![
            entity("RollbackProcedure", "Procedure", &["rollback"]),
            entity("Deploy", "Process", &["deploy"]),
        ],
        facts: vec![
            fact(stmt_k, &["RollbackProcedure"]),
            fact(stmt_a, &["RollbackProcedure"]),
            fact(stmt_b, &["RollbackProcedure"]),
            fact(stmt_old, &["Deploy"]),
            fact(stmt_new, &["Deploy"]),
        ],
        ..Default::default()
    };

    // ── known: the answer fact is in a healthy non-empty pack ─────────────
    let pkg = compile_context_with_validity(
        "What is the rollback procedure?",
        &ir,
        BUDGET,
        None,
        &HashSet::new(),
    );
    let facts = pkg_facts(&pkg);
    assert_eq!(pkg.status, RetrievalStatus::Healthy);
    assert!(
        facts.contains(&stmt_k),
        "known classification: answer fact missing: {facts:?}"
    );

    // ── unknown: zero-overlap task → healthy EMPTY pack ──────────────────
    let pkg = compile_context_with_validity(
        "What is the pricing model?",
        &ir,
        BUDGET,
        None,
        &HashSet::new(),
    );
    assert!(
        pkg.facts.is_empty() && pkg.status == RetrievalStatus::Healthy,
        "unknown classification must be a healthy empty pack, got {:?}/{:?}",
        pkg_facts(&pkg),
        pkg.status,
    );

    // ── conflicting: both current claims delivered, no silent pick ───────
    let pkg =
        compile_context_with_validity("How is rollback done?", &ir, BUDGET, None, &HashSet::new());
    let facts = pkg_facts(&pkg);
    assert!(
        facts.contains(&stmt_a) && facts.contains(&stmt_b),
        "conflicting classification: KNOW-007 requires both claims, got {facts:?}"
    );

    // ── historical-only: stale boundary drops the only candidate; the
    // kernel still serves the KO (history reachable, context empty) ───────
    let stale = kernel_stale(&k, &[(old, stmt_old)]);
    let pkg = compile_context_with_validity("blue-green switching", &ir, BUDGET, None, &stale);
    assert!(
        pkg.facts.is_empty() && pkg.status == RetrievalStatus::Healthy,
        "historical-only: stale candidate must leave an empty pack, got {:?}",
        pkg_facts(&pkg),
    );
    assert!(
        k.get(alice(), &old).unwrap().valid_to().is_some(),
        "history must remain reachable via get"
    );

    // ── measured false-confidence probe (honest negative evidence) ──────
    // Vocabulary overlap delivers a non-empty pack for a question whose
    // answer is absent: "rollback procedure" tokens hit the policy fact,
    // but nothing answers "for failed deploys". Reported, not asserted.
    let probe = compile_context_with_validity(
        "What is the rollback procedure for failed deploys?",
        &ir,
        BUDGET,
        None,
        &HashSet::new(),
    );
    eprintln!(
        "[W3-UNK-001] classes known/unknown/conflicting/historical-only asserted; \
         false-confidence probe: {} fact(s) delivered on an absent answer \
         (IR-level false confidence — the caller must not read a non-empty pack \
         as answered)",
        probe.facts.len(),
    );
    eprintln!(
        "[W3-UNK-001] classification: known=non-empty+answer, unknown=healthy-empty, \
         conflicting=both-claims (KNOW-007), historical-only=stale-empty + get() reachable"
    );
}

/// W3-CONF-001 — contradiction value. An unsafe issue-note and an outdated
/// ADR both conflict with the current policy. The kernel supersedes both
/// onto the policy KO (superseded_by), and the compile boundary delivers
/// ONLY the policy fact; the RAG baseline packs all three chunks (shared
/// vocabulary) and hands the unsafe instruction to the LLM. Both superseded
/// claims stay readable via get — authority selection, not data loss.
#[test]
fn w3_conf_001_contradiction_value() {
    let (k, clock) = mk();
    let t0 = 1_767_225_600_000u64;
    clock.set(t0);

    let stmt_note = "Deploys restart the service.";
    let stmt_adr = "Deploys use blue-green switching.";
    let stmt_policy = "Deploys must not restart services during business hours.";

    let note = assert_claim(
        &k,
        "Claim",
        props(&[("action", "restart the service")]),
        "documentation",
        "kb/issues.md",
    );
    let adr = assert_claim(
        &k,
        "Claim",
        props(&[("decision", "blue-green")]),
        "architecture_decision",
        "kb/adr.md",
    );
    let policy = assert_claim(
        &k,
        "Claim",
        props(&[("requirement", "zero-downtime")]),
        "organization_policy",
        "kb/policy.md",
    );

    // The policy KO is the resolution target: both conflicting claims are
    // superseded onto it (no new KO — superseded_by).
    for (old, why) in [
        (note, "policy overrides issue note"),
        (adr, "policy overrides ADR"),
    ] {
        let mut req = SupersedeRequest::new(alice(), old, "Claim");
        req.superseded_by = Some(policy);
        req.evidence = vec![ev("kb/policy.md")];
        req.reason = Some(why.into());
        k.supersede(req).unwrap();
    }

    // Kernel: both claims stamped superseded, policy current, and the
    // supersedes edge points at the policy (auditable resolution).
    assert!(k.get(alice(), &note).unwrap().valid_to().is_some());
    assert!(k.get(alice(), &adr).unwrap().valid_to().is_some());
    assert_eq!(k.get(alice(), &policy).unwrap().valid_to(), None);
    assert!(k
        .get(alice(), &note)
        .unwrap()
        .relationships
        .iter()
        .any(|r| r.rel_type == SUPERSEDES && r.target == policy));

    let ir = KnowledgeIr {
        entities: vec![entity("Deploy", "Process", &["deploy"])],
        facts: vec![
            fact(stmt_note, &["Deploy"]),
            fact(stmt_adr, &["Deploy"]),
            fact(stmt_policy, &["Deploy"]),
        ],
        ..Default::default()
    };
    let stale = kernel_stale(&k, &[(note, stmt_note), (adr, stmt_adr)]);

    let task = "What do deploys require?";
    let pkg = compile_context_with_validity(task, &ir, BUDGET, None, &stale);
    let facts = pkg_facts(&pkg);
    assert_eq!(
        facts.len(),
        1,
        "only the policy fact may survive: {facts:?}"
    );
    assert!(facts.contains(&stmt_policy));

    let provider = MockEmbeddingProvider::new();
    let chunks: Vec<common::CorpusChunk> = vec![
        chunk("kb/issues.md", 0, stmt_note),
        chunk("kb/adr.md", 0, stmt_adr),
        chunk("kb/policy.md", 0, stmt_policy),
    ];
    let rag = rag_pack(task, &chunks, &provider);
    let rag_claims = chunks.iter().filter(|c| payload_has(&rag, &c.2)).count();
    assert!(
        payload_has(&rag, stmt_note),
        "rag baseline must deliver the superseded unsafe claim"
    );

    eprintln!(
        "[W3-CONF-001] aikoql delivered 1/3 claims (policy only); rag delivered \
         {rag_claims}/3 (superseded claims included) — the contradiction value \
         is compile-boundary authority selection with audit-ready history"
    );
}

/// W3-LONG-001 — longitudinal value over 90 days. Capacity claims
/// superseded in turn (100 → 200 → 500 → 900). Three treatments per
/// checkpoint: stateless RAG (packs the accumulated chunks — correct until
/// the world changes, then stale-confused), conversation history (the full
/// growing transcript — carries stale claims forever), and AIKOQL (compile
/// with the kernel-computed stale set — current fact only, flat tokens).
#[test]
fn w3_long_001_longitudinal_value() {
    let (k, clock) = mk();
    let day = 86_400_000u64;
    let t0 = 1_767_225_600_000u64;

    let caps = ["100", "200", "500", "900"];
    let stmts: Vec<String> = caps
        .iter()
        .map(|c| format!("Region capacity is {c}."))
        .collect();
    let provider = MockEmbeddingProvider::new();

    // claims[i] = (koid, index into stmts) in publication order; the last
    // is always the current claim.
    let mut claims: Vec<(KOID, usize)> = Vec::new();
    // Accumulated doc chunks (the stateless retriever indexes everything).
    let mut chunks: Vec<common::CorpusChunk> = Vec::new();
    let mut transcript = String::new(); // conversation-history buffer
    let mut aikoql_success = 0usize;
    let mut rag_success = 0usize;
    let mut hist_success = 0usize;
    let mut aikoql_tokens = Vec::new();
    let mut hist_tokens = Vec::new();

    let days = [0u64, 7, 30, 90];
    let query = "Region capacity";

    clock.set(t0);
    claims.push((
        assert_claim(
            &k,
            "Claim",
            props(&[("capacity", caps[0])]),
            "deployment_observed",
            "ops/capacity.md",
        ),
        0,
    ));
    chunks.push(chunk("ops/capacity.md", 0, &stmts[0]));

    for (i, &d) in days.iter().enumerate() {
        // Evolve the world before measuring (except day 0).
        if i > 0 {
            clock.set(t0 + d * day);
            claims.push((
                supersede_claim(
                    &k,
                    claims[i - 1].0,
                    props(&[("capacity", caps[i])]),
                    "capacity upgrade",
                    "ops/capacity.md",
                ),
                i,
            ));
            chunks.push(chunk("ops/capacity.md", i, &stmts[i]));
        }

        // The IR grows with the world: all claims published so far.
        let ir = KnowledgeIr {
            entities: vec![entity("Region", "Region", &["capacity"])],
            facts: stmts[..=i].iter().map(|s| fact(s, &["Region"])).collect(),
            ..Default::default()
        };
        let claim_pairs: Vec<(KOID, &str)> = claims
            .iter()
            .map(|(koid, idx)| (*koid, stmts[*idx].as_str()))
            .collect();
        let stale = kernel_stale(&k, &claim_pairs);
        // All but the last claim are superseded by construction.
        let superseded: Vec<&str> = claims
            .iter()
            .take(claims.len().saturating_sub(1))
            .map(|(_, idx)| stmts[*idx].as_str())
            .collect();

        // ── AIKOQL: compile with the kernel-computed boundary ─────────────
        let pkg = compile_context_with_validity(query, &ir, BUDGET, None, &stale);
        let facts = pkg_facts(&pkg);
        let rendered = render_context_markdown(&pkg);
        assert_eq!(
            facts.len(),
            1,
            "day {d}: aikoql must deliver exactly the current claim: {facts:?}"
        );
        assert!(facts.contains(&stmts[i].as_str()));
        let a_ok = payload_has(&rendered, &stmts[i])
            && superseded.iter().all(|s| !payload_has(&rendered, s));
        assert!(a_ok, "day {d}: aikoql boundary failed");
        aikoql_success += 1;
        aikoql_tokens.push(rendered.len() / 4);

        // ── Stateless RAG over the accumulated chunks ─────────────────────
        let rag = rag_pack(query, &chunks, &provider);
        let r_ok = payload_has(&rag, &stmts[i]) && superseded.iter().all(|s| !payload_has(&rag, s));
        rag_success += r_ok as usize;

        // ── Conversation history: the transcript grows, nothing forgotten ─
        transcript.push_str(&rag);
        transcript.push(' ');
        hist_tokens.push(transcript.len() / 4);
        let h_ok = payload_has(&transcript, &stmts[i])
            && superseded.iter().all(|s| !payload_has(&transcript, s));
        hist_success += h_ok as usize;

        eprintln!(
            "[W3-LONG-001] day {d:>2}: aikoql ok={a_ok} tokens={} rag ok={r_ok} \
             hist ok={h_ok} tokens={}",
            rendered.len() / 4,
            transcript.len() / 4,
        );
    }

    // Kernel history at the end: every superseded generation still
    // readable with its valid_to stamped.
    for (koid, idx) in &claims[..3] {
        assert!(k.get(alice(), koid).unwrap().valid_to().is_some());
        let _ = idx;
    }
    assert_eq!(k.get(alice(), &claims[3].0).unwrap().valid_to(), None);

    assert_eq!(aikoql_success, 4, "aikoql must stay correct all 90 days");
    assert_eq!(
        rag_success, 1,
        "stateless rag is right only until the world changes"
    );
    assert_eq!(
        hist_success, 1,
        "the transcript carries stale claims forever"
    );
    assert!(
        aikoql_tokens.windows(2).all(|w| w[0] == w[1]),
        "aikoql tokens must stay flat over time: {aikoql_tokens:?}"
    );
    assert!(
        hist_tokens.windows(2).all(|w| w[0] < w[1]),
        "history tokens must grow: {hist_tokens:?}"
    );

    eprintln!(
        "[W3-LONG-001] 90-day verdict: aikoql {aikoql_success}/4 with flat tokens \
         ({aikoql_tokens:?}); rag {rag_success}/4; conversation history \
         {hist_success}/4 with growing tokens ({hist_tokens:?})"
    );
}

/// W3-DEBUG-001 — observability: five injected failures, each surfaced by a
/// deterministic kernel read (the diagnosis path a human follows):
/// (a) wrong source — explain() reveals the evidence artifact;
/// (b) stale fact — get() shows valid_to + the SUPERSEDES target;
/// (c) wrong relationship — get() shows the committed edge;
/// (d) conflicting source — both current claims readable + Conflict KO;
/// (e) missing evidence — assert_knowledge without evidence fails closed
///     (P0-1: no assertion without provenance).
/// Root-cause identification rate 5/5 asserted.
#[test]
fn w3_debug_001_observability_root_cause() {
    let (k, clock) = mk();
    let t0 = 1_767_225_600_000u64;
    clock.set(t0);
    let t_start = std::time::Instant::now();
    let mut diagnosed = 0usize;

    // (a) wrong source: the claim cites the wrong runbook — explain()
    // surfaces the artifact instead of trusting it silently.
    let claim = assert_claim(
        &k,
        "Claim",
        props(&[("restart", "true")]),
        "human_approved",
        "wrong-runbook.md",
    );
    let ex = k.explain(alice(), &claim, None).unwrap();
    assert_eq!(
        ex.source.as_deref(),
        Some("wrong-runbook.md"),
        "wrong source not surfaced by explain()"
    );
    diagnosed += 1;

    // (b) stale fact: superseded → valid_to + the supersedes edge to its
    // replacement. Reading the old KO shows it is history, not current.
    let replacement = supersede_claim(
        &k,
        claim,
        props(&[("restart", "false")]),
        "policy",
        "kb/runbook.md",
    );
    let old = k.get(alice(), &claim).unwrap();
    assert!(old.valid_to().is_some(), "superseded claim lacks valid_to");
    assert!(
        old.relationships
            .iter()
            .any(|r| r.rel_type == SUPERSEDES && r.target == replacement),
        "supersedes edge missing"
    );
    diagnosed += 1;

    // (c) wrong relationship: the edge as committed is readable — the
    // diagnosis compares it against the intended topology.
    let svc_a = assert_claim(
        &k,
        "Service",
        props(&[("name", "Checkout")]),
        "deployment_observed",
        "kb/arch.md",
    );
    let svc_b = assert_claim(
        &k,
        "Service",
        props(&[("name", "Payments")]),
        "deployment_observed",
        "kb/arch.md",
    );
    k.relate(RelateRequest::new(alice(), svc_a, svc_b, "depends_on"))
        .unwrap();
    let head = k.get(alice(), &svc_a).unwrap();
    assert!(
        head.relationships
            .iter()
            .any(|r| r.rel_type == "depends_on" && r.target == svc_b),
        "committed edge not readable"
    );
    diagnosed += 1;

    // (d) conflicting source: two current claims, different authorities —
    // both readable, the conflict KO persists (KNOW-007: no silent pick).
    let mut contra = ContradictionRequest::new(alice(), replacement);
    contra.counter_props = props(&[("restart", "true")]);
    contra.authority = Some("documentation".into());
    contra.evidence = vec![ev("chat/incident.md")];
    let res = k.contradict(contra).unwrap();
    assert_eq!(k.get(alice(), &replacement).unwrap().valid_to(), None);
    assert_eq!(k.get(alice(), &res.counter).unwrap().valid_to(), None);
    assert_eq!(
        k.get(alice(), &res.conflict).unwrap().metadata.type_name,
        "aikoql:conflict"
    );
    diagnosed += 1;

    // (e) missing evidence: assertion without provenance fails closed.
    let mut bare = AssertionRequest::new(alice(), "Claim");
    bare.properties = props(&[("restart", "false")]);
    bare.authority = Some("human_approved".into());
    assert!(
        k.assert_knowledge(bare).is_err(),
        "evidence-free assertion must fail closed (P0-1)"
    );
    diagnosed += 1;

    assert_eq!(diagnosed, 5, "not all injected failures were diagnosed");
    eprintln!(
        "[W3-DEBUG-001] root-cause identification rate {diagnosed}/5; mechanical \
         time-to-diagnose {:?} (a human still reasons about which reading \
         pinpoints the root cause — the kernel surfaces all five)",
        t_start.elapsed(),
    );
}
