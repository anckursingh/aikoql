//! PR-Q (HLD §53 "Semantic extraction" + §60): the last two §60 decision
//! metrics without an instrument — fact extraction and relation extraction
//! quality — plus the rest of the §53 semantic-extraction stage (entity
//! precision/recall). §53:
//!
//! ```text
//! entity precision / recall
//! relation precision / recall
//! fact accuracy
//! event accuracy
//! ```
//!
//! Every golden fixture compiles through the real mock pipeline (rule
//! boundary + mock components — the same baseline stack as the golden
//! suite and the retrieval instrument) and its `KnowledgeIr` is judged
//! against hand-authored ground truth. Matching is set-based on normalized
//! exact equality (entities by name; relations by (subject, object) —
//! the mock's only predicate is `related_to`, so the predicate carries no
//! signal; facts by statement). A fixture is judged only in categories
//! where its document really contains something to extract (a gold-empty
//! category is skipped, not scored — same convention as scanned.pdf's
//! exclusion from retrieval). Extracted duplicates collapse into the set.
//!
//! Precision = |extracted ∩ gold| / |extracted| (1.0 when nothing is
//! extracted — recall carries the failure signal); recall = |extracted ∩
//! gold| / |gold|. "Fact accuracy" is fact precision per §53's name; both
//! numbers print. Event accuracy prints N/A: no fixture produces an
//! EventCandidate under the mock rule pipeline (no event rules) — the same
//! honest-N/A convention as the earlier §60 visual cells, with the event
//! count still printed so a future event extractor makes the cell real.
//!
//! Floors assert the measured baseline, like the PR-G retrieval floors:
//! a real regression (entity loss, fact extraction breakage) fails CI, a
//! variant improvement passes trivially.

use aikoql_ingestion::KnowledgeIr;
use std::collections::HashSet;
use std::path::Path;

use common::FIXTURE_DIR;

mod common;

/// Ground truth per fixture — what the document really contains. Only
/// categories the document actually expresses are listed; fixtures whose
/// category set is empty are skipped for that category.
struct Gold {
    fixture: &'static str,
    entities: &'static [&'static str],
    /// (subject, object) pairs the document really relates.
    relations: &'static [(&'static str, &'static str)],
    facts: &'static [&'static str],
}

const GOLD: &[Gold] = &[
    Gold {
        fixture: "plain-text.pdf",
        entities: &["Acme Corporation", "Globex Industries"],
        relations: &[],
        facts: &[],
    },
    Gold {
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
    Gold {
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
    Gold {
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
    Gold {
        fixture: "architecture-diagram.pdf",
        entities: &["Client", "Gateway", "Database", "Cache"],
        relations: &[
            ("client", "gateway"),
            ("gateway", "database"),
            ("gateway", "cache"),
        ],
        facts: &[],
    },
    Gold {
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
    Gold {
        fixture: "annual-report.pdf",
        entities: &["Acme Corporation", "Globex Industries", "Gamma Partners"],
        relations: &[],
        facts: &["Metric: Growth", "Value: 8 percent"],
    },
];

/// Case-insensitive, whitespace-collapsed canonical form.
fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compile each golden fixture once through the real mock pipeline (rule
/// boundary, mock components — the baseline stack) and keep its IR.
fn compile_ir() -> Vec<(&'static str, KnowledgeIr)> {
    let provider = aikoql_ingestion::MockEmbeddingProvider::new();
    let mut irs = Vec::new();
    for name in common::FIXTURES {
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
        irs.push((*name, result.ir));
    }
    irs
}

/// Set-based precision/recall over normalized strings. Extracted duplicates
/// collapse into the set; an empty extracted set scores precision 1.0 (no
/// false positives — recall carries the failure).
fn pr(extracted: impl Iterator<Item = String>, gold: &[&str]) -> (f32, f32) {
    let ex: HashSet<String> = extracted.map(|s| normalize(&s)).collect();
    let go: HashSet<String> = gold.iter().map(|g| normalize(g)).collect();
    let hit = ex.intersection(&go).count();
    let p = if ex.is_empty() {
        1.0
    } else {
        hit as f32 / ex.len() as f32
    };
    let r = hit as f32 / go.len() as f32;
    (p, r)
}

fn pair<I>(extracted: I, gold: &[(&str, &str)]) -> (f32, f32)
where
    I: Iterator<Item = (String, String)>,
{
    let ex: HashSet<(String, String)> = extracted
        .map(|(s, o)| (normalize(&s), normalize(&o)))
        .collect();
    let go: HashSet<(String, String)> = gold
        .iter()
        .map(|(s, o)| (normalize(s), normalize(o)))
        .collect();
    let hit = ex.intersection(&go).count();
    let p = if ex.is_empty() {
        1.0
    } else {
        hit as f32 / ex.len() as f32
    };
    let r = hit as f32 / go.len() as f32;
    (p, r)
}

/// §53/§60 semantic-extraction quality over the golden suite, macro-averaged
/// over the fixtures judged in each category. Prints one line per judged
/// (fixture, category) cell and one summary; asserts the baseline floors.
#[test]
fn semantic_extraction_quality() {
    let irs = compile_ir();
    let by_fixture: std::collections::HashMap<&str, &KnowledgeIr> =
        irs.iter().map(|(f, ir)| (*f, ir)).collect();

    let mut totals = [0.0f32; 6]; // ent P/R, rel P/R, fact P/R
    let mut judged = [0usize; 3]; // fixtures judged per category
    let mut event_count = 0usize;

    for g in GOLD {
        let ir = by_fixture[g.fixture];
        event_count += ir.events.len();

        if !g.entities.is_empty() {
            let (p, r) = pr(ir.entities.iter().map(|e| e.name.clone()), g.entities);
            eprintln!(
                "[SEMANTIC-E {}] extracted={} gold={} precision={p:.3} recall={r:.3}",
                g.fixture,
                ir.entities.len(),
                g.entities.len()
            );
            totals[0] += p;
            totals[1] += r;
            judged[0] += 1;
        }
        if !g.relations.is_empty() {
            let (p, r) = pair(
                ir.relations
                    .iter()
                    .map(|x| (x.subject.clone(), x.object.clone())),
                g.relations,
            );
            eprintln!(
                "[SEMANTIC-R {}] extracted={} gold={} precision={p:.3} recall={r:.3}",
                g.fixture,
                ir.relations.len(),
                g.relations.len()
            );
            totals[2] += p;
            totals[3] += r;
            judged[1] += 1;
        }
        if !g.facts.is_empty() {
            let (p, r) = pr(ir.facts.iter().map(|f| f.statement.clone()), g.facts);
            eprintln!(
                "[SEMANTIC-F {}] extracted={} gold={} accuracy={p:.3} recall={r:.3}",
                g.fixture,
                ir.facts.len(),
                g.facts.len()
            );
            totals[4] += p;
            totals[5] += r;
            judged[2] += 1;
        }
    }

    // Macro-average over judged fixtures per category; an unjudged category
    // scores 1.0 (nothing to extract, nothing mis-extracted).
    let avg = |t: usize, j: usize| {
        if judged[j] == 0 {
            1.0
        } else {
            totals[t] / judged[j] as f32
        }
    };
    let (ep, er) = (avg(0, 0), avg(1, 0));
    let (rp, rr) = (avg(2, 1), avg(3, 1));
    let (fp, fr) = (avg(4, 2), avg(5, 2));
    eprintln!(
        "[SEMANTIC-SUMMARY] entity P/R={ep:.3}/{er:.3} relation P/R={rp:.3}/{rr:.3} \
         fact accuracy/recall={fp:.3}/{fr:.3} (judged fixtures: {}/{}/{})",
        judged[0], judged[1], judged[2]
    );
    eprintln!(
        "[SEMANTIC-EVENT] N/A — {event_count} EventCandidate(s) across the corpus \
         (mock rule pipeline has no event rules; a future event extractor makes this cell real)"
    );

    // Baseline floors: regressions fail, improvements pass trivially.
    assert!(ep >= 0.5, "entity precision regressed: {ep}");
    assert!(er >= 0.5, "entity recall regressed: {er}");
    assert!(rp >= 0.6, "relation precision regressed: {rp}");
    assert!(rr >= 0.6, "relation recall regressed: {rr}");
    assert!(fp >= 0.7, "fact accuracy regressed: {fp}");
    assert!(fr >= 0.7, "fact recall regressed: {fr}");
}
