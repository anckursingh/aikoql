//! MVP-QA-002 Suite C — adversarial retrieval (compiler legs).
//!
//! - QA2-RET-003 Entity ambiguity: "What is Apple's revenue?" against
//!   Apple Inc. / Apple Records / Apple Bank. Ambiguity must be handled
//!   explicitly — no arbitrary entity is silently selected.
//! - QA2-RET-005 Conflicting evidence: both sides of a contradiction are
//!   surfaced with their own provenance, never silently hidden.
//!   (QA2-RET-008 multi-hop lives at the graph-engine boundary:
//!   crates/kernel/tests/qa2_retrieval.rs.)

use aikoql_ingestion::{
    compile_context, render_context_markdown, EntityCandidate, Evidence, FactCandidate, KnowledgeIr,
};

fn ev(doc: &str) -> Evidence {
    Evidence {
        document_id: Some(doc.into()),
        page: None,
        source: None,
        extractor: "qa2".into(),
        model: None,
        confidence: 0.9,
    }
}

fn ent(name: &str) -> EntityCandidate {
    EntityCandidate {
        name: name.into(),
        type_hint: Some("Organization".into()),
        mentions: vec![],
        confidence: 0.9,
        evidence: ev("corpus.md"),
    }
}

fn fact(statement: &str, entities: &[&str], doc: &str) -> FactCandidate {
    FactCandidate {
        statement: statement.into(),
        entities: entities.iter().map(|s| s.to_string()).collect(),
        confidence: 0.9,
        evidence: ev(doc),
        snippet: None,
    }
}

fn ir(entities: Vec<EntityCandidate>, facts: Vec<FactCandidate>) -> KnowledgeIr {
    KnowledgeIr {
        entities,
        relations: vec![],
        facts,
        events: vec![],
        temporal: vec![],
        document_id: Some("corpus.md".into()),
        source_revision: None,
        content_trust: None,
        page_count: 1,
        extractor: "qa2".into(),
    }
}

// ---------------------------------------------------------------------------
// QA2-RET-003 — entity ambiguity: no arbitrary silent selection
// ---------------------------------------------------------------------------

#[test]
fn w2_ret_003_entity_ambiguity_never_silently_selects_one() {
    let ir = ir(
        vec![ent("Apple Inc."), ent("Apple Records"), ent("Apple Bank")],
        vec![],
    );

    // Unbounded budget: every equally-matched candidate surfaces, ranked
    // by deterministic tie-break — never one arbitrary pick.
    let pkg = compile_context("What is Apple's revenue?", &ir, 0);
    let names: Vec<&str> = pkg.entities.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Apple Bank", "Apple Inc.", "Apple Records"],
        "all three candidates must be present, deterministically ordered"
    );
    let s0 = pkg.entities[0].score;
    assert!(s0 > 0.0);
    assert!(
        pkg.entities.iter().all(|e| e.score == s0),
        "equally-matched candidates must rank equally — no arbitrary winner"
    );

    // Tight budget (fits ~one entity under the pack cap): the tie group
    // cannot fit whole, so NONE of the candidates is packed — no arbitrary
    // single pick — and the whole group is surfaced as an explicit
    // ambiguity the caller can resolve instead of guessing.
    let tight = compile_context("What is Apple's revenue?", &ir, 40);
    assert!(
        tight.entities.is_empty(),
        "a tight budget must not pack one arbitrary apple candidate; got {:?}",
        tight
            .entities
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        tight.ambiguous_entities,
        vec![
            "Apple Bank".to_string(),
            "Apple Inc.".to_string(),
            "Apple Records".to_string()
        ],
        "the unresolved candidates must be named explicitly"
    );
    assert!(tight.trimmed, "the dropped group must set the trim flag");
    assert!(
        render_context_markdown(&tight).contains("Ambiguous Entities"),
        "the ambiguity must be visible in the rendered package"
    );
}

// ---------------------------------------------------------------------------
// QA2-RET-005 — conflicting evidence is surfaced, not hidden
// ---------------------------------------------------------------------------

#[test]
fn w2_ret_005_conflicting_evidence_is_surfaced_not_hidden() {
    let ir = ir(
        vec![ent("Team A"), ent("Team B")],
        vec![
            fact(
                "Team A owns the deployment pipeline",
                &["Team A"],
                "policy-a.md",
            ),
            fact(
                "Team B owns the deployment pipeline",
                &["Team B"],
                "policy-b.md",
            ),
        ],
    );

    let pkg = compile_context("Who owns the deployment pipeline?", &ir, 0);

    // Both sides of the contradiction appear — the retrieval layer must
    // never silently hide one side of a conflict.
    let stmts: Vec<&str> = pkg.facts.iter().map(|f| f.statement.as_str()).collect();
    assert!(
        stmts.contains(&"Team A owns the deployment pipeline"),
        "side A of the conflict must be surfaced; got {stmts:?}"
    );
    assert!(
        stmts.contains(&"Team B owns the deployment pipeline"),
        "side B of the conflict must be surfaced; got {stmts:?}"
    );

    // Each side carries its own provenance so the agent can adjudicate
    // instead of trusting an arbitrary winner.
    let docs: Vec<Option<&str>> = pkg
        .facts
        .iter()
        .map(|f| f.evidence.as_ref().and_then(|e| e.document_id.as_deref()))
        .collect();
    assert!(docs.contains(&Some("policy-a.md")), "provenance for side A");
    assert!(docs.contains(&Some("policy-b.md")), "provenance for side B");
}
