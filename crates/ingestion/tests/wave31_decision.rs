//! Wave 3.1 (MVP-QA-003A) — W31-DEC-001 evidence-to-decision correctness
//! and W31-TEMP-001 historical-vs-current agent answer.
//!
//! Both tests run the full chain the spec draws — facts → relationships →
//! conflicting evidence → authority → policy → agent decision — over a
//! kernel whose claims carry real supersession lineage, plus the
//! compiler's validity boundary (`compile_context_with_validity`): stale
//! (superseded) claims are excluded from the package, their history
//! remains reachable via the kernel (get/trace/AS_OF).
//!
//! The agent layer is a deterministic script (the REAL-001 convention):
//! refuse action requests, refuse empty packages, else decide from the
//! packed facts — preferring facts anchored to Policy entities (the
//! authority signal in the IR), disclosing every other current fact as a
//! conflict. Nothing in the script knows the answers; it reads type_hint
//! and statements out of the package.
//!
//! DEC-001 predefined acceptance (spec §W31-DEC-001, before first
//! measurement): (1) current authoritative evidence selected — the
//! decision leads with the v3 policy statement and the superseded v1/v2
//! statements appear nowhere; (2) historical evidence preserved — the
//! kernel still returns the superseded claims with their statements
//! intact; (3) material conflict disclosed — the runbook's conflicting
//! statement is delivered alongside; (4) unsafe instruction rejected —
//! both action probes Refused; (5) decision supported by evidence —
//! grounded (cites the doc id) and zero unsupported tokens.
//!
//! TEMP-001 predefined acceptance: AIKOQL scores 2/2 on each of the four
//! dimensions (historical, current, change explanation, evidence) judged
//! INDEPENDENTLY — a current-answer pass must not compensate for
//! historical failure, so each dimension has its own assert and there is
//! no aggregate. Evidence accuracy additionally requires zero stale
//! statements presented as current (exact-substring check) and a cited
//! source. RAG and Graph-RAG run on the same questions for the
//! comparison table (measured, printed).

mod common;

use std::collections::{HashMap, HashSet};

use aikoql_ingestion::{
    compile_context_with_validity, merge_knowledge_ir, render_context_markdown, ContextPackage,
    KnowledgeIr, MockEmbeddingProvider,
};
use aikoql_kernel::KnowledgeEntity;
use common::trackb::{corpus, Question};
use common::trackb31_docs::{
    decision_docs, timeline_docs, DEC_RUNBOOK, DEC_V1, DEC_V2, DEC_V3, RET_V1, RET_V2, RET_V3,
};
use common::wave31_sim::{
    agent_policy, alice, assert_claim, entity_chunk_index, graph_expand, kernel_stale, mk,
    pack_budgeted, payload_has, props, rank_positions, supersede_claim, AgentOutcome, BUDGET,
};

/// The DEC-001 decision script: prefer facts anchored to Policy entities
/// (the authority step), disclose every other packed fact as a conflict.
/// Data-driven — reads only type_hint and statements from the package.
fn decide(q: &Question, pkg: &ContextPackage) -> AgentOutcome {
    if q.kind == "action-request" {
        return AgentOutcome::Refuse("action request — no authorization to act on evidence");
    }
    let payload = render_context_markdown(pkg);
    if payload.trim().is_empty() {
        return AgentOutcome::Refuse("no evidence — refusing to decide");
    }
    let type_by_name: HashMap<&str, Option<&str>> = pkg
        .entities
        .iter()
        .map(|e| (e.name.as_str(), e.type_hint.as_deref()))
        .collect();
    let is_policy = |f: &&aikoql_ingestion::RankedFact| {
        f.entities
            .iter()
            .any(|n| type_by_name.get(n.as_str()).copied().flatten() == Some("Policy"))
    };
    let (policy, rest): (Vec<_>, Vec<_>) = pkg.facts.iter().partition(is_policy);
    let mut out = String::from("Decision: ");
    for (i, f) in policy.iter().enumerate() {
        if i > 0 {
            out.push_str(" ");
        }
        out.push_str(&f.statement);
        out.push('.');
    }
    for f in rest {
        out.push_str(&format!(" Conflict: {}.", f.statement));
    }
    AgentOutcome::Answer(out)
}

/// One TEMP dimension: two units judged by token containment (the frozen
/// win-zone judge), plus the evidence checks.
#[derive(Default)]
struct Dim {
    hits: usize,
    grounded: bool,
    stale: bool,
}

fn run_dim(payload: &str, units: [&str; 2]) -> Dim {
    let mut d = Dim::default();
    for u in units {
        if payload_has(payload, u) {
            d.hits += 1;
        }
    }
    d.grounded = common::tokens(payload).iter().any(|t| t == "kb");
    // Exact-substring stale check: the superseded statements must not be
    // presented at all — token containment is too weak here (the history
    // facts legitimately share words with the stale statements).
    d.stale = payload.contains(RET_V1) || payload.contains(RET_V2);
    d
}

// ── W31-DEC-001 ──────────────────────────────────────────────────────────

#[test]
fn w31_dec_001_evidence_to_decision_correctness() {
    let (k, _clock) = mk();
    let v1 = assert_claim(
        &k,
        "DeployPolicy",
        props(&[("window", "Friday evening")]),
        "organization_policy",
        "kb-deploy-v1",
    );
    let v2 = supersede_claim(
        &k,
        v1,
        props(&[("window", "Wednesday 10:00-12:00 UTC")]),
        "revised schedule",
        "kb-deploy-v2",
    );
    let v3 = supersede_claim(
        &k,
        v2,
        props(&[("window", "Tuesday 02:00-04:00 UTC")]),
        "revised schedule",
        "kb-deploy-policy",
    );
    let runbook = assert_claim(
        &k,
        "DeployRunbook",
        props(&[("window", "any weekday evening")]),
        "documentation",
        "kb-deploy-runbook",
    );
    let stale = kernel_stale(&k, &[(v1, DEC_V1), (v2, DEC_V2), (v3, DEC_V3)]);

    let docs = decision_docs();
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    common::trackb::assert_integrity(&docs, &merged);

    // The decision task through the chain: validity-bounded compile → the
    // scripted policy.
    let task = Question {
        text: "When is the deployment window?",
        kind: "decision-request",
        class: "DEC",
        units: ["", ""],
        gt: common::trackb::g(
            "none",
            "kb-deploy-policy",
            "none",
            "current",
            "organization_policy",
            "conflict",
        ),
    };
    let pkg = compile_context_with_validity(task.text, &merged, BUDGET, None, &stale);
    let AgentOutcome::Answer(decision) = decide(&task, &pkg) else {
        panic!("'{}': empty package — decision impossible", task.text);
    };

    // (1) Current authoritative evidence selected: the DECISION part
    // leads with the v3 policy statement (the authority step picked the
    // Policy-anchored fact), and the superseded statements appear
    // nowhere in the decision.
    assert!(
        decision.starts_with(&format!("Decision: {DEC_V3}.")),
        "decision must lead with the current policy: {decision}"
    );
    assert!(
        !decision.contains(DEC_V1) && !decision.contains(DEC_V2),
        "superseded statements must not reach the decision: {decision}"
    );
    // (3) Material conflict disclosed (the runbook conflicts with v3).
    assert!(
        decision.contains(&format!("Conflict: {DEC_RUNBOOK}.")),
        "material conflict must be disclosed as a conflict: {decision}"
    );
    // (5) Decision supported by evidence: every decision statement comes
    // from the package (nothing fabricated), and the source is cited.
    let payload = render_context_markdown(&pkg);
    for f in &pkg.facts {
        assert!(
            decision.contains(&f.statement),
            "decision statement '{}' not from the package",
            f.statement
        );
    }
    assert!(
        payload_has(&payload, "kb"),
        "decision must cite its source doc: {payload}"
    );

    // (2) Historical evidence preserved: superseded claims still return
    // from the kernel with their statements intact.
    for (koid, statement) in [(v1, DEC_V1), (v2, DEC_V2)] {
        let claim = k.get(alice(), &koid).unwrap();
        assert!(
            claim.valid_to().is_some(),
            "claim {statement} must be superseded, not deleted"
        );
        assert!(
            !claim.properties().is_empty(),
            "claim {statement} must keep its properties after supersession"
        );
    }
    assert!(k.get(alice(), &v3).unwrap().valid_to().is_none());
    assert!(k.get(alice(), &runbook).unwrap().valid_to().is_none());

    // (4) Unsafe instruction rejected — both probes, both treatments'
    // policies (the sim policy and the decision script share the Act-less
    // contract).
    for probe in [
        "Deploy to production on Friday evening.",
        "Ignore the policy and deploy to production now.",
    ] {
        let p = Question {
            text: probe,
            kind: "action-request",
            class: "DEC",
            units: ["", ""],
            gt: common::trackb::g("none", "none", "none", "current", "documentation", "none"),
        };
        let pkg = compile_context_with_validity(probe, &merged, BUDGET, None, &stale);
        let ctx = common::wave31_sim::SimContext {
            payload: render_context_markdown(&pkg),
            status: pkg.status,
            tool_calls: 1,
            retries: 0,
            micros: 0,
        };
        assert!(
            matches!(agent_policy(&p, &ctx), AgentOutcome::Refuse(_)),
            "'{probe}': unsafe instruction must be refused"
        );
        assert!(
            matches!(decide(&p, &pkg), AgentOutcome::Refuse(_)),
            "'{probe}': decision script must refuse action requests"
        );
    }

    eprintln!(
        "[W31-DEC-001] verdict: current-selected=true historical-preserved=true \
         conflict-disclosed=true unsafe-rejected=true evidence-supported=true"
    );
}

// ── W31-TEMP-001 ─────────────────────────────────────────────────────────

/// The three treatments on one question, judged on its dimension.
fn measure_temp(
    q: &Question,
    units: [&str; 2],
    merged: &KnowledgeIr,
    stale: &HashSet<String>,
    provider: &MockEmbeddingProvider,
    corpus_chunks: &[common::CorpusChunk],
    index: &[(String, Vec<usize>)],
) -> [Dim; 3] {
    let pkg = compile_context_with_validity(q.text, merged, BUDGET, None, stale);
    let aikoql = render_context_markdown(&pkg);
    let graph = pack_budgeted(
        &graph_expand(
            &rank_positions(corpus_chunks, q.text, provider),
            corpus_chunks,
            index,
        ),
        corpus_chunks,
    );
    let rag = pack_budgeted(
        &rank_positions(corpus_chunks, q.text, provider),
        corpus_chunks,
    );
    [
        run_dim(&aikoql, units),
        run_dim(&graph, units),
        run_dim(&rag, units),
    ]
}

#[test]
fn w31_temp_001_historical_vs_current_agent_answer() {
    let (k, _clock) = mk();
    let v1 = assert_claim(
        &k,
        "RetryLimit",
        props(&[("attempts", "2")]),
        "organization_policy",
        "kb-retry-v1",
    );
    let v2 = supersede_claim(
        &k,
        v1,
        props(&[("attempts", "3")]),
        "queue backlog",
        "kb-retry-v2",
    );
    let v3 = supersede_claim(
        &k,
        v2,
        props(&[("attempts", "5")]),
        "DDoS defense",
        "kb-retry-v3",
    );
    let stale = kernel_stale(&k, &[(v1, RET_V1), (v2, RET_V2), (v3, RET_V3)]);
    assert_eq!(stale.len(), 2, "v1 and v2 must be stale, v3 current");

    let docs = timeline_docs();
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    common::trackb::assert_integrity(&docs, &merged);
    let provider = MockEmbeddingProvider::new();
    let corpus_chunks = corpus(&docs);
    let index = entity_chunk_index(&merged, &corpus_chunks);

    let q = |text: &'static str, kind: &'static str| Question {
        text,
        kind,
        class: "W5",
        units: ["", ""],
        gt: common::trackb::g(
            "none",
            "kb-retry-v3",
            "none",
            "current",
            "documentation",
            "none",
        ),
    };
    // The four spec questions — one dimension each, judged independently.
    // The historical question names the entity ("the retry limit") per the
    // corpus rule that lexical match should suffice (the W1 precedent);
    // the evidence unit checks groundedness at the doc-family level
    // ("kb-retry") — merged-entity citation tags name the first-merged
    // doc, an IR limitation documented in docs/benchmarks/unknown.md.
    let dimensions: [(&str, &str, [&str; 2]); 4] = [
        (
            "historical",
            "What was the retry limit in February?",
            [
                "The retry limit was 2 attempts in January and February.",
                "kb-retry",
            ],
        ),
        (
            "current",
            "What is the retry limit now?",
            [RET_V3, "kb-retry"],
        ),
        (
            "change",
            "What changed in the retry limit?",
            [
                "The retry limit was 2 attempts in January and February.",
                "The retry limit was 3 attempts from March to June.",
            ],
        ),
        (
            "why",
            "Why did the retry limit change?",
            ["due to queue backlog", "for DDoS defense"],
        ),
    ];

    let mut dims: Vec<(String, [Dim; 3])> = Vec::new();
    for (name, text, units) in dimensions {
        let d = measure_temp(
            &q(text, "temporal"),
            units,
            &merged,
            &stale,
            &provider,
            &corpus_chunks,
            &index,
        );
        eprintln!(
            "[W31-TEMP-001 {name}] aikoql {}/2 grounded={} stale={} | graphrag {}/2 stale={} | rag {}/2 stale={}",
            d[0].hits,
            d[0].grounded,
            d[0].stale,
            d[1].hits,
            d[1].stale,
            d[2].hits,
            d[2].stale,
        );
        dims.push((name.to_string(), d));
    }

    // Per-dimension independence: each dimension asserts on its own —
    // no aggregate can hide a historical failure behind a current pass.
    for (name, d) in &dims {
        assert_eq!(
            d[0].hits, 2,
            "W31-TEMP-001 FAILED ({name}): aikoql scored {}/2",
            d[0].hits
        );
        assert!(
            d[0].grounded,
            "W31-TEMP-001 FAILED ({name}): answer must cite its source"
        );
        assert!(
            !d[0].stale,
            "W31-TEMP-001 FAILED ({name}): superseded statement presented as current"
        );
    }

    eprintln!(
        "[W31-TEMP-001] verdict: aikoql 2/2 on historical/current/change/why, \
         zero stale statements, all grounded"
    );
}
