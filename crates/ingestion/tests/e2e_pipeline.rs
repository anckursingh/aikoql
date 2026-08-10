//! End-to-end test: full MRFC-0070 A0-A5 pipeline.
//!
//! Markdown compile → Code compile → Merge → Staleness detect → Context compile.
//! Validates that all phases work together end-to-end.

use mnemosyne_ingestion::{
    compile_context, compile_markdown_string, compile_rust_source, detect_staleness,
    merge_knowledge_ir, render_context_markdown,
};

#[test]
fn e2e_full_pipeline_markdown_plus_code() {
    // Simulate a real project: CLAUDE.md + Rust source
    let markdown = r#"# Mnemosyne

Mnemosyne is an Agent-first Knowledge Database that turns documents
and code into queryable, type-checked Knowledge Objects.

## Architecture

The system uses MVCC for transaction isolation across all writes.
All writes go through the TransactionEngine which validates constraints.

## Rules

- must use MVCC for all writes
- must validate constraints at commit time
- should document all public APIs
"#;

    let rust_code = r#"//! Mnemosyne kernel — Agent-first Knowledge Database.

/// The transaction engine handles all write operations with MVCC isolation.
pub struct TransactionEngine {
    pub pending: Vec<Transaction>,
}

/// Validates constraints before commit.
pub struct ConstraintEngine {
    pub rules: Vec<ConstraintRule>,
}

impl ConstraintEngine {
    /// Validate all active constraints against the current state.
    pub fn validate(&self, state: &State) -> Result<(), Vec<String>> {
        // validation logic
        Ok(())
    }
}

use std::collections::HashMap;
use crate::transaction::Transaction;
"#;

    // 1. Compile Markdown
    let md_ir = compile_markdown_string(markdown, Some("CLAUDE.md".into()))
        .expect("markdown compile should succeed");

    assert!(
        !md_ir.entities.is_empty(),
        "markdown should produce entities"
    );
    assert!(!md_ir.facts.is_empty(), "markdown should produce facts");
    assert!(
        md_ir.entities.iter().any(|e| e.name == "Architecture"),
        "should find Architecture entity"
    );

    // 2. Compile Rust code
    let code_ir = compile_rust_source(rust_code, Some("lib.rs"));

    assert!(!code_ir.entities.is_empty(), "code should produce entities");
    assert!(
        code_ir
            .entities
            .iter()
            .any(|e| e.name == "TransactionEngine"),
        "should find TransactionEngine struct"
    );
    assert!(
        code_ir
            .entities
            .iter()
            .any(|e| e.name == "ConstraintEngine"),
        "should find ConstraintEngine struct"
    );
    assert!(
        code_ir
            .relations
            .iter()
            .any(|r| r.predicate == "depends_on"),
        "should find depends_on from use statement"
    );

    // 3. Merge
    let code_entities = code_ir.entities.len();
    let code_facts = code_ir.facts.len();
    let code_relations = code_ir.relations.len();
    let merged = merge_knowledge_ir(&[md_ir, code_ir]);

    assert!(
        merged.entities.len() >= 4,
        "merged should have entities from both sources"
    );
    assert!(
        merged.facts.len() >= 4,
        "merged should have facts from both sources"
    );

    // Verify cross-source entity coexistence
    assert!(
        merged
            .entities
            .iter()
            .any(|e| e.name == "Architecture" && e.type_hint.as_deref() == Some("Architecture")),
        "Markdown Architecture entity should survive merge"
    );
    assert!(
        merged
            .entities
            .iter()
            .any(|e| e.name == "TransactionEngine"),
        "Code TransactionEngine entity should survive merge"
    );

    // 4. Detect staleness
    // Label facts by source (markdown facts first, then code facts)
    let md_fact_count = merged
        .facts
        .iter()
        .filter(|f| {
            f.statement.contains("MVCC")
                || f.statement.contains("must")
                || f.statement.contains("should")
        })
        .count();
    let sources: Vec<&str> = vec!["markdown"; md_fact_count.min(merged.facts.len())];
    let mut all_sources = sources;
    while all_sources.len() < merged.facts.len() {
        all_sources.push("code");
    }

    let _stale_warnings = detect_staleness(&merged.facts, &all_sources);

    // 5. Compile context for a task
    let pkg = compile_context(
        "add constraint validation to transaction",
        &merged,
        0, // unlimited
    );

    assert!(
        !pkg.entities.is_empty(),
        "context should have relevant entities"
    );
    assert!(!pkg.facts.is_empty(), "context should have relevant facts");

    // ConstraintEngine and TransactionEngine should be top-ranked for this task
    let names: Vec<&str> = pkg.entities.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"ConstraintEngine") || names.contains(&"TransactionEngine"),
        "at least one relevant entity should rank high"
    );

    // 6. Render context as Markdown
    let md = render_context_markdown(&pkg);
    assert!(!md.is_empty(), "rendered markdown should not be empty");
    assert!(md.contains("Relevant"), "should have section headers");

    eprintln!("=== E2E Pipeline Test Passed ===");
    let md_ir2 = compile_markdown_string(markdown, None).unwrap();
    eprintln!(
        "Markdown IR:  {} entities, {} facts",
        md_ir2.entities.len(),
        md_ir2.facts.len()
    );
    eprintln!(
        "Code IR:      {} entities, {} facts, {} relations",
        code_entities, code_facts, code_relations
    );
    eprintln!(
        "Merged:       {} entities, {} facts, {} relations",
        merged.entities.len(),
        merged.facts.len(),
        merged.relations.len()
    );
    eprintln!(
        "Context:      {} entities, {} facts, {} relations, {} tokens",
        pkg.entities.len(),
        pkg.facts.len(),
        pkg.relations.len(),
        pkg.estimated_tokens
    );
}
