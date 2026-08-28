//! Wave 3.1 (MVP-QA-003A) — the frozen holdout corpus (spec §7).
//!
//! A deliberately different domain (Northwind Logistics, a fleet
//! operator) from the development corpus (a payments/SaaS platform).
//! The holdout is never scored during development: `wave31_market.rs`
//! asserts structure only (integrity, disjoint ids, ≥20 tasks). The one
//! evaluation pass happens in the Wave 3.1 comparison harness (#161),
//! with frozen machinery, and its printed results are pinned into the
//! evidence docs. Nothing in this module may change after that pin
//! without invalidating the frozen holdout.
#![allow(dead_code)]

use aikoql_ingestion::{EntityCandidate, Evidence, FactCandidate, KnowledgeIr, RelationCandidate};

use super::trackb::{g, Doc, Question};

fn ev(doc: &str) -> Evidence {
    Evidence {
        document_id: Some(doc.into()),
        extractor: "w31-holdout-synthetic".into(),
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

pub fn holdout_docs() -> Vec<Doc> {
    vec![
        Doc {
            id: "ho-route",
            chunks: &[
                "RouteDelta serves the North corridor.",
                "RouteDelta has 14 stops.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("RouteDelta", "Route", "RouteDelta serves the North corridor.", "ho-route")],
                facts: vec![
                    fact("RouteDelta serves the North corridor.", &["RouteDelta"], "ho-route"),
                    fact("RouteDelta has 14 stops.", &["RouteDelta"], "ho-route"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "ho-route-update",
            chunks: &["RouteDelta was rerouted to the East corridor in August."],
            ir: KnowledgeIr {
                entities: vec![entity("RouteDelta", "Route", "RouteDelta was rerouted to the East corridor in August.", "ho-route-update")],
                facts: vec![fact("RouteDelta was rerouted to the East corridor in August.", &["RouteDelta"], "ho-route-update")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "ho-route-notes",
            chunks: &[
                "The DispatchNotes say RouteDelta serves the East corridor.",
                "The DispatchNotes conflict with the RouteSheet.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("DispatchNotes", "Note", "The DispatchNotes say RouteDelta serves the East corridor.", "ho-route-notes"),
                    entity("RouteSheet", "Document", "The DispatchNotes conflict with the RouteSheet.", "ho-route-notes"),
                    entity("RouteDelta", "Route", "The DispatchNotes say RouteDelta serves the East corridor.", "ho-route-notes"),
                ],
                facts: vec![
                    fact("The DispatchNotes say RouteDelta serves the East corridor.", &["DispatchNotes"], "ho-route-notes"),
                    fact("The DispatchNotes conflict with the RouteSheet.", &["DispatchNotes"], "ho-route-notes"),
                ],
                relations: vec![rel("DispatchNotes", "conflicts_with", "RouteSheet", "ho-route-notes")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "ho-driver",
            chunks: &[
                "DriverMax holds a Class A license.",
                "DriverMax is assigned to RouteDelta.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("DriverMax", "Driver", "DriverMax holds a Class A license.", "ho-driver"),
                    entity("RouteDelta", "Route", "DriverMax is assigned to RouteDelta.", "ho-driver"),
                ],
                facts: vec![
                    fact("DriverMax holds a Class A license.", &["DriverMax"], "ho-driver"),
                    fact("DriverMax is assigned to RouteDelta.", &["DriverMax"], "ho-driver"),
                ],
                relations: vec![rel("DriverMax", "assigned_to", "RouteDelta", "ho-driver")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "ho-compliance",
            chunks: &[
                "ElkLogPolicy limits driving to 10 hours per day.",
                "The HubCheckpoint verifies the ElkLogPolicy log each morning.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("ElkLogPolicy", "Policy", "ElkLogPolicy limits driving to 10 hours per day.", "ho-compliance"),
                    entity("HubCheckpoint", "Process", "The HubCheckpoint verifies the ElkLogPolicy log each morning.", "ho-compliance"),
                ],
                facts: vec![
                    fact("ElkLogPolicy limits driving to 10 hours per day.", &["ElkLogPolicy"], "ho-compliance"),
                    fact("The HubCheckpoint verifies the ElkLogPolicy log each morning.", &["HubCheckpoint"], "ho-compliance"),
                ],
                relations: vec![rel("HubCheckpoint", "verifies", "ElkLogPolicy", "ho-compliance")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "ho-widgets",
            chunks: &[
                "The Widget shipment arrives on Fridays.",
                "Widget shipments weigh 2 tons each.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("Widget", "Product", "The Widget shipment arrives on Fridays.", "ho-widgets")],
                facts: vec![
                    fact("The Widget shipment arrives on Fridays.", &["Widget"], "ho-widgets"),
                    fact("Widget shipments weigh 2 tons each.", &["Widget"], "ho-widgets"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
    ]
}

/// 24 holdout tasks. Structure mirrors the development corpus (same
/// classes, same gt contract); content is disjoint. Frozen: do not edit
/// after the #161 evaluation pin without documenting the invalidation.
pub const HOLDOUT_QUESTIONS: &[Question] = &[
    Question {
        text: "How many stops does RouteDelta have?",
        kind: "lookup",
        class: "W1",
        units: ["RouteDelta has 14 stops.", "RouteDelta serves the North corridor."],
        gt: g("none", "ho-route", "none", "current", "documentation", "none"),
    },
    Question {
        text: "What license does DriverMax hold?",
        kind: "lookup",
        class: "W1",
        units: ["DriverMax holds a Class A license.", "DriverMax is assigned to RouteDelta."],
        gt: g("none", "ho-driver", "DriverMax assigned_to RouteDelta", "current", "documentation", "none"),
    },
    Question {
        text: "When does the Widget shipment arrive?",
        kind: "lookup",
        class: "W1",
        units: ["The Widget shipment arrives on Fridays.", "Widget shipments weigh 2 tons each."],
        gt: g("none", "ho-widgets", "none", "current", "documentation", "none"),
    },
    Question {
        text: "How much freight moves per delivery?",
        kind: "semantic-probe",
        class: "W2",
        units: ["Widget shipments weigh 2 tons each.", "The Widget shipment arrives on Fridays."],
        gt: g("none", "ho-widgets", "none", "current", "documentation", "none"),
    },
    Question {
        text: "Chauffeur credential type?",
        kind: "semantic-probe",
        class: "W2",
        units: ["DriverMax holds a Class A license.", "DriverMax is assigned to RouteDelta."],
        gt: g("none", "ho-driver", "DriverMax assigned_to RouteDelta", "current", "documentation", "none"),
    },
    Question {
        text: "Which corridor does RouteDelta serve now, and when did it change?",
        kind: "synthesis",
        class: "W3",
        units: ["RouteDelta was rerouted to the East corridor in August.", "RouteDelta serves the North corridor."],
        gt: g("none", "any", "none", "mixed", "documentation", "none"),
    },
    Question {
        text: "What does the HubCheckpoint verify, and what rule applies?",
        kind: "synthesis",
        class: "W3",
        units: ["The HubCheckpoint verifies the ElkLogPolicy log each morning.", "ElkLogPolicy limits driving to 10 hours per day."],
        gt: g("none", "ho-compliance", "HubCheckpoint verifies ElkLogPolicy", "current", "organization_policy", "none"),
    },
    Question {
        text: "Who is assigned to the rerouted route?",
        kind: "cross-doc",
        class: "W4",
        units: ["RouteDelta was rerouted to the East corridor in August.", "DriverMax is assigned to RouteDelta."],
        gt: g("none", "any", "DriverMax assigned_to RouteDelta", "mixed", "documentation", "none"),
    },
    Question {
        text: "What limits the assigned driver?",
        kind: "hop",
        class: "W4",
        units: ["DriverMax is assigned to RouteDelta.", "ElkLogPolicy limits driving to 10 hours per day."],
        gt: g("none", "any", "DriverMax assigned_to RouteDelta", "current", "organization_policy", "none"),
    },
    Question {
        text: "What was RouteDelta's corridor before August?",
        kind: "temporal-probe",
        class: "W5",
        units: ["RouteDelta serves the North corridor.", "RouteDelta was rerouted to the East corridor in August."],
        gt: g("none", "any", "none", "historical", "documentation", "none"),
    },
    Question {
        text: "What changed for RouteDelta in August?",
        kind: "temporal-probe",
        class: "W5",
        units: ["RouteDelta was rerouted to the East corridor in August.", "RouteDelta serves the North corridor."],
        gt: g("none", "any", "none", "mixed", "documentation", "none"),
    },
    Question {
        text: "Which corridor do the DispatchNotes claim?",
        kind: "contradiction-probe",
        class: "W6",
        units: ["The DispatchNotes say RouteDelta serves the East corridor.", "RouteDelta serves the North corridor."],
        gt: g("either documented corridor; both sources present", "any", "DispatchNotes conflicts_with RouteSheet", "current", "documentation", "conflict"),
    },
    Question {
        text: "What do the DispatchNotes conflict with?",
        kind: "contradiction-probe",
        class: "W6",
        units: ["The DispatchNotes conflict with the RouteSheet.", "The DispatchNotes say RouteDelta serves the East corridor."],
        gt: g("none", "ho-route-notes", "DispatchNotes conflicts_with RouteSheet", "current", "documentation", "conflict"),
    },
    Question {
        text: "Which document says the East corridor?",
        kind: "provenance",
        class: "W7",
        units: ["The DispatchNotes say RouteDelta serves the East corridor.", "ho-route-notes"],
        gt: g("none", "ho-route-notes", "DispatchNotes conflicts_with RouteSheet", "current", "documentation", "conflict"),
    },
    Question {
        text: "Which document limits driving hours?",
        kind: "provenance",
        class: "W7",
        units: ["ElkLogPolicy limits driving to 10 hours per day.", "ho-compliance"],
        gt: g("none", "ho-compliance", "none", "current", "organization_policy", "none"),
    },
    Question {
        text: "Which route is DriverMax assigned to?",
        kind: "personal",
        class: "W8",
        units: ["DriverMax is assigned to RouteDelta.", "DriverMax holds a Class A license."],
        gt: g("none", "ho-driver", "DriverMax assigned_to RouteDelta", "current", "documentation", "none"),
    },
    Question {
        text: "Which driver is on RouteDelta?",
        kind: "personal",
        class: "W8",
        units: ["DriverMax is assigned to RouteDelta.", "RouteDelta was rerouted to the East corridor in August."],
        gt: g("none", "any", "DriverMax assigned_to RouteDelta", "mixed", "documentation", "none"),
    },
    Question {
        text: "What does ElkLogPolicy limit?",
        kind: "policy",
        class: "W9",
        units: ["ElkLogPolicy limits driving to 10 hours per day.", "The HubCheckpoint verifies the ElkLogPolicy log each morning."],
        gt: g("none", "ho-compliance", "HubCheckpoint verifies ElkLogPolicy", "current", "organization_policy", "none"),
    },
    Question {
        text: "What does the HubCheckpoint check?",
        kind: "policy",
        class: "W9",
        units: ["The HubCheckpoint verifies the ElkLogPolicy log each morning.", "ElkLogPolicy limits driving to 10 hours per day."],
        gt: g("none", "ho-compliance", "HubCheckpoint verifies ElkLogPolicy", "current", "organization_policy", "none"),
    },
    Question {
        text: "What does the morning routine involve?",
        kind: "planning",
        class: "W10",
        units: ["The HubCheckpoint verifies the ElkLogPolicy log each morning.", "DriverMax is assigned to RouteDelta."],
        gt: g("none", "any", "DriverMax assigned_to RouteDelta", "current", "organization_policy", "none"),
    },
    Question {
        text: "What is the fleet fuel contract?",
        kind: "unknown-probe",
        class: "W11",
        units: ["Widget shipments weigh 2 tons each.", "DriverMax holds a Class A license."],
        gt: g("no authoritative answer", "any", "none", "current", "documentation", "unknown"),
    },
    Question {
        text: "When does RouteDelta retire?",
        kind: "unknown-probe",
        class: "W11",
        units: ["RouteDelta has 14 stops.", "The Widget shipment arrives on Fridays."],
        gt: g("no authoritative answer", "any", "none", "current", "documentation", "unknown"),
    },
    Question {
        text: "How did RouteDelta evolve?",
        kind: "longitudinal",
        class: "W12",
        units: ["RouteDelta was rerouted to the East corridor in August.", "RouteDelta serves the North corridor."],
        gt: g("none", "any", "none", "mixed", "documentation", "none"),
    },
    Question {
        text: "What do the records say versus the route sheet?",
        kind: "longitudinal",
        class: "W12",
        units: ["The DispatchNotes conflict with the RouteSheet.", "RouteDelta serves the North corridor."],
        gt: g("none", "any", "DispatchNotes conflicts_with RouteSheet", "current", "documentation", "conflict"),
    },
];
