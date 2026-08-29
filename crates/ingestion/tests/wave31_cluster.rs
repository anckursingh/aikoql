//! Wave 3.1 (MVP-QA-003A) — W31-CLUSTER-001/002 cluster-level precision
//! trimming (plan item 12 / TP-7 item 3).
//!
//! Loss rows: W1 control packs 185 vs RAG 70 tokens, W9 policy 354 vs
//! 298 for identical scores — entity clusters pack whole. Measured
//! resolution: the RELATION channel ships (relations pack above half
//! the top relation's score); the FACTS channel is a documented
//! negative result (the W1 Q17 secondary unit sits inside the W9 noise
//! band — no lexical threshold separates them; losses.md).
//!
//! TDD order (spec §4): baseline composition measured first (001
//! prints, asserts nothing), then the floor contract declared, then
//! RED → fix → GREEN. 002 pins the shipped relation floor; the fact
//! floor's measurement history is in its doc comment.

mod common;

use aikoql_ingestion::{
    compile_context, merge_knowledge_ir, render_context_markdown, EntityCandidate, Evidence,
    FactCandidate, KnowledgeIr,
};
use common::wave31_sim::{payload_has, union_docs};

const BUDGET: usize = 300;

/// The exact probes whose token costs the loss rows record.
const W1: &str = "What writes ledger entries?";
const W9: &str = "What must deploys satisfy per the policy and architecture decision?";
/// W1 lookup whose rag baseline costs 28 tokens (Q13, 243 vs 28).
const W1_LOOKUP: &str = "How fast does StandardTier respond versus PriorityTier?";
/// W1 lookup at 340 vs 297 (Q15).
const W1_ONCALL: &str = "Who runs the oncall rotation on Mondays?";
/// W4 depth-2 (Q0): the relation and the leaf fact are both units.
const W4_HOP: &str = "What does the RootService ultimately depend on?";

fn evd() -> Evidence {
    Evidence {
        document_id: Some("cluster-synth".into()),
        ..Default::default()
    }
}

fn ent(name: &str, type_hint: &str, mentions: &[&str]) -> EntityCandidate {
    EntityCandidate {
        name: name.into(),
        type_hint: Some(type_hint.into()),
        mentions: mentions.iter().map(|m| (*m).into()).collect(),
        confidence: 0.9,
        evidence: evd(),
    }
}

fn fac(stmt: &str, entities: &[&str]) -> FactCandidate {
    FactCandidate {
        statement: stmt.into(),
        entities: entities.iter().map(|e| (*e).into()).collect(),
        confidence: 0.9,
        evidence: evd(),
        snippet: None,
    }
}

/// The frozen w3_unk_001 rollback IR (same facts/entities as the pin).
fn rollback_ir() -> KnowledgeIr {
    KnowledgeIr {
        entities: vec![
            ent("RollbackProcedure", "Procedure", &["rollback"]),
            ent("Deploy", "Process", &["deploy"]),
        ],
        facts: vec![
            fac("The rollback procedure is to redeploy the previous version.", &["RollbackProcedure"]),
            fac("Rollback is immediate.", &["RollbackProcedure"]),
            fac("Rollback is scheduled.", &["RollbackProcedure"]),
            fac("Old deploys used blue-green switching.", &["Deploy"]),
            fac("Deploys use canary releases.", &["Deploy"]),
        ],
        ..Default::default()
    }
}

/// The frozen w3_temp_001 IR (same facts/entities as the pin).
fn temp_ir() -> KnowledgeIr {
    KnowledgeIr {
        entities: vec![
            ent("ArchV1", "Architecture", &[]),
            ent("ArchV2", "Architecture", &[]),
            ent("ArchV3", "Architecture", &[]),
            ent("FebruaryOutage", "Incident", &["payments outage"]),
        ],
        facts: vec![
            fac("ArchV1 is the deployed architecture.", &["ArchV1"]),
            fac("ArchV2 is the deployed architecture.", &["ArchV2"]),
            fac("ArchV3 is the deployed architecture.", &["ArchV3"]),
            fac(
                "The FebruaryOutage happened while ArchV1 was deployed.",
                &["FebruaryOutage", "ArchV1"],
            ),
        ],
        ..Default::default()
    }
}

fn dump(label: &str, probe: &str, ir: &KnowledgeIr) {
    let pkg = compile_context(probe, ir, BUDGET);
    let payload = render_context_markdown(&pkg);
    if label == "W4-hop" {
        eprintln!("[W31-CLUSTER W4-hop PAYLOAD]\n{payload}");
    }
    eprintln!(
        "[W31-CLUSTER {label}] \"{probe}\" tokens={} status={:?}",
        payload.len() / 4,
        pkg.status
    );
    for e in &pkg.entities {
        eprintln!(
            "[W31-CLUSTER {label}] entity name={} score={} type={:?} mentions={}",
            e.name, e.score, e.type_hint, e.mentions.len()
        );
    }
    for f in &pkg.facts {
        eprintln!("[W31-CLUSTER {label}] fact score={} stmt={}", f.score, f.statement);
    }
    for r in &pkg.relations {
        eprintln!(
            "[W31-CLUSTER {label}] rel score={} {} {} {}",
            r.score, r.subject, r.predicate, r.object
        );
    }
}

fn union_merged() -> KnowledgeIr {
    let docs = union_docs();
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    merge_knowledge_ir(&irs)
}

#[test]
fn w31_cluster_001_baseline_composition() {
    let merged = union_merged();

    for (label, probe) in [
        ("W1", W1),
        ("W9", W9),
        ("W1-lookup", W1_LOOKUP),
        ("W1-oncall", W1_ONCALL),
        ("W4-hop", W4_HOP),
    ] {
        dump(label, probe, &merged);
    }
    dump("w3-unk-known", "What is the rollback procedure?", &rollback_ir());
    dump("w3-unk-conflict", "How is rollback done?", &rollback_ir());
    dump(
        "w3-temp",
        "Which architecture was deployed during the FebruaryOutage?",
        &temp_ir(),
    );
}

/// W31-CLUSTER-002 — the relation relevance floor pin (plan item 12).
///
/// Contract, declared with the fix (spec §4): the answer units pack,
/// and the identified tail relation edges do not — relations below half
/// the top relation's score are cluster edges, not answers (the
/// W1-lookup DutyManager edges, the W1-oncall escalates/conflicts
/// edges, the W4-hop 0.315 depends_on tail).
///
/// Honest record (2026-08-29): the first RED draft of this pin also
/// declared a FACTS floor — sub-band keyword-matched facts (half the
/// top fact, absolute 1.4, statement-score ≥1.0 exempt) with token
/// bounds 200/160/260/230. Two measurements killed it:
/// 1. The token bounds were a pre-measurement estimate of the floor's
///    effect and the estimate was wrong — the freed budget refills
///    with exempt entity-connected facts, so token counts moved less
///    than predicted (W9 354→340, W1 185→169, W1-oncall 340→263,
///    W1-lookup 243→221).
/// 2. The fact floor broke the frozen W31-COMP-001 W1 full-parity
///    assert: the W1 class's secondary unit "An SLA breach earns
///    customers a 10 percent service credit." (Q17, score ~1.2) sits
///    INSIDE the W9 noise band (1.165–1.33) — no lexical threshold
///    separates them. The fact floor was reverted; the negative result
///    is recorded in losses.md, not hidden.
#[test]
fn w31_cluster_002_relevance_floor() {
    let merged = union_merged();

    struct Probe {
        text: &'static str,
        units: &'static [&'static str],
        /// Relation triples the floor must keep out.
        noise_rels: &'static [(&'static str, &'static str, &'static str)],
    }
    let probes: [Probe; 3] = [
        Probe {
            text: W1_ONCALL,
            units: &[
                "Alex runs the OncallRotation on Mondays and Wednesdays.",
                "Priya runs the OncallRotation on Tuesdays and Thursdays.",
            ],
            noise_rels: &[
                ("PriorityTier", "escalates_to", "DutyManager"),
                ("SchedulingMemo", "conflicts_with", "TeamWiki"),
            ],
        },
        Probe {
            text: W1_LOOKUP,
            units: &[
                "PriorityTier responds within 4 hours and escalates to DutyManager.",
                "StandardTier responds within 24 business hours.",
            ],
            noise_rels: &[("DutyManager", "covers", "OncallRotation")],
        },
        Probe {
            text: W4_HOP,
            // Only the relation unit — "LeafRule sets ceiling at 99."
            // never packed here, pre or post floor (the G11 depth-2
            // ceiling, losses.md; the battery's Q6 is 1/2 both ways).
            units: &["MiddlePolicy depends_on LeafRule"],
            noise_rels: &[
                ("CheckoutService", "depends_on", "PaymentService"),
                ("FraudEngine", "depends_on", "RiskRules"),
                ("PaymentService", "depends_on", "RetryPolicy"),
                ("WarrantyPolicy", "depends_on", "RepairVendor"),
            ],
        },
    ];
    for p in &probes {
        let pkg = compile_context(p.text, &merged, BUDGET);
        let payload = render_context_markdown(&pkg);
        eprintln!("[W31-CLUSTER-002] \"{}\" tokens={}", p.text, payload.len() / 4);
        for u in p.units {
            assert!(
                payload_has(&payload, u),
                "\"{}\": unit missing: {u}\n{payload}",
                p.text
            );
        }
        for (s, pr, o) in p.noise_rels {
            assert!(
                !pkg.relations.iter().any(|r| {
                    r.subject == *s && r.predicate == *pr && r.object == *o
                }),
                "\"{}\": noise relation packed: {s} {pr} {o}\n{payload}",
                p.text
            );
        }
    }

    // The top relation of the depth-2 probe must survive the floor —
    // the unit relation is the band's own anchor (floor = its half).
    let hop = compile_context(
        "Why does the PaymentService stop charging after repeated failures?",
        &merged,
        BUDGET,
    );
    let hop_payload = render_context_markdown(&hop);
    assert!(
        payload_has(&hop_payload, "PaymentService depends_on RetryPolicy"),
        "depth-2 relation unit missing:\n{hop_payload}"
    );
    assert!(
        payload_has(&hop_payload, "Retry limit is 3 attempts."),
        "depth-2 leaf fact unit missing:\n{hop_payload}"
    );
}
