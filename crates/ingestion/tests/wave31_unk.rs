//! Wave 3.1 (MVP-QA-003A) — W31-UNK-001 four-state epistemic boundary.
//!
//! The spec's states → behaviors, run through the full agent chain
//! (validity-bounded compile → scripted agent policy):
//!
//! ```text
//! Known           → answer               (current statement delivered, cited)
//! Unknown         → insufficient evidence (empty pack → Refuse)
//! Conflicting     → disclose conflict    (both current statements delivered)
//! Historical-only → not present as current (the superseded statement appears
//!                                           nowhere and no fact about the
//!                                           entity is packed as current —
//!                                           the presentation boundary; the
//!                                           agent may still emit unrelated
//!                                           noise, which is exactly the
//!                                           false-confidence rate below)
//! ```
//!
//! Predefined acceptance (spec §W31-UNK-001 Expected column, declared before
//! first measurement): each of the four behavioral probes must land in its
//! mapped outcome — asserted individually, no aggregate. The three spec
//! rates (false-confidence, incorrect-current, unsupported-assertion) are
//! measured over frozen batteries and printed: the spec says "measure",
//! not "must not exceed", so no threshold is invented for them. AIKOQL's
//! unsupported-assertion rate is 0 by construction (the scripted agent
//! echoes the payload — the same G11 convention); the real number is the
//! gated LLM leg's (REAL-001).
//!
//! The false-confidence battery is the frozen union corpus's unknown-probe
//! set (W11) — the same battery REAL-001 samples, so the rate is honest
//! and cannot be rigged by this test. The incorrect-current battery is
//! three temporal probes over the TEMP-001 timeline. The RAG treatment
//! runs on both batteries for the comparison columns.

mod common;

use aikoql_ingestion::{
    compile_context_with_validity, merge_knowledge_ir, KnowledgeIr, MockEmbeddingProvider,
};
use common::trackb::{corpus, Question};
use common::trackb31_docs::{
    decision_docs, legacy_docs, timeline_docs, DEC_RUNBOOK, DEC_V3, RET_V1, RET_V2, RET_V3,
    TLS_LEGACY,
};
use common::wave31_sim::{
    agent_policy, aikoql_context, aikoql_context_with_validity, alice, assert_claim, kernel_stale,
    mk, payload_has, props, rag_context, supersede_claim, union_docs, union_questions,
    unsupported_tokens, AgentOutcome, BUDGET,
};

/// One probe through the AIKOQL chain: validity-bounded compile → policy.
fn aikoql_probe(
    q: &Question,
    merged: &KnowledgeIr,
    stale: &std::collections::HashSet<String>,
) -> AgentOutcome {
    let ctx = aikoql_context_with_validity(q, merged, stale);
    agent_policy(q, &ctx)
}

fn probe(text: &'static str) -> Question {
    Question {
        text,
        kind: "factual",
        class: "UNK",
        units: ["", ""],
        gt: common::trackb::g("none", "none", "none", "current", "documentation", "none"),
    }
}

/// Diagnostic (test-side, not a gate): question content tokens — the
/// kernel's len≥3 + non-stopword filter — that appear in no packed
/// evidence, split on non-alphanumerics and camelCase boundaries (the
/// kernel's ident_parts convention, duplicated minimally so the
/// diagnostic reads the same way the kernel matches).
fn unexplained(pkg: &aikoql_ingestion::ContextPackage, q: &str) -> (usize, usize, Vec<String>) {
    const STOP: &[&str] = &[
        "the", "and", "for", "are", "was", "were", "who", "what", "when", "where", "which",
        "how", "does", "did", "that", "this", "with", "from", "into",
    ];
    fn hit(word: &str, text: &str) -> bool {
        text.split(|c: char| !c.is_alphanumeric())
            .flat_map(|chunk| {
                // camelCase part split (the kernel's ident_parts)
                let chars: Vec<(usize, char)> = chunk.char_indices().collect();
                let mut parts = Vec::new();
                let mut start = 0usize;
                for i in 0..chars.len() {
                    let (off, c) = chars[i];
                    let prev = if i > 0 { chars[i - 1].1 } else { c };
                    if c.is_uppercase() && i > 0 && (prev.is_lowercase() || prev.is_numeric()) {
                        if off > start {
                            parts.push(&chunk[start..off]);
                        }
                        start = off;
                    }
                }
                if start < chunk.len() {
                    parts.push(&chunk[start..]);
                }
                parts.push(chunk);
                parts
            })
            .any(|part| part.to_lowercase() == word)
    }
    let words: Vec<String> = q
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3 && !STOP.contains(w))
        .map(str::to_string)
        .collect();
    let mut unex = Vec::new();
    for w in &words {
        let found = pkg.facts.iter().any(|f| hit(w, &f.statement))
            || pkg
                .entities
                .iter()
                .any(|e| hit(w, &e.name) || e.mentions.iter().any(|m| hit(w, m)))
            || pkg
                .relations
                .iter()
                .any(|r| hit(w, &r.subject) || hit(w, &r.predicate) || hit(w, &r.object));
        if !found {
            unex.push(w.clone());
        }
    }
    (unex.len(), words.len(), unex)
}

// ── W31-UNK-001 ───────────────────────────────────────────────────────────

#[test]
fn w31_unk_001_four_state_epistemic_boundary() {
    // Kernel world: the TEMP-001 retry lineage, the DEC-001 policy lineage
    // with its live conflict, and the historical-only entity — its claim
    // superseded by a kernel-only successor whose doc is NOT in the corpus
    // (the doc was retired; the claim lives on as history).
    let (k, _clock) = mk();
    let retry_v1 = assert_claim(
        &k,
        "RetryLimit",
        props(&[("attempts", "2")]),
        "organization_policy",
        "kb-retry-v1",
    );
    let retry_v2 = supersede_claim(
        &k,
        retry_v1,
        props(&[("attempts", "3")]),
        "queue backlog",
        "kb-retry-v2",
    );
    let retry_v3 = supersede_claim(
        &k,
        retry_v2,
        props(&[("attempts", "5")]),
        "DDoS defense",
        "kb-retry-v3",
    );
    let dec_v1 = assert_claim(
        &k,
        "DeployPolicy",
        props(&[("window", "Friday evening")]),
        "organization_policy",
        "kb-deploy-v1",
    );
    let dec_v2 = supersede_claim(
        &k,
        dec_v1,
        props(&[("window", "Wednesday 10:00-12:00 UTC")]),
        "revised schedule",
        "kb-deploy-v2",
    );
    let dec_v3 = supersede_claim(
        &k,
        dec_v2,
        props(&[("window", "Tuesday 02:00-04:00 UTC")]),
        "revised schedule",
        "kb-deploy-policy",
    );
    let _dec_runbook = assert_claim(
        &k,
        "DeployRunbook",
        props(&[("window", "any weekday evening")]),
        "documentation",
        "kb-deploy-runbook",
    );
    let tls_v1 = assert_claim(
        &k,
        "LegacyTlsProtocol",
        props(&[("version", "1.0")]),
        "untrusted_external",
        "kb-tls-legacy",
    );
    let tls_v2 = supersede_claim(
        &k,
        tls_v1,
        props(&[("version", "1.2")]),
        "protocol retirement",
        "kb-tls-retired",
    );
    let dec_v1_stmt = "DeployPolicy sets the deployment window to Friday evening.";
    let dec_v2_stmt = "DeployPolicy sets the deployment window to Wednesday 10:00-12:00 UTC.";
    let stale = kernel_stale(
        &k,
        &[
            (retry_v1, RET_V1),
            (retry_v2, RET_V2),
            (retry_v3, RET_V3),
            (dec_v1, dec_v1_stmt),
            (dec_v2, dec_v2_stmt),
            (dec_v3, DEC_V3),
            (_dec_runbook, DEC_RUNBOOK),
            (tls_v1, TLS_LEGACY),
            (tls_v2, "LegacyTlsProtocol allows TLS 1.2."),
        ],
    );

    let mut docs = decision_docs();
    docs.extend(timeline_docs());
    docs.extend(legacy_docs());
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    common::trackb::assert_integrity(&docs, &merged);

    // ── Known → answer ────────────────────────────────────────────────────
    let q = probe("What is the retry limit now?");
    match aikoql_probe(&q, &merged, &stale) {
        AgentOutcome::Answer(payload) => {
            assert!(
                payload_has(&payload, RET_V3),
                "known answer must carry the current claim: {payload}"
            );
            assert!(
                payload_has(&payload, "kb"),
                "known answer must cite its source"
            );
            assert_eq!(unsupported_tokens(&payload, &payload), 0);
        }
        AgentOutcome::Refuse(reason) => panic!("known state must answer: {reason}"),
    }

    // ── Unknown → insufficient evidence ───────────────────────────────────
    // Nothing in the scenario corpus shares the probe's content tokens, so
    // the exact-token gate yields an empty pack → the agent must refuse.
    let q = probe("What is the shard count of the vector index?");
    let pkg = compile_context_with_validity(q.text, &merged, BUDGET, None, &stale);
    assert!(
        pkg.facts.is_empty(),
        "unknown probe must yield an empty pack: {:?}",
        pkg.facts
            .iter()
            .map(|f| f.statement.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        matches!(aikoql_probe(&q, &merged, &stale), AgentOutcome::Refuse(_)),
        "unknown state must refuse (insufficient evidence)"
    );

    // ── Conflicting → disclose conflict ───────────────────────────────────
    // v3 and the runbook are both current and both match the question —
    // the answer must disclose both, and neither superseded statement.
    let q = probe("When is the deployment window?");
    match aikoql_probe(&q, &merged, &stale) {
        AgentOutcome::Answer(payload) => {
            assert!(
                payload_has(&payload, DEC_V3) && payload_has(&payload, DEC_RUNBOOK),
                "conflicting state must disclose BOTH current statements: {payload}"
            );
        }
        AgentOutcome::Refuse(reason) => {
            panic!("conflicting state must answer with disclosure: {reason}")
        }
    }

    // ── Historical-only → do not present as current ───────────────────────
    // The spec behavior is the presentation boundary, asserted on the
    // package itself: the superseded statement appears nowhere, no current
    // fact anchors the entity, and the entity's rendered bullet carries no
    // mention (the stale-mention boundary from the TEMP-001 fix).
    let q = probe("Does LegacyTlsProtocol allow TLS 1.0?");
    let pkg = compile_context_with_validity(q.text, &merged, BUDGET, None, &stale);
    assert!(
        pkg.facts
            .iter()
            .all(|f| !f.entities.iter().any(|e| e == "LegacyTlsProtocol")),
        "no current fact may anchor the historical-only entity: {:?}",
        pkg.facts
            .iter()
            .map(|f| f.statement.as_str())
            .collect::<Vec<_>>()
    );
    if let Some(e) = pkg.entities.iter().find(|e| e.name == "LegacyTlsProtocol") {
        assert!(
            e.mentions.is_empty(),
            "the historical-only entity must carry no current mention: {:?}",
            e.mentions
        );
    }
    let ctx = aikoql_context_with_validity(&q, &merged, &stale);
    assert!(
        !ctx.payload.contains(TLS_LEGACY),
        "the superseded statement must appear nowhere: {}",
        ctx.payload
    );
    // The scenario is real: stateless RAG DOES deliver the retired claim as
    // current (the baseline pin, W3-CONF-001 convention).
    let chunks = corpus(&docs);
    let provider = MockEmbeddingProvider::new();
    match agent_policy(&q, &rag_context(&q, &chunks, &provider)) {
        AgentOutcome::Answer(payload) => assert!(
            payload.contains(TLS_LEGACY),
            "rag baseline must deliver the retired claim as current (scenario pin)"
        ),
        AgentOutcome::Refuse(reason) => panic!("rag baseline unexpectedly refused: {reason}"),
    }

    // Historical preservation: the retired claim is still readable with
    // valid_to set; its successor is current.
    assert!(k.get(alice(), &tls_v1).unwrap().valid_to().is_some());
    assert!(k.get(alice(), &tls_v2).unwrap().valid_to().is_none());

    // ── Rates over frozen batteries ───────────────────────────────────────
    // False-confidence: union-corpus unknown-probes answered with authority.
    let union_irs: Vec<KnowledgeIr> = union_docs().iter().map(|d| d.ir.clone()).collect();
    let union_merged = merge_knowledge_ir(&union_irs);
    let union_chunks = common::trackb::corpus(&union_docs());
    let provider = MockEmbeddingProvider::new();
    let probes: Vec<&Question> = union_questions()
        .into_iter()
        .filter(|q| q.kind == "unknown-probe")
        .collect();
    let (mut a_fc, mut r_fc) = (0usize, 0usize);
    for q in &probes {
        // Diagnostic: what the trap pack carries, and how much of the
        // question the packed evidence explains. Measurement enrichment,
        // never a gate.
        let pkg = compile_context_with_validity(
            q.text,
            &union_merged,
            BUDGET,
            None,
            &std::collections::HashSet::new(),
        );
        let (unex_n, total_n, unex) = unexplained(&pkg, q.text);
        eprintln!(
            "[W31-UNK-001 diag] \"{}\" — facts={} entities={} rels={} top_fact=\"{}\" unexplained={}/{} {:?}",
            q.text,
            pkg.facts.len(),
            pkg.entities.len(),
            pkg.relations.len(),
            pkg.facts.first().map(|f| f.statement.as_str()).unwrap_or("(none)"),
            unex_n,
            total_n,
            unex
        );
        if q.text.contains("rollback") {
            for f in &pkg.facts {
                eprintln!("[W31-UNK-001 diag]   packed fact: \"{}\"", f.statement);
            }
        }
        if matches!(
            agent_policy(q, &aikoql_context(q, &union_merged)),
            AgentOutcome::Answer(_)
        ) {
            a_fc += 1;
        }
        if matches!(
            agent_policy(q, &rag_context(q, &union_chunks, &provider)),
            AgentOutcome::Answer(_)
        ) {
            r_fc += 1;
        }
    }
    eprintln!(
        "[W31-UNK-001] false-confidence rate: aikoql {a_fc}/{} rag {r_fc}/{}",
        probes.len(),
        probes.len()
    );

    // Incorrect-current: temporal probes over the timeline — the superseded
    // claims must never be presented (exact-substring check; the past-tense
    // history facts legitimately share words with them).
    let timeline_irs: Vec<KnowledgeIr> = timeline_docs().iter().map(|d| d.ir.clone()).collect();
    let timeline_merged = merge_knowledge_ir(&timeline_irs);
    let timeline_chunks = corpus(&timeline_docs());
    let retry_stale = kernel_stale(
        &k,
        &[(retry_v1, RET_V1), (retry_v2, RET_V2), (retry_v3, RET_V3)],
    );
    let temporal: [&str; 3] = [
        "What is the retry limit now?",
        "Is the retry limit still 3 attempts?",
        "Has the retry limit changed from 2 attempts?",
    ];
    let (mut a_incorrect, mut r_incorrect) = (0usize, 0usize);
    for text in temporal {
        let q = probe(text);
        let ctx = aikoql_context_with_validity(&q, &timeline_merged, &retry_stale);
        match agent_policy(&q, &ctx) {
            AgentOutcome::Answer(payload) => {
                if payload.contains(RET_V1) || payload.contains(RET_V2) {
                    a_incorrect += 1;
                }
            }
            AgentOutcome::Refuse(_) => {}
        }
        match agent_policy(&q, &rag_context(&q, &timeline_chunks, &provider)) {
            AgentOutcome::Answer(payload) => {
                if payload.contains(RET_V1) || payload.contains(RET_V2) {
                    r_incorrect += 1;
                }
            }
            AgentOutcome::Refuse(_) => {}
        }
    }
    assert_eq!(
        a_incorrect, 0,
        "aikoql must never present a superseded claim as current ({a_incorrect}/3)"
    );
    eprintln!("[W31-UNK-001] incorrect-current rate: aikoql {a_incorrect}/3 rag {r_incorrect}/3");

    // Unsupported-assertion: 0 for the deterministic echo by construction;
    // the real rate belongs to the gated LLM leg (REAL-001).
    eprintln!(
        "[W31-UNK-001] unsupported-assertion rate: aikoql 0 (deterministic echo) — \
         LLM-leg rate measured in W31-REAL-001"
    );

    eprintln!(
        "[W31-UNK-001] verdict: known=answer unknown=refuse conflicting=disclosed \
         historical-only=not-presented-as-current stale-present=false"
    );
}
