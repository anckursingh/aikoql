//! Track-B corpus + question machinery, shared by `knowledge_bench.rs`
//! (Track-B) and `comparative_chatbot_bench.rs` (G11, chatbot suite §52).
//! The corpus design doc lives in `knowledge_bench.rs`; this module holds
//! the data, the integrity asserts, and the token-containment judge.
#![allow(dead_code)]

use aikoql_ingestion::{EntityCandidate, Evidence, FactCandidate, KnowledgeIr, RelationCandidate};

pub struct Doc {
    pub id: &'static str,
    pub chunks: &'static [&'static str],
    pub ir: KnowledgeIr,
}

fn ev(doc: &str) -> Evidence {
    Evidence {
        document_id: Some(doc.into()),
        extractor: "track-b-synthetic".into(),
        confidence: 0.9,
        ..Evidence::default()
    }
}

fn entity(name: &str, ty: &str, mention: &str, doc: &str) -> EntityCandidate {
    EntityCandidate {
        name: name.into(),
        type_hint: Some(ty.into()),
        mentions: vec![mention.into()],
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

/// The synthetic 15-document corpus: 9 knowledge docs + 6 keyword-rich
/// distractors. Every chunk text is chosen so the RAG-side overlaps
/// (or zero-overlaps) are exactly what the question rows intend — see
/// `knowledge_bench.rs` for the per-question analysis.
pub fn docs() -> Vec<Doc> {
    vec![
        Doc {
            id: "kb-payments",
            chunks: &[
                "PaymentService depends on RetryPolicy.",
                "Retry limit is 3 attempts.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("PaymentService", "Service", "PaymentService depends on RetryPolicy.", "kb-payments"),
                    entity("RetryPolicy", "Policy", "PaymentService depends on RetryPolicy.", "kb-payments"),
                ],
                facts: vec![fact("Retry limit is 3 attempts.", &["RetryPolicy"], "kb-payments")],
                relations: vec![rel("PaymentService", "depends_on", "RetryPolicy", "kb-payments")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-ledger",
            chunks: &[
                "The LedgerService writes ledger entries and is tested by the AuditTeam.",
                "Ledger entries require exactly one write.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("LedgerService", "Service", "The LedgerService writes ledger entries and is tested by the AuditTeam.", "kb-ledger"),
                    entity("AuditTeam", "Team", "The LedgerService writes ledger entries and is tested by the AuditTeam.", "kb-ledger"),
                ],
                facts: vec![fact("Ledger entries require exactly one write.", &["LedgerService"], "kb-ledger")],
                relations: vec![rel("LedgerService", "tested_by", "AuditTeam", "kb-ledger")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-audit",
            chunks: &[
                "AuditTeam reviews the ledger each quarter.",
                "Audit activities are mandated by SOX.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("AuditTeam", "Team", "AuditTeam reviews the ledger each quarter.", "kb-audit")],
                facts: vec![fact("Audit activities are mandated by SOX.", &["AuditTeam"], "kb-audit")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-warranty-a",
            chunks: &[
                "HomeAutomation carries a two year warranty. WarrantyPolicy depends on RepairVendor.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("HomeAutomation", "Product", "HomeAutomation carries a two year warranty.", "kb-warranty-a"),
                    entity("WarrantyPolicy", "Policy", "WarrantyPolicy depends on RepairVendor.", "kb-warranty-a"),
                ],
                facts: vec![],
                relations: vec![rel("WarrantyPolicy", "depends_on", "RepairVendor", "kb-warranty-a")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-warranty-b",
            chunks: &["RepairVendor ships replacements within 48 hours."],
            ir: KnowledgeIr {
                entities: vec![entity("RepairVendor", "Vendor", "RepairVendor ships replacements within 48 hours.", "kb-warranty-b")],
                facts: vec![fact("RepairVendor ships replacements within 48 hours.", &["RepairVendor"], "kb-warranty-b")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-payments-v2",
            chunks: &["Retry limit is 5 attempts."],
            ir: KnowledgeIr {
                entities: vec![],
                // Anchored to the kb-payments RetryPolicy: the v2 document
                // updates the same policy entity (cross-document anchoring).
                facts: vec![fact("Retry limit is 5 attempts.", &["RetryPolicy"], "kb-payments-v2")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-growth-a",
            chunks: &["ActiveUsers grew 40 percent in 2025."],
            ir: KnowledgeIr {
                entities: vec![entity("ActiveUsers", "Program", "ActiveUsers grew 40 percent in 2025.", "kb-growth-a")],
                facts: vec![fact("ActiveUsers grew 40 percent in 2025.", &["ActiveUsers"], "kb-growth-a")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-growth-b",
            chunks: &["ActiveUsers grew 20 percent in 2025."],
            ir: KnowledgeIr {
                entities: vec![entity("ActiveUsers", "Program", "ActiveUsers grew 20 percent in 2025.", "kb-growth-b")],
                facts: vec![fact("ActiveUsers grew 20 percent in 2025.", &["ActiveUsers"], "kb-growth-b")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-depth",
            chunks: &[
                "RootService depends on MiddlePolicy.",
                "MiddlePolicy references LeafRule.",
                "LeafRule sets ceiling at 99.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("RootService", "Service", "RootService depends on MiddlePolicy.", "kb-depth"),
                    // Deliberately a mention with no question-keyword
                    // overlap: the depth-2 probe requires the intermediate
                    // entity to have zero pre-boost score, or the single
                    // boost round would cascade through it.
                    entity("MiddlePolicy", "Policy", "MiddlePolicy references LeafRule.", "kb-depth"),
                    entity("LeafRule", "Rule", "LeafRule sets ceiling at 99.", "kb-depth"),
                ],
                facts: vec![fact("LeafRule sets ceiling at 99.", &["LeafRule"], "kb-depth")],
                relations: vec![
                    rel("RootService", "depends_on", "MiddlePolicy", "kb-depth"),
                    rel("MiddlePolicy", "depends_on", "LeafRule", "kb-depth"),
                ],
                ..KnowledgeIr::default()
            },
        },
        // ── keyword-rich distractors: share question vocabulary, carry no
        // answer units. They hoover RAG budget slots and test the entity
        // gate (AikoQL must not deliver their anchored facts when their
        // entity does not rank).
        Doc {
            id: "kb-net",
            chunks: &[
                "NetworkModule controls retry behavior.",
                "Retry storms cause cascading failures.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("NetworkModule", "Module", "NetworkModule controls retry behavior.", "kb-net")],
                facts: vec![
                    fact("NetworkModule controls retry behavior.", &["NetworkModule"], "kb-net"),
                    fact("Retry storms cause cascading failures.", &["NetworkModule"], "kb-net"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-fin",
            chunks: &["FinanceAnnex tracks ledger entries and intercompany transfers."],
            ir: KnowledgeIr {
                entities: vec![entity("FinanceAnnex", "Section", "FinanceAnnex tracks ledger entries and intercompany transfers.", "kb-fin")],
                facts: vec![fact("FinanceAnnex tracks ledger entries and intercompany transfers.", &["FinanceAnnex"], "kb-fin")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-ops",
            chunks: &["OperationsTeam auditors review quarterly ledger access logs."],
            ir: KnowledgeIr {
                entities: vec![entity("OperationsTeam", "Team", "OperationsTeam auditors review quarterly ledger access logs.", "kb-ops")],
                facts: vec![fact("OperationsTeam auditors review quarterly ledger access logs.", &["OperationsTeam"], "kb-ops")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-sec",
            chunks: &["PCIPolicy governs payment card handling."],
            ir: KnowledgeIr {
                entities: vec![entity("PCIPolicy", "Policy", "PCIPolicy governs payment card handling.", "kb-sec")],
                facts: vec![fact("PCIPolicy governs payment card handling.", &["PCIPolicy"], "kb-sec")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-api",
            chunks: &["ApiGateway charges fees per transaction after retry attempts fail."],
            ir: KnowledgeIr {
                entities: vec![entity("ApiGateway", "Service", "ApiGateway charges fees per transaction after retry attempts fail.", "kb-api")],
                facts: vec![fact("ApiGateway charges fees per transaction after retry attempts fail.", &["ApiGateway"], "kb-api")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-mkt",
            chunks: &["LoyaltyProgram active users grew after the warranty campaign."],
            ir: KnowledgeIr {
                entities: vec![entity("LoyaltyProgram", "Program", "LoyaltyProgram active users grew after the warranty campaign.", "kb-mkt")],
                facts: vec![fact("LoyaltyProgram active users grew after the warranty campaign.", &["LoyaltyProgram"], "kb-mkt")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
    ]
}

#[derive(Clone)]
pub struct Question {
    pub text: &'static str,
    pub kind: &'static str,
    /// W3-MKT-001 workload class (Wave 3 plan §8: W1 lookup, W2 semantic,
    /// W3 synthesis, W4 multi-hop, W5 temporal, W6 contradiction, W7
    /// provenance, W8 personal memory, W9 policy/constraint, W10 agent
    /// planning, W11 unknown handling, W12 longitudinal).
    pub class: &'static str,
    pub units: [&'static str; 2],
}

pub const QUESTIONS: &[Question] = &[
    Question {
        kind: "hop",
        class: "W4",
        text: "Why does the PaymentService stop charging after repeated failures?",
        units: [
            "PaymentService depends_on RetryPolicy",
            "Retry limit is 3 attempts.",
        ],
    },
    Question {
        kind: "hop",
        class: "W4",
        text: "What law governs the auditors of the ledger?",
        units: [
            "LedgerService tested_by AuditTeam",
            "Audit activities are mandated by SOX.",
        ],
    },
    Question {
        kind: "cross-doc",
        class: "W4",
        text: "What warranty does the HomeAutomation carry?",
        units: [
            "WarrantyPolicy depends_on RepairVendor",
            "RepairVendor ships replacements within 48 hours.",
        ],
    },
    Question {
        kind: "temporal-probe",
        class: "W5",
        text: "What is the current retry limit?",
        units: ["Retry limit is 5 attempts.", "Retry limit is 3 attempts."],
    },
    Question {
        kind: "contradiction-probe",
        class: "W6",
        text: "How much did active users grow in 2025?",
        units: [
            "ActiveUsers grew 40 percent in 2025.",
            "ActiveUsers grew 20 percent in 2025.",
        ],
    },
    Question {
        kind: "control",
        class: "W1",
        text: "What writes ledger entries?",
        units: [
            "Ledger entries require exactly one write.",
            "LedgerService tested_by AuditTeam",
        ],
    },
    Question {
        kind: "depth-2-probe",
        class: "W4",
        text: "What does the RootService ultimately depend on?",
        units: [
            "MiddlePolicy depends_on LeafRule",
            "LeafRule sets ceiling at 99.",
        ],
    },
];

/// W3-MKT-001: the market-reality extension — engineering-knowledge docs
/// (architecture timeline, incidents with customer impact, a service
/// dependency chain, a policy/ADR/issue conflict set) and the market
/// workload questions over them. Kept separate from `docs()`/`QUESTIONS`
/// so the pinned Track-B and G11 benches keep their exact corpus; the
/// Wave 3 win-zone bench runs the union.
pub fn market_docs() -> Vec<Doc> {
    vec![
        Doc {
            id: "kb-arch",
            chunks: &[
                "ArchV1 handled payments from January through February.",
                "ArchV2 replaced ArchV1 in March and added the retry cache.",
                "ArchV3 replaced ArchV2 in June and is the current architecture.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("ArchV1", "Architecture", "ArchV1 handled payments from January through February.", "kb-arch"),
                    entity("ArchV2", "Architecture", "ArchV2 replaced ArchV1 in March and added the retry cache.", "kb-arch"),
                    entity("ArchV3", "Architecture", "ArchV3 replaced ArchV2 in June and is the current architecture.", "kb-arch"),
                ],
                facts: vec![
                    fact("ArchV1 handled payments from January through February.", &["ArchV1"], "kb-arch"),
                    fact("ArchV2 replaced ArchV1 in March and added the retry cache.", &["ArchV2"], "kb-arch"),
                    fact("ArchV3 replaced ArchV2 in June and is the current architecture.", &["ArchV3"], "kb-arch"),
                ],
                relations: vec![
                    rel("ArchV2", "replaced", "ArchV1", "kb-arch"),
                    rel("ArchV3", "replaced", "ArchV2", "kb-arch"),
                ],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-incident",
            chunks: &[
                "The FebruaryOutage hit BillingCustomers while ArchV1 was the active architecture.",
                "The FebruaryOutage was caused by a dependency upgrade.",
                "The rollout of ArchV3 fixed the FebruaryOutage for BillingCustomers.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("FebruaryOutage", "Incident", "The FebruaryOutage hit BillingCustomers while ArchV1 was the active architecture.", "kb-incident"),
                    entity("BillingCustomers", "Customer", "The FebruaryOutage hit BillingCustomers while ArchV1 was the active architecture.", "kb-incident"),
                ],
                facts: vec![
                    fact("The FebruaryOutage hit BillingCustomers while ArchV1 was the active architecture.", &["FebruaryOutage"], "kb-incident"),
                    fact("The FebruaryOutage was caused by a dependency upgrade.", &["FebruaryOutage"], "kb-incident"),
                    fact("The rollout of ArchV3 fixed the FebruaryOutage for BillingCustomers.", &["ArchV3"], "kb-incident"),
                ],
                relations: vec![
                    rel("FebruaryOutage", "affected", "BillingCustomers", "kb-incident"),
                    rel("FebruaryOutage", "occurred_during", "ArchV1", "kb-incident"),
                ],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-deps",
            chunks: &[
                "CheckoutService depends on PaymentService.",
                "The dependency upgrade broke CheckoutService for PremiumCustomers.",
                "PremiumCustomers reported failed checkouts during the incident.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("CheckoutService", "Service", "CheckoutService depends on PaymentService.", "kb-deps"),
                    entity("PremiumCustomers", "Customer", "The dependency upgrade broke CheckoutService for PremiumCustomers.", "kb-deps"),
                ],
                facts: vec![
                    fact("The dependency upgrade broke CheckoutService for PremiumCustomers.", &["CheckoutService"], "kb-deps"),
                    fact("PremiumCustomers reported failed checkouts during the incident.", &["PremiumCustomers"], "kb-deps"),
                ],
                relations: vec![rel("CheckoutService", "depends_on", "PaymentService", "kb-deps")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-policy",
            chunks: &[
                "The DeployPolicy requires zero-downtime deploys.",
                "The ArchitectureDecision mandates canary deploys for all services.",
                "The OldIssueNote suggests restarting the service during deploys.",
                "The DeployPolicy supersedes the OldIssueNote.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("DeployPolicy", "Policy", "The DeployPolicy requires zero-downtime deploys.", "kb-policy"),
                    entity("ArchitectureDecision", "Decision", "The ArchitectureDecision mandates canary deploys for all services.", "kb-policy"),
                    entity("OldIssueNote", "Issue", "The OldIssueNote suggests restarting the service during deploys.", "kb-policy"),
                ],
                facts: vec![
                    fact("The DeployPolicy requires zero-downtime deploys.", &["DeployPolicy"], "kb-policy"),
                    fact("The ArchitectureDecision mandates canary deploys for all services.", &["ArchitectureDecision"], "kb-policy"),
                    fact("The OldIssueNote suggests restarting the service during deploys.", &["OldIssueNote"], "kb-policy"),
                    fact("The DeployPolicy supersedes the OldIssueNote.", &["DeployPolicy"], "kb-policy"),
                ],
                relations: vec![rel("DeployPolicy", "supersedes", "OldIssueNote", "kb-policy")],
                ..KnowledgeIr::default()
            },
        },
    ]
}

/// The market workload questions (Wave 3 plan §8). Two probes invert the
/// standard unit counting — see `wave3_market_reality.rs`:
/// - `unknown-probe` (W11): the units are TRAPS. Correct = deliver neither
///   (the mechanical payload-level false-confidence rate; the refusal
///   phrase itself is chatbot-layer, unmeasurable here).
/// - `semantic-probe` (W2): zero lexical overlap by construction; both
///   mechanical treatments are expected to miss (no embedding model on
///   either path) — the honest semantic gap, kept as negative evidence.
pub const MARKET_QUESTIONS: &[Question] = &[
    Question {
        kind: "temporal-probe",
        class: "W5",
        text: "Which architecture was active in February, and which is current?",
        units: [
            "The FebruaryOutage hit BillingCustomers while ArchV1 was the active architecture.",
            "ArchV3 replaced ArchV2 in June and is the current architecture.",
        ],
    },
    Question {
        kind: "synthesis",
        class: "W3",
        text: "What broke for PremiumCustomers and what caused it?",
        units: [
            "PremiumCustomers reported failed checkouts during the incident.",
            "The dependency upgrade broke CheckoutService for PremiumCustomers.",
        ],
    },
    Question {
        kind: "policy",
        class: "W9",
        text: "What must deploys satisfy per the policy and architecture decision?",
        units: [
            "The DeployPolicy requires zero-downtime deploys.",
            "The ArchitectureDecision mandates canary deploys for all services.",
        ],
    },
    Question {
        kind: "provenance",
        class: "W7",
        text: "What does the DeployPolicy require for deploys, and where does it come from?",
        units: [
            "The DeployPolicy requires zero-downtime deploys.",
            "kb-policy",
        ],
    },
    Question {
        kind: "unknown-probe",
        class: "W11",
        text: "What is the rollback procedure for failed deploys?",
        units: [
            "The OldIssueNote suggests restarting the service during deploys.",
            "ArchV2 replaced ArchV1 in March and added the retry cache.",
        ],
    },
    Question {
        kind: "semantic-probe",
        class: "W2",
        text: "Who gets parts to buyers quickly?",
        units: [
            "RepairVendor ships replacements within 48 hours.",
            "WarrantyPolicy depends on RepairVendor",
        ],
    },
];

/// A unit is delivered when every token of the unit string occurs in the
/// payload (token containment — the G12 answer-hit definition).
pub fn unit_hit(delivered: &str, unit: &str) -> bool {
    let d = super::tokens(delivered);
    super::tokens(unit).iter().all(|t| d.contains(t))
}

pub fn units_hit(delivered: &str, q: &Question) -> (usize, [bool; 2]) {
    let hits = [
        unit_hit(delivered, q.units[0]),
        unit_hit(delivered, q.units[1]),
    ];
    (hits.iter().filter(|h| **h).count(), hits)
}

/// The corpus as RAG-readable chunks, in deterministic doc order.
pub fn corpus(docs: &[Doc]) -> Vec<super::CorpusChunk<'static>> {
    docs.iter()
        .flat_map(|d| {
            d.chunks
                .iter()
                .enumerate()
                .map(|(i, c)| (d.id, i, c.to_string()))
        })
        .collect()
}

/// Fairness: AikoQL gets no knowledge RAG could not in principle have
/// retrieved from the same text — every fact appears verbatim in a chunk
/// of its document, every entity name appears in a chunk, every relation's
/// endpoints co-occur in a chunk. Then the merged-graph anchors and
/// relation endpoints must resolve (or the compiler gate silently drops
/// them).
pub fn assert_integrity(docs: &[Doc], merged: &KnowledgeIr) {
    for d in docs {
        for f in &d.ir.facts {
            let backing = d.chunks.iter().any(|c| {
                super::tokens(&f.statement)
                    .iter()
                    .all(|t| super::tokens(c).contains(t))
            });
            assert!(
                backing,
                "fact '{}' has no verbatim backing chunk in {}",
                f.statement, d.id
            );
        }
        for e in &d.ir.entities {
            assert!(
                d.chunks
                    .iter()
                    .any(|c| c.to_lowercase().contains(&e.name.to_lowercase())),
                "entity '{}' never appears in a chunk of {}",
                e.name,
                d.id
            );
        }
        for r in &d.ir.relations {
            assert!(
                d.chunks.iter().any(|c| {
                    let t = super::tokens(c);
                    t.contains(&r.subject.to_lowercase()) && t.contains(&r.object.to_lowercase())
                }),
                "relation {}-{} has no co-mention basis chunk in {}",
                r.subject,
                r.object,
                d.id
            );
        }
    }
    let names: std::collections::HashSet<&str> =
        merged.entities.iter().map(|e| e.name.as_str()).collect();
    for f in &merged.facts {
        assert!(
            f.entities.iter().all(|en| names.contains(en.as_str())),
            "fact '{}' anchors to an unknown entity",
            f.statement
        );
    }
    for r in &merged.relations {
        assert!(
            names.contains(r.subject.as_str()) && names.contains(r.object.as_str()),
            "relation {} -> {} has an unknown endpoint",
            r.subject,
            r.object
        );
    }
}
