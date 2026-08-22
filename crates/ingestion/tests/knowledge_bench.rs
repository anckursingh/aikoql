//! Track-B (TESTING-PLAN P1): the knowledge-centric benchmark — questions
//! where a flat chunk retriever cannot win structurally: multi-hop, cross-
//! document, and graph-traversal answers whose content shares no keywords
//! with the question.
//!
//! Treatments, both fully mechanical over the SAME synthetic corpus (the
//! G12 convention, CI-reproducible):
//! - **AikoQL**: the hand-built IRs of 15 documents merged into one graph
//!   (A3), then `compile_context` packs the package under the budget. The
//!   corpus is synthetic because the real mock pipeline extracts relations
//!   only from markdown links; Track-B needs hand-authored graph structure
//!   to pose the questions at all.
//! - **RAG**: `common::rank` lexical retriever (exact token overlap, the
//!   G12 baseline) packs ranked chunks until the budget runs out. Zero-
//!   overlap chunks are dropped at RANK time — the structural miss is the
//!   retriever's, not the budget's (the corpus fits well under the budget,
//!   so every ranked chunk packs deterministically).
//!
//! Judge: each question requires 2 evidence units (a fact statement and/or
//! a relation triple); a unit is delivered when every token of the unit
//! string appears in the payload — token containment, the same definition
//! G12 uses for answer-hit. Both treatments are judged on the payload the
//! agent actually receives: the rendered markdown for AikoQL, the packed
//! chunk text for RAG.
//!
//! Corpus integrity (fairness): every fact statement appears verbatim
//! (token-identical) in a chunk of its document; every entity name appears
//! in a chunk of its document; every relation's endpoints co-occur in a
//! chunk of its document (its extraction basis). AikoQL gets no knowledge
//! RAG could not in principle have retrieved from the same text.
//!
//! Question types:
//! - Q0/Q1 multi-hop: the question names the hub entity; the answer fact
//!   lives in a chunk with zero lexical overlap. AikoQL follows the
//!   relation boost to the neighbor's fact; RAG never ranks the chunk.
//! - Q2 cross-document: hub doc carries the relation, a second doc carries
//!   the answer fact (keyword-invisible). Traversal across the merge.
//! - Q3/Q4 probes (documented, not scored as wins): temporal supersession
//!   and contradiction. BOTH treatments surface both claims — neither
//!   compiler nor retriever suppresses the stale/conflicting claim (no
//!   temporal policy in the compiler; a trust-model/temporal-policy item
//!   remains open).
//! - Q5 control: a plain single-doc keyword question — RAG's home turf.
//!   Both treatments must deliver both units, or the bench is rigged.
//! - Q6 depth-2 probe: A → B → C. The boost is single-round (ponytail:
//!   no transitivity in context.rs), so B ranks but C's fact is gated
//!   out; the B→C RELATION still renders (B was boosted). AikoQL delivers
//!   the pointer but not the content — the documented ceiling that
//!   progressive context expansion (P1) is meant to lift.
//!
//! Determinism: hand-built IRs in a fixed doc order, deterministic merge
//! (BTreeMaps), deterministic compiler tie-breaks, and `common::rank`'s
//! deterministic sort — the bench is bit-reproducible.
//!
//! The gates pin the structural separation with headroom (the PR-G
//! convention): a regression in graph traversal — or a leak that hands RAG
//! the zero-overlap chunks — fails CI; an improvement passes trivially.

mod common;

use aikoql_ingestion::{
    compile_context, merge_knowledge_ir, render_context_markdown, EntityCandidate, Evidence,
    FactCandidate, KnowledgeIr, MockEmbeddingProvider, RelationCandidate,
};

/// Token budget both treatments must respect (len/4 estimate, the G12
/// convention). Deliberately large enough that RAG packs every ranked
/// chunk — its misses must come from ranking, not the budget — and small
/// enough that AikoQL still minimizes (worst question est ≈ 240).
const BUDGET: usize = 300;

struct Doc {
    id: &'static str,
    chunks: &'static [&'static str],
    ir: KnowledgeIr,
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
/// (or zero-overlaps) are exactly what the question rows intend — see the
/// module docs for the per-question analysis.
fn docs() -> Vec<Doc> {
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

struct Question {
    text: &'static str,
    kind: &'static str,
    units: [&'static str; 2],
}

const QUESTIONS: &[Question] = &[
    Question {
        kind: "hop",
        text: "Why does the PaymentService stop charging after repeated failures?",
        units: [
            "PaymentService depends_on RetryPolicy",
            "Retry limit is 3 attempts.",
        ],
    },
    Question {
        kind: "hop",
        text: "What law governs the auditors of the ledger?",
        units: [
            "LedgerService tested_by AuditTeam",
            "Audit activities are mandated by SOX.",
        ],
    },
    Question {
        kind: "cross-doc",
        text: "What warranty does the HomeAutomation carry?",
        units: [
            "WarrantyPolicy depends_on RepairVendor",
            "RepairVendor ships replacements within 48 hours.",
        ],
    },
    Question {
        kind: "temporal-probe",
        text: "What is the current retry limit?",
        units: ["Retry limit is 5 attempts.", "Retry limit is 3 attempts."],
    },
    Question {
        kind: "contradiction-probe",
        text: "How much did active users grow in 2025?",
        units: [
            "ActiveUsers grew 40 percent in 2025.",
            "ActiveUsers grew 20 percent in 2025.",
        ],
    },
    Question {
        kind: "control",
        text: "What writes ledger entries?",
        units: [
            "Ledger entries require exactly one write.",
            "LedgerService tested_by AuditTeam",
        ],
    },
    Question {
        kind: "depth-2-probe",
        text: "What does the RootService ultimately depend on?",
        units: [
            "MiddlePolicy depends_on LeafRule",
            "LeafRule sets ceiling at 99.",
        ],
    },
];

/// A unit is delivered when every token of the unit string occurs in the
/// payload (token containment — the G12 answer-hit definition).
fn unit_hit(delivered: &str, unit: &str) -> bool {
    let d = common::tokens(delivered);
    common::tokens(unit).iter().all(|t| d.contains(t))
}

fn units_hit(delivered: &str, q: &Question) -> (usize, [bool; 2]) {
    let hits = [
        unit_hit(delivered, q.units[0]),
        unit_hit(delivered, q.units[1]),
    ];
    (hits.iter().filter(|h| **h).count(), hits)
}

#[test]
fn knowledge_bench() {
    let provider = MockEmbeddingProvider::new();
    let docs = docs();

    // ── Corpus integrity: AikoQL gets no knowledge RAG could not retrieve
    // from the same text.
    for d in &docs {
        for f in &d.ir.facts {
            let backing = d.chunks.iter().any(|c| {
                common::tokens(&f.statement)
                    .iter()
                    .all(|t| common::tokens(c).contains(t))
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
                    let t = common::tokens(c);
                    t.contains(&r.subject.to_lowercase()) && t.contains(&r.object.to_lowercase())
                }),
                "relation {}-{} has no co-mention basis chunk in {}",
                r.subject,
                r.object,
                d.id
            );
        }
    }

    // Both treatments read the same documents: chunks for RAG, IRs for
    // AikoQL (fixed doc order → deterministic merge).
    let corpus: Vec<common::CorpusChunk<'static>> = docs
        .iter()
        .flat_map(|d| {
            d.chunks
                .iter()
                .enumerate()
                .map(|(i, c)| (d.id, i, c.to_string()))
        })
        .collect();
    let irs: Vec<KnowledgeIr> = docs.iter().map(|d| d.ir.clone()).collect();
    let merged = merge_knowledge_ir(&irs);
    eprintln!(
        "[TRACK-B STRUCTURE] chunks={} merged_entities={} merged_facts={} merged_relations={} budget={BUDGET}",
        corpus.len(),
        merged.entities.len(),
        merged.facts.len(),
        merged.relations.len(),
    );

    // Fact anchors and relation endpoints must resolve against the merged
    // graph, or the compiler gate would silently exclude them.
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

    let mut a_units = 0usize;
    let mut r_units = 0usize;
    let mut a_tokens = 0usize;
    let mut r_tokens = 0usize;

    for (qi, q) in QUESTIONS.iter().enumerate() {
        // ── AikoQL treatment ──────────────────────────────────────────────
        let pkg = compile_context(q.text, &merged, BUDGET);
        assert!(
            pkg.estimated_tokens <= BUDGET,
            "{}: aikoql package exceeded the budget: {} > {BUDGET}",
            q.text,
            pkg.estimated_tokens
        );
        let delivered = render_context_markdown(&pkg);
        let (ah, a_hits) = units_hit(&delivered, q);

        // ── RAG baseline treatment ───────────────────────────────────────
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
        let r_delivered_tokens = packed_text.len() / 4;
        assert!(
            r_delivered_tokens <= BUDGET,
            "{}: rag pack exceeded the budget: {r_delivered_tokens} > {BUDGET}",
            q.text
        );
        let (rh, r_hits) = units_hit(&packed_text, q);

        a_units += ah;
        r_units += rh;
        a_tokens += delivered.len() / 4;
        r_tokens += r_delivered_tokens;

        eprintln!(
            "[TRACK-B Q{qi} {} {:?}] aikoql={ah}/2 {:?} rag={rh}/2 {:?} aikoql_tokens={} rag_tokens={}",
            q.kind,
            q.text,
            a_hits.map(|h| if h { "hit" } else { "miss" }),
            r_hits.map(|h| if h { "hit" } else { "miss" }),
            delivered.len() / 4,
            r_delivered_tokens,
        );
    }

    let n = QUESTIONS.len();
    eprintln!(
        "[TRACK-B SUMMARY] questions={n} aikoql_units={a_units}/{} rag_units={r_units}/{} \
         aikoql_tokens={} rag_tokens={}",
        n * 2,
        n * 2,
        a_tokens / n,
        r_tokens / n,
    );

    // ── Gates: pin the structural separation with headroom ────────────────
    // Expected (hand-verified per question): AikoQL 13/14 — the only miss
    // is the depth-2 probe's leaf fact (single-round boost, documented
    // ceiling); RAG 9/14 — it retrieves every unit whose words appear in
    // any ranked chunk, and none of the zero-overlap answer facts. A
    // regression in traversal, or a leak that hands RAG the zero-overlap
    // chunks, fails CI.
    assert!(
        a_units >= 12,
        "aikoql knowledge coverage regressed: {a_units}/{} (expected 13)",
        n * 2
    );
    assert!(
        r_units <= 10,
        "rag baseline covered knowledge it should not: {r_units}/{} (expected 9)",
        n * 2
    );
    assert!(
        a_units > r_units,
        "structural separation lost: aikoql {a_units} vs rag {r_units}"
    );
}
