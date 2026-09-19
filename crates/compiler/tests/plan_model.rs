//! P5-M3 (ND-03) — logical/physical plan model. qm001–qm004.
//!
//! The roadmap's ND-03 RED list verbatim: qm001 — LogicalPlan compiles from
//! every existing query form (golden corpus extended); qm002 — the physical
//! plan selects the vector-index path for SIMILAR TO and the scan path for
//! plain MATCH (strategy visible in EXPLAIN); qm003 — plan serialization
//! round-trips byte-stable (golden byte-pin, plain JSON so the Python SDK
//! can verify the same bytes — rule 4); qm004 — the logical layer never
//! touches storage internals (module-boundary pin: no aikoql-v2 / engine
//! imports in the plan layer).

use aikoql_compiler::parser;
use aikoql_kernel::ir::*;

fn phys(source: &str) -> PhysicalPlan {
    parser::compile_physical(source).unwrap_or_else(|e| panic!("compile failed: {}", e))
}

// ---------------------------------------------------------------------------
// qm001 — LogicalPlan compiles from every existing query form
// ---------------------------------------------------------------------------

#[test]
fn qm001_logical_plan_compiles_from_every_query_form() {
    let corpus: &[&str] = &[
        "MATCH Person RETURN *",
        "MATCH Person WHERE company == \"Visa\" AND city == \"Amsterdam\" RETURN *",
        "MATCH Doc SIMILAR TO \"concept\" RETURN *",
        "MATCH Doc SIMILAR TO \"concept\" SCORE BM25 USING EMBEDDING RETURN *",
        "MATCH Person TRAVERSE knows DEPTH 2 RETURN *",
        "MATCH Fact AS_OF 1000 RETURN *",
        "MATCH Fact BETWEEN 100 AND 200 RETURN *",
        "MATCH Fact HISTORICAL RETURN *",
        "MATCH Fact EPISTEMIC verified, observed RETURN *",
        "MATCH Fact SOURCE \"a.md\" RETURN *",
        "MATCH Fact LIMIT 10 OFFSET 3 RETURN *",
        "MATCH Fact ORDER BY ts DESC RETURN *",
        "MATCH Fact GROUP BY kind, COUNT(*) RETURN *",
        "MATCH Employee JOIN Department ON id == dept RETURN *",
        "INGEST \"x.pdf\" EXTRACT tables COMMIT",
    ];
    for src in corpus {
        let plan = parser::compile_logical(src)
            .unwrap_or_else(|e| panic!("logical compile failed for {:?}: {}", src, e));
        assert_eq!(plan.version, PLAN_VERSION, "version stamp on {:?}", src);
        plan.validate()
            .unwrap_or_else(|e| panic!("validate failed for {:?}: {}", src, e));
        match &plan.operators[0] {
            IrOp::Scan { .. } | IrOp::Traverse { .. } | IrOp::Ingest { .. } => {}
            other => panic!(
                "first op of {:?} must start the pipeline, got {:?}",
                src, other
            ),
        }
        // The physical path must compile the same corpus (the executable form).
        phys(src);
    }
}

#[test]
fn qm001_logical_plan_keeps_the_query_description() {
    let plan = parser::compile_logical("MATCH Person WHERE company == \"Visa\" RETURN *").unwrap();
    assert!(plan.description.is_some(), "compile stamps a description");
}

// ---------------------------------------------------------------------------
// qm002 — physical strategy selection, visible in the plan summary (EXPLAIN)
// ---------------------------------------------------------------------------

#[test]
fn qm002_plain_match_scans() {
    let plan = phys("MATCH Person RETURN *");
    match &plan.operators[0] {
        PhysicalOp {
            op: IrOp::Scan { .. },
            strategy: Strategy::FullScan,
        } => {}
        other => panic!("plain MATCH must be a FullScan, got {:?}", other),
    }
}

#[test]
fn qm002_similar_to_using_embedding_uses_the_vector_index() {
    let plan = phys("MATCH Doc SIMILAR TO \"q\" USING EMBEDDING RETURN *");
    let ann = plan
        .operators
        .iter()
        .find(|p| matches!(p.op, IrOp::AnnSearch { .. }))
        .expect("AnnSearch in plan");
    assert_eq!(ann.strategy, Strategy::VectorIndex);
}

#[test]
fn qm002_bm25_search_uses_the_text_index() {
    let plan = phys("MATCH Doc SIMILAR TO \"q\" SCORE BM25 RETURN *");
    let search = plan
        .operators
        .iter()
        .find(|p| matches!(p.op, IrOp::TextSearch { .. }))
        .expect("TextSearch in plan");
    assert_eq!(search.strategy, Strategy::TextIndex);
}

#[test]
fn qm002_row_operators_are_inline() {
    let plan = phys("MATCH X WHERE a == 1 LIMIT 2 RETURN koid");
    for p in &plan.operators {
        match p.op {
            IrOp::Scan { .. } => {}
            _ => assert_eq!(p.strategy, Strategy::Inline, "{:?} must be Inline", p.op),
        }
    }
}

#[test]
fn qm002_strategy_is_visible_in_the_plan_summary() {
    let summary = phys("MATCH Doc SIMILAR TO \"q\" USING EMBEDDING RETURN *").summary();
    assert!(
        summary.iter().any(|line| line.contains("VectorIndex")),
        "EXPLAIN shows the vector-index strategy: {:?}",
        summary
    );
    let summary = phys("MATCH Person RETURN *").summary();
    assert!(
        summary.iter().any(|line| line.contains("FullScan")),
        "EXPLAIN shows the scan strategy: {:?}",
        summary
    );
}

// ---------------------------------------------------------------------------
// qm003 — plan serialization round-trips byte-stable
// ---------------------------------------------------------------------------

#[test]
fn qm003_serialization_round_trips_byte_stable() {
    let src =
        "MATCH Fact WHERE temp == 35 AND kind == \"hot\" ORDER BY ts DESC LIMIT 3 RETURN koid";
    let s1 = serde_json::to_string(&phys(src)).unwrap();
    assert!(
        s1.contains("\"version\":1"),
        "wire form stamps the version: {}",
        s1
    );
    let back: PhysicalPlan = serde_json::from_str(&s1).unwrap();
    let s2 = serde_json::to_string(&back).unwrap();
    assert_eq!(
        s1, s2,
        "serialize → deserialize → serialize is byte-identical"
    );
}

#[test]
fn qm003_plan_golden_byte_pin() {
    // Committed byte pin (rule 4): plain JSON so the Python SDK can verify
    // the same bytes. Regenerating the fixture is a conscious change.
    let src =
        "MATCH Fact WHERE temp == 35 AND kind == \"hot\" ORDER BY ts DESC LIMIT 3 RETURN koid";
    let golden = include_str!("fixtures/plan_golden.json");
    assert_eq!(serde_json::to_string(&phys(src)).unwrap(), golden);
}

#[test]
fn qm003_logical_plan_serializes_too() {
    let src = "MATCH Fact ORDER BY ts LIMIT 2 RETURN *";
    let s1 = serde_json::to_string(&parser::compile_logical(src).unwrap()).unwrap();
    let back: LogicalPlan = serde_json::from_str(&s1).unwrap();
    assert_eq!(serde_json::to_string(&back).unwrap(), s1);
}

// ---------------------------------------------------------------------------
// qm004 — the logical layer never touches storage internals
// ---------------------------------------------------------------------------

#[test]
fn qm004_logical_layer_is_storage_independent() {
    let forbidden = [
        "aikoql_v2",
        "aikoql-v2",
        "aikoql_storage_v2",
        "aikoql-vector",
        "aikoql-graph",
        "aikoql-scheduler",
        "aikoql-reasoning",
    ];
    // The plan layer = the compiler crate plus the plan-type module (kernel ir.rs).
    let roots = [
        concat!(env!("CARGO_MANIFEST_DIR"), "/src"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/../kernel/src/ir.rs"),
    ];
    for root in &roots {
        for path in rust_sources(root) {
            let text = std::fs::read_to_string(&path).unwrap();
            for needle in &forbidden {
                assert!(
                    !text.contains(needle),
                    "{} imports a storage/engine crate ({}) — the logical layer is storage-independent",
                    path.display(),
                    needle
                );
            }
        }
    }
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    for needle in &forbidden {
        assert!(
            !manifest.contains(needle),
            "compiler Cargo.toml depends on a storage/engine crate ({})",
            needle
        );
    }
}

fn rust_sources(root: &str) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let path = std::path::Path::new(root);
    if path.is_file() {
        out.push(path.to_path_buf());
        return out;
    }
    for entry in std::fs::read_dir(path).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            out.extend(rust_sources(&p.to_string_lossy()));
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
    out
}
