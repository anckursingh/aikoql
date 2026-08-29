//! Wave 3.1 (MVP-QA-003A) — W31-IMPACT-001 knowledge change propagation.
//!
//! The spec's scenario: a service's dependency changes — Aurora depends
//! on Beacon becomes Aurora depends on Cobalt. Re-ingest the edited doc,
//! recompile the same tasks, measure what the change touched.
//!
//! Fixture note: entity names are prefix-distinct (Aurora/Beacon/Cobalt/
//! Dune). The compiler's deliberate morphology rule (shared prefix ≥4
//! chars) would make every "ServiceX" rank for every service question —
//! that is its own feature, not propagation, and would blur the blast
//! radius measured here. Distinct names isolate edge-driven propagation.
//!
//! Ground truth declared BEFORE measurement (spec §4 — never adjusted):
//! - affected knowledge records: 2 — kb-a's dependency fact + dependency
//!   relation (computed by symmetric-diffing the two merged IRs);
//! - affected relationships: 1 edge (A→B replaced by A→C);
//! - affected contexts: T1, T2, T3 — each pre-change package references
//!   the changed records (T1/T2: edge + fact, T3: edge only; T3 ranks
//!   Beacon, whose incoming edge packs as its relation);
//! - affected answers: T1 (dependency target name flips), T2 (the edge,
//!   the boosted entity, and the delivered sla value all flip — the
//!   relationship-boost re-routes from Beacon to Cobalt);
//! - unaffected: T4 (Dune — no reference to kb-a in either version),
//!   and Beacon's own sla fact (a touched entity's *own* knowledge
//!   survives its neighbor's change).
//!
//! Pinned acceptance: precision 1.0, recall 1.0, false propagation 0,
//! missed propagation 0, stale-answer rate 0 (no post-change package may
//! still carry the old edge or the old dependency target).

mod common;

use aikoql_ingestion::{
    compile_context, merge_knowledge_ir, render_context_markdown, ContextPackage, EntityCandidate,
    Evidence, FactCandidate, KnowledgeIr, RelationCandidate,
};
use common::trackb::{assert_integrity, Doc};
use common::wave31_sim::BUDGET;

fn ev(doc: &str) -> Evidence {
    Evidence {
        document_id: Some(doc.into()),
        extractor: "w31-impact-synthetic".into(),
        confidence: 0.9,
        ..Evidence::default()
    }
}

fn entity(name: &str, ty: &str, doc: &str) -> EntityCandidate {
    EntityCandidate {
        name: name.into(),
        type_hint: Some(ty.into()),
        mentions: vec![name.into()],
        confidence: 0.9,
        evidence: ev(doc),
    }
}

fn fact(statement: &str, anchors: &[&str], doc: &str) -> FactCandidate {
    FactCandidate {
        statement: statement.into(),
        entities: anchors.iter().map(|s| s.to_string()).collect(),
        confidence: 0.9,
        evidence: ev(doc),
        snippet: None,
    }
}

fn rel(subject: &str, predicate: &str, object: &str, doc: &str) -> RelationCandidate {
    RelationCandidate {
        subject: subject.into(),
        predicate: predicate.into(),
        object: object.into(),
        confidence: 0.9,
        evidence: ev(doc),
    }
}

fn doc(id: &'static str, chunks: &'static [&'static str], ir: KnowledgeIr) -> Doc {
    Doc { id, chunks, ir }
}

/// World v1: Aurora → Beacon. World v2: Aurora → Cobalt. Else identical.
fn world(target: &'static str) -> Vec<Doc> {
    let a_chunk: &'static [&'static str] = match target {
        "Beacon" => &["Aurora depends on Beacon."],
        _ => &["Aurora depends on Cobalt."],
    };
    vec![
        doc(
            "kb-a",
            a_chunk,
            KnowledgeIr {
                entities: vec![entity("Aurora", "service", "kb-a")],
                facts: vec![fact(
                    &format!("Aurora depends on {target}."),
                    &["Aurora"],
                    "kb-a",
                )],
                relations: vec![rel("Aurora", "depends_on", target, "kb-a")],
                ..KnowledgeIr::default()
            },
        ),
        doc(
            "kb-b",
            &["Beacon sla is 99.9."],
            KnowledgeIr {
                entities: vec![entity("Beacon", "service", "kb-b")],
                facts: vec![fact("Beacon sla is 99.9.", &["Beacon"], "kb-b")],
                ..KnowledgeIr::default()
            },
        ),
        doc(
            "kb-c",
            &["Cobalt sla is 99.5."],
            KnowledgeIr {
                entities: vec![entity("Cobalt", "service", "kb-c")],
                facts: vec![fact("Cobalt sla is 99.5.", &["Cobalt"], "kb-c")],
                ..KnowledgeIr::default()
            },
        ),
        doc(
            "kb-d",
            &["Dune sla is 97.0."],
            KnowledgeIr {
                entities: vec![entity("Dune", "service", "kb-d")],
                facts: vec![fact("Dune sla is 97.0.", &["Dune"], "kb-d")],
                ..KnowledgeIr::default()
            },
        ),
    ]
}

/// Structure fingerprint of a package — what the change can touch:
/// entity names, relation triples, fact statements.
fn fp(p: &ContextPackage) -> (Vec<String>, Vec<(String, String, String)>, Vec<String>) {
    let mut es: Vec<String> = p.entities.iter().map(|e| e.name.clone()).collect();
    let mut rs: Vec<(String, String, String)> = p
        .relations
        .iter()
        .map(|r| (r.subject.clone(), r.predicate.clone(), r.object.clone()))
        .collect();
    let mut fs: Vec<String> = p.facts.iter().map(|f| f.statement.clone()).collect();
    es.sort();
    rs.sort();
    fs.sort();
    (es, rs, fs)
}

#[test]
fn w31_impact_001_knowledge_change_propagation() {
    let docs_v1 = world("Beacon");
    let docs_v2 = world("Cobalt");
    for docs in [docs_v1.as_slice(), docs_v2.as_slice()] {
        assert_integrity(
            docs,
            &merge_knowledge_ir(&docs.iter().map(|d| d.ir.clone()).collect::<Vec<_>>()),
        );
    }
    let merged_v1 = merge_knowledge_ir(&docs_v1.iter().map(|d| d.ir.clone()).collect::<Vec<_>>());
    let merged_v2 = merge_knowledge_ir(&docs_v2.iter().map(|d| d.ir.clone()).collect::<Vec<_>>());

    // ── Knowledge-record diff (symmetric: removed + added) ────────────────
    let facts_diff = merged_v1
        .facts
        .iter()
        .filter(|f| !merged_v2.facts.iter().any(|g| g.statement == f.statement))
        .count()
        + merged_v2
            .facts
            .iter()
            .filter(|f| !merged_v1.facts.iter().any(|g| g.statement == f.statement))
            .count();
    let rels_diff = merged_v1
        .relations
        .iter()
        .filter(|f| !merged_v2.relations.iter().any(|g| g.subject == f.subject && g.object == f.object && g.predicate == f.predicate))
        .count()
        + merged_v2
            .relations
            .iter()
            .filter(|f| !merged_v1.relations.iter().any(|g| g.subject == f.subject && g.object == f.object && g.predicate == f.predicate))
            .count();
    assert_eq!(facts_diff, 2, "one dependency fact replaced");
    assert_eq!(rels_diff, 2, "one dependency edge replaced");
    assert_eq!(
        merged_v1.entities.len(),
        merged_v2.entities.len(),
        "entity records untouched by the change"
    );

    // ── Per-task measurement ──────────────────────────────────────────────
    let tasks: [(&str, &str); 4] = [
        ("T1", "What does Aurora depend on?"),
        ("T2", "What is the sla of the service Aurora depends on?"),
        ("T3", "What is the sla of Beacon?"),
        ("T4", "What is the sla of Dune?"),
    ];
    let mut report = String::from("task | changed | pre  | post\n");
    report.push_str("-----|---------|------|------\n");
    let mut changed: Vec<&str> = Vec::new();
    for (name, text) in tasks {
        let p1 = compile_context(text, &merged_v1, BUDGET);
        let p2 = compile_context(text, &merged_v2, BUDGET);
        let (f1, f2) = (fp(&p1), fp(&p2));
        if f1 != f2 {
            changed.push(name);
        }
        let old_edge = f1.1.iter().any(|(s, _, o)| s == "Aurora" && o == "Beacon");
        let new_edge = f2.1.iter().any(|(s, _, o)| s == "Aurora" && o == "Cobalt");
        let edge_pre = if old_edge { "A→B" } else { "-" };
        let edge_post = if new_edge { "A→C" } else { "-" };
        report.push_str(&format!(
            "{name:<4} | {:<7} | {edge_pre:<4} | {edge_post}\n",
            if f1 != f2 { "yes" } else { "no" },
        ));

        // Pinned per-task laws.
        match name {
            "T1" => {
                // Answer flips: target name appears, old one is gone.
                let r1 = render_context_markdown(&p1).to_lowercase();
                let r2 = render_context_markdown(&p2).to_lowercase();
                assert!(r1.contains("beacon"), "T1 pre must answer Beacon");
                assert!(r2.contains("cobalt"), "T1 post must answer Cobalt");
                assert!(!r2.contains("beacon"), "T1 post must not name the old target");
            }
            "T2" => {
                // The relationship-boost re-routes: Beacon was the boosted
                // neighbor, Cobalt is. The delivered sla value flips with
                // it (the unboosted service's fact stays gated — no
                // exact-token escape: prefix-distinct names share no
                // second content token with the task).
                assert!(
                    f1.0.contains(&"Beacon".to_string()) && !f1.0.contains(&"Cobalt".to_string()),
                    "T2 pre boosts Beacon, not Cobalt"
                );
                assert!(
                    f2.0.contains(&"Cobalt".to_string()) && !f2.0.contains(&"Beacon".to_string()),
                    "T2 post boosts Cobalt, not Beacon"
                );
                let r1 = render_context_markdown(&p1).to_lowercase();
                let r2 = render_context_markdown(&p2).to_lowercase();
                assert!(r1.contains("99.9"), "T2 pre must deliver Beacon's sla");
                assert!(!r1.contains("99.5"), "T2 pre must not deliver Cobalt's sla");
                assert!(r2.contains("99.5"), "T2 post must deliver Cobalt's sla");
                assert!(!r2.contains("99.9"), "T2 post must not still deliver Beacon's sla");
            }
            "T3" => {
                // Context loses the incoming edge; Beacon's OWN fact
                // survives.
                assert!(old_edge, "T3 pre packs Beacon's incoming edge");
                assert!(!f2.1.iter().any(|(s, _, _)| s == "Aurora"), "T3 post drops the re-targeted edge");
                let r2 = render_context_markdown(&p2).to_lowercase();
                assert!(r2.contains("99.9"), "T3 post still answers Beacon's sla — neighbor change must not corrupt own facts");
            }
            "T4" => {
                // Unaffected control: byte-identical context AND payload.
                assert_eq!(f1, f2, "T4 structure changed — false propagation");
                assert_eq!(
                    render_context_markdown(&p1),
                    render_context_markdown(&p2),
                    "T4 rendered payload changed — false propagation"
                );
            }
            _ => unreachable!(),
        }
    }

    // ── Pinned metrics over the declared ground truth ─────────────────────
    let gt = ["T1", "T2", "T3"];
    let affected = changed.iter().filter(|t| gt.contains(t)).count();
    let precision = affected as f64 / changed.len().max(1) as f64;
    let recall = affected as f64 / gt.len() as f64;
    let false_prop: Vec<_> = changed.iter().filter(|t| !gt.contains(t)).collect();
    let missed: Vec<_> = gt.iter().filter(|t| !changed.contains(t)).collect();
    assert_eq!(precision, 1.0, "false propagation: {false_prop:?}");
    assert_eq!(recall, 1.0, "missed propagation: {missed:?}");

    // ── Stale-answer sweep: no post-change package may still carry the ─────
    // old edge or the old dependency target.
    let mut stale = 0usize;
    for (_, text) in tasks {
        let p = compile_context(text, &merged_v2, BUDGET);
        let f = fp(&p);
        if f.1.iter().any(|(s, _, o)| s == "Aurora" && o == "Beacon") {
            stale += 1;
        }
        if f.2.iter().any(|st| st.contains("depends on Beacon")) {
            stale += 1;
        }
    }
    assert_eq!(stale, 0, "post-change packages still carry the old dependency");

    println!(
        "\n[W31-IMPACT-001] dependency change Beacon → Cobalt:\n{}",
        report
    );
    println!(
        "KO diff: 1 fact + 1 relation replaced (2 records); entities untouched\n\
         precision {precision} recall {recall} false-prop {false_prop:?} missed {missed:?} \
         stale-answers {stale}"
    );
}
