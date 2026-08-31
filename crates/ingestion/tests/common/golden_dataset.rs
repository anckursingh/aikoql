//! G9 (HLD §50): THE unified golden dataset — every hand-authored ground
//! truth for the corpus instruments lives in this one file, and every
//! regression instrument runs against it:
//!
//! - `GOLDEN` — one `GoldenQuestion` per question with the §50 fields the
//!   static corpus supports: expected answer, expected KOs (entities),
//!   expected relationships, expected evidence (qrel chunks). Temporal
//!   state / authorization / action expectations are scenario-shaped, not
//!   corpus-shaped: they live in the §51 certification scripts
//!   (`mcp_real_world.rs`, TP-3a/3b) which hold their own goldens inline.
//! - `queries()` / `visual_queries()` — the retrieval instruments' query
//!   projections (was `common::QUERIES` / `common::VISUAL_QUERIES`).
//! - `SEMANTIC_GOLD` — per-fixture complete extraction ground truth (was
//!   `semantic_extraction_quality.rs::GOLD`): complete sets for precision,
//!   unlike the per-question expected-KO subsets above.
//! - `multimodal_expected_entities` — the human annotation lists the
//!   golden-suite gate asserts (was `multimodal_golden.rs::expected_entities`).
//!
//! `golden_dataset_integrity.rs` cross-checks all of it against the real
//! mock pipeline: answers grounded in their qrel chunks, expected KOs and
//! relations extracted, the two entity lists consistent.

// Shared test helper: each test binary uses a subset of this module.
#![allow(dead_code)]

use aikoql_ingestion::KnowledgeIr;
use std::collections::HashMap;
use std::path::Path;

use super::FIXTURE_DIR;

/// One retrieval query with hand-annotated relevant chunks as
/// (fixture, 0-based `chunk.position.chunk_index`) pairs in the RULE corpus;
/// qrel text is resolved from there and matched by containment on every
/// corpus.
pub struct Query {
    pub text: &'static str,
    pub relevant: &'static [(&'static str, usize)],
}

/// §50: one golden question. `textual` enters the text-retrieval
/// instrument, `visual` the visual-retrieval instrument (a question can be
/// both). Expected KOs/relations are the subset of the answer's evidence
/// fixture the question actually draws on — `SEMANTIC_GOLD` holds the
/// complete per-fixture sets for precision scoring.
pub struct GoldenQuestion {
    pub id: &'static str,
    pub question: &'static str,
    /// Key tokens a correct answer must contain (the PR-R judge input).
    pub expected_answer: &'static str,
    pub relevant: &'static [(&'static str, usize)],
    pub expected_entities: &'static [&'static str],
    pub expected_relations: &'static [(&'static str, &'static str)],
    pub textual: bool,
    pub visual: bool,
}

/// The unified golden dataset, in corpus-question order (the first 15 are
/// the §60 retrieval queries, order preserved so the pinned 0.867 baseline
/// stays comparable; the last 2 are visual-only probes).
pub const GOLDEN: &[GoldenQuestion] = &[
    GoldenQuestion {
        id: "q-01",
        question: "What was the revenue for Q3 2025?",
        expected_answer: "$10M",
        relevant: &[("plain-text.pdf", 1)],
        expected_entities: &["Acme Corporation"],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-02",
        question: "Who publishes quarterly reports?",
        expected_answer: "Acme Corporation",
        relevant: &[("plain-text.pdf", 0)],
        expected_entities: &["Acme Corporation"],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-03",
        question: "How old is Alice Smith?",
        expected_answer: "30",
        relevant: &[("tables.pdf", 0)],
        expected_entities: &["Alice Smith"],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-04",
        question: "What is the revenue in North America?",
        expected_answer: "1200",
        relevant: &[("tables.pdf", 1)],
        expected_entities: &["North America"],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-05",
        question: "How many units were sold in Q1 2025?",
        expected_answer: "4000",
        relevant: &[("complex-table.pdf", 0)],
        expected_entities: &["Units Sold"],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-06",
        question: "What is the warranty on Home Automation?",
        expected_answer: "24 months",
        relevant: &[("complex-table.pdf", 1)],
        expected_entities: &["Home Automation"],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-07",
        question: "Which quarter had the highest total revenue?",
        expected_answer: "Q2",
        relevant: &[("charts.pdf", 0)],
        expected_entities: &["Total Revenue"],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    // Paraphrase probe: no lexical token overlap with any chunk — the
    // measured gap a semantic retriever must close.
    GoldenQuestion {
        id: "q-08",
        question: "What was their best-performing three-month period?",
        expected_answer: "Q2",
        relevant: &[("charts.pdf", 0)],
        expected_entities: &["Fiscal Quarter"],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-09",
        question: "How does the client reach the database?",
        expected_answer: "Gateway",
        relevant: &[("architecture-diagram.pdf", 0)],
        expected_entities: &["Client", "Gateway", "Database"],
        expected_relations: &[("client", "gateway"), ("gateway", "database")],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-10",
        question: "Who validates payments?",
        expected_answer: "Billing Team",
        relevant: &[("mixed-report.pdf", 0)],
        expected_entities: &["Payment", "Billing Team"],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    // Paraphrase probe: "in charge of" for Owner, "financial record book"
    // for Ledger — no token overlap.
    GoldenQuestion {
        id: "q-11",
        question: "Who is in charge of the financial record book?",
        expected_answer: "Ledger Team",
        relevant: &[("mixed-report.pdf", 0)],
        expected_entities: &["Ledger", "Ledger Team"],
        expected_relations: &[("payment", "ledger")],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-12",
        question: "What was Globex Industries revenue?",
        expected_answer: "$10M",
        relevant: &[("annual-report.pdf", 1)],
        expected_entities: &["Globex Industries"],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-13",
        question: "What do Gamma Partners expect?",
        expected_answer: "continued growth",
        relevant: &[("annual-report.pdf", 2)],
        expected_entities: &["Gamma Partners"],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-14",
        question: "What is the energy mass equation?",
        expected_answer: "E = mc^2",
        relevant: &[("formulas.pdf", 0)],
        expected_entities: &[],
        expected_relations: &[],
        textual: true,
        visual: false,
    },
    GoldenQuestion {
        id: "q-15",
        question: "What logo is shown in figure 3?",
        expected_answer: "Company logo",
        relevant: &[("images.pdf", 0)],
        expected_entities: &[],
        expected_relations: &[],
        textual: true,
        visual: true,
    },
    // Visual-only probes: the logo paraphrase pair and the PR-N chart
    // record — ranked by query-vs-caption cosine, judged by caption
    // containment (no text generator runs on them).
    GoldenQuestion {
        id: "q-16",
        question: "What does the company logo depict?",
        expected_answer: "Company logo",
        relevant: &[("images.pdf", 0)],
        expected_entities: &[],
        expected_relations: &[],
        textual: false,
        visual: true,
    },
    GoldenQuestion {
        id: "q-17",
        question: "What does the bar chart in figure 1 show?",
        expected_answer: "Total Revenue",
        relevant: &[("charts.pdf", 0)],
        expected_entities: &["Total Revenue"],
        expected_relations: &[],
        textual: false,
        visual: true,
    },
];

/// The text-retrieval instrument's query projection (was
/// `common::QUERIES`): §60's 15 queries, corpus order preserved.
pub fn queries() -> Vec<Query> {
    GOLDEN
        .iter()
        .filter(|g| g.textual)
        .map(|g| Query {
            text: g.question,
            relevant: g.relevant,
        })
        .collect()
}

/// The visual-retrieval instrument's query projection (was
/// `common::VISUAL_QUERIES`).
pub fn visual_queries() -> Vec<Query> {
    GOLDEN
        .iter()
        .filter(|g| g.visual)
        .map(|g| Query {
            text: g.question,
            relevant: g.relevant,
        })
        .collect()
}

/// Golden answer key tokens in textual-question order — the PR-R judge's
/// input (was the index-aligned `GOLDEN_ANSWERS` const; now the answer
/// lives next to its question, so the alignment cannot silently shift).
pub fn golden_answers() -> Vec<&'static str> {
    GOLDEN
        .iter()
        .filter(|g| g.textual)
        .map(|g| g.expected_answer)
        .collect()
}

/// Ground truth per fixture — what the document really contains (was
/// `semantic_extraction_quality.rs::GOLD`). Only categories the document
/// actually expresses are listed; fixtures whose category set is empty are
/// skipped for that category. Complete sets — precision needs the full
/// gold, unlike the per-question expected-KO subsets in `GOLDEN`.
pub struct SemanticGold {
    pub fixture: &'static str,
    pub entities: &'static [&'static str],
    /// (subject, object) pairs the document really relates.
    pub relations: &'static [(&'static str, &'static str)],
    pub facts: &'static [&'static str],
}

pub const SEMANTIC_GOLD: &[SemanticGold] = &[
    SemanticGold {
        fixture: "plain-text.pdf",
        entities: &["Acme Corporation", "Globex Industries"],
        relations: &[],
        facts: &[],
    },
    SemanticGold {
        fixture: "tables.pdf",
        entities: &[
            "Alice Smith",
            "Bob Johnson",
            "North America",
            "South America",
        ],
        relations: &[],
        facts: &[
            "Employee Name: Alice Smith",
            "Age: 30",
            "Employee Name: Bob Johnson",
            "Age: 45",
            "Region: North America",
            "Revenue (USD): 1200",
            "Region: South America",
            "Revenue (USD): 800",
        ],
    },
    SemanticGold {
        fixture: "complex-table.pdf",
        entities: &["Industrial Sensors", "Home Automation"],
        relations: &[],
        facts: &[
            "Quarter: Q1 2025",
            "Units Sold: 4000",
            "Margin (%): 12.5",
            "Quarter: Q2 2025",
            "Units Sold: 5100",
            "Margin (%): 13.0",
            "Product Line: Industrial Sensors",
            "Warranty (months): 36",
            "Product Line: Home Automation",
            "Warranty (months): 24",
        ],
    },
    SemanticGold {
        fixture: "charts.pdf",
        entities: &[],
        relations: &[],
        facts: &[
            "Fiscal Quarter: Q1",
            "Total Revenue: 1200",
            "Fiscal Quarter: Q2",
            "Total Revenue: 1500",
        ],
    },
    SemanticGold {
        fixture: "architecture-diagram.pdf",
        entities: &["Client", "Gateway", "Database", "Cache"],
        relations: &[
            ("client", "gateway"),
            ("gateway", "database"),
            ("gateway", "cache"),
        ],
        facts: &[],
    },
    SemanticGold {
        fixture: "mixed-report.pdf",
        entities: &[
            "Acme Corporation",
            "Billing Team",
            "Ledger Team",
            "Payment",
            "Ledger",
        ],
        relations: &[("payment", "ledger")],
        facts: &[
            "Step: Validate",
            "Owner: Billing Team",
            "Step: Commit",
            "Owner: Ledger Team",
        ],
    },
    SemanticGold {
        fixture: "annual-report.pdf",
        entities: &["Acme Corporation", "Globex Industries", "Gamma Partners"],
        relations: &[],
        facts: &["Metric: Growth", "Value: 8 percent"],
    },
];

/// HLD §53 human-annotation recall lists for the golden-suite gate (was
/// `multimodal_golden.rs::expected_entities`): the entities a human reading
/// each fixture would annotate.
pub fn multimodal_expected_entities(name: &str) -> &'static [&'static str] {
    match name {
        "plain-text.pdf" => &["Acme Corporation", "Globex Industries"],
        "scanned.pdf" => &[],
        "tables.pdf" => &[
            "Employee Name",
            "Alice Smith",
            "Bob Johnson",
            "North America",
            "South America",
        ],
        "complex-table.pdf" => &[
            "Units Sold",
            "Product Line",
            "Industrial Sensors",
            "Home Automation",
        ],
        "charts.pdf" => &["Fiscal Quarter", "Total Revenue"],
        "architecture-diagram.pdf" => &["Client", "Gateway", "Database"],
        "mixed-report.pdf" => &[
            "Billing Pipeline",
            "Acme Corporation",
            "Billing Team",
            "Ledger Team",
            "Payment",
            "Ledger",
        ],
        "formulas.pdf" => &[],
        "images.pdf" => &[],
        "annual-report.pdf" => &[
            "Annual Report",
            "Acme Corporation",
            "Globex Industries",
            "Gamma Partners",
        ],
        _ => &[],
    }
}

/// Case-insensitive, whitespace-collapsed canonical form.
pub fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compile each golden fixture once through the real mock pipeline (rule
/// boundary, mock components — the baseline stack) and return the IRs
/// keyed by fixture name.
pub fn compile_fixture_irs() -> HashMap<&'static str, KnowledgeIr> {
    let provider = aikoql_ingestion::MockEmbeddingProvider::new();
    let mut irs = HashMap::new();
    for name in super::FIXTURES {
        let path = Path::new(FIXTURE_DIR).join(name);
        let dm =
            aikoql_ingestion::extract_document(&path.to_string_lossy(), "application/pdf", None)
                .unwrap_or_else(|e| panic!("{name}: extraction failed: {e}"));
        let result = aikoql_ingestion::compile_document_with_detector(
            &dm,
            &aikoql_ingestion::MockSemanticAnalyzer::new(),
            &aikoql_ingestion::MockEntityResolver::new(),
            &aikoql_ingestion::MockKnowledgeReconciler::new(),
            &aikoql_ingestion::HeadingProjector::new(),
            &provider,
            &aikoql_ingestion::RuleBoundaryDetector,
            &[],
            None,
        );
        irs.insert(*name, result.ir);
    }
    irs
}
