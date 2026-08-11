//! MRFC-0070 Phase A10: Benchmark suite for the Knowledge Pipeline.
//!
//! Measures: extraction throughput, context compilation quality,
//! token reduction vs raw documents, and reconciliation accuracy.
//!
//! Run with: cargo test -p aikoql-ingestion -- benchmarks --nocapture

#[cfg(test)]
mod benchmarks {
    use aikoql_ingestion::{
        apply_proposal, auto_proposals_from_stale, compile_context, compile_markdown_string,
        compile_rust_source, connector_metadata_to_ir, discover_connector_schema, filter_secrets,
        merge_knowledge_ir, process_workflow, reconcile, render_context_markdown,
        validate_proposal,
    };
    use std::time::Instant;

    const MARKDOWN_DOC: &str = r#"# Aikoql Architecture

aikoql is an Agent-first Knowledge Database that turns documents
and code into queryable, type-checked Knowledge Objects.

## Core Components

### TransactionEngine

The TransactionEngine handles all write operations with MVCC isolation.
It coordinates with the ConstraintEngine to validate rules before commit.

### ConstraintEngine

The ConstraintEngine validates all active constraints against the current
state. Constraints are defined as aikoql rules that can reference multiple
objects and their relationships.

### AuthService

The AuthService handles authentication and authorization. It supports
OAuth2, JWT, and API key authentication methods. Role-based access control
is enforced at the API gateway level.

## Rules

- must use MVCC for all write operations
- must validate constraints at commit time
- must not allow writes that violate active constraints
- should document all public API functions
- should use structured error types for all fallible operations

## Architecture Decisions

### ADR-001: MVCC over locking

We chose MVCC (Multi-Version Concurrency Control) over traditional
row-level locking because it provides better read performance and
allows concurrent writes without deadlock risk.

### ADR-002: Constraint-first design

Constraints are validated before any write is committed. This ensures
data integrity at the database level rather than relying on application
logic alone.
"#;

    const RUST_CODE: &str = r#"//! aikoql kernel — Agent-first Knowledge Database.

/// The transaction engine handles all write operations with MVCC isolation.
pub struct TransactionEngine {
    pub pending: Vec<Transaction>,
    pub committed: Vec<Transaction>,
}

impl TransactionEngine {
    pub fn begin(&mut self, tx: Transaction) -> Result<(), TxError> {
        self.pending.push(tx);
        Ok(())
    }

    pub fn commit(&mut self) -> Result<(), Vec<ConstraintError>> {
        for tx in &self.pending {
            // validate constraints before commit
        }
        Ok(())
    }
}

/// Validates constraints before commit.
pub struct ConstraintEngine {
    pub rules: Vec<ConstraintRule>,
}

impl ConstraintEngine {
    pub fn validate(&self, state: &State) -> Result<(), Vec<String>> {
        for rule in &self.rules {
            rule.check(state)?;
        }
        Ok(())
    }
}

/// Authentication and authorization service.
pub struct AuthService {
    pub providers: Vec<AuthProvider>,
    pub roles: HashMap<String, Vec<String>>,
}

impl AuthService {
    pub fn authenticate(&self, token: &str) -> Result<Identity, AuthError> {
        // OAuth2 / JWT validation
        Ok(Identity::default())
    }
}

use std::collections::HashMap;
use crate::transaction::Transaction;
use crate::error::{TxError, ConstraintError, AuthError};
use crate::state::State;
use crate::constraint::ConstraintRule;
use crate::auth::{AuthProvider, Identity};
"#;

    /// Measure extraction throughput: markdown entities/sec + code entities/sec.
    #[test]
    #[ignore = "benchmark: use cargo test --ignored or nightly workflow"]
    fn bench_extraction_throughput() {
        let iterations = 100;
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = compile_markdown_string(MARKDOWN_DOC, Some("CLAUDE.md".into())).unwrap();
        }
        let md_elapsed = start.elapsed();
        let md_per_sec = iterations as f64 / md_elapsed.as_secs_f64();

        let start = Instant::now();
        for _ in 0..iterations {
            let _ = compile_rust_source(RUST_CODE, Some("lib.rs"));
        }
        let code_elapsed = start.elapsed();
        let code_per_sec = iterations as f64 / code_elapsed.as_secs_f64();

        eprintln!("=== BENCH: Extraction Throughput ===");
        eprintln!(
            "  Markdown: {:.1} docs/sec ({:.2?} for {} iterations)",
            md_per_sec, md_elapsed, iterations
        );
        eprintln!(
            "  Code:     {:.1} files/sec ({:.2?} for {} iterations)",
            code_per_sec, code_elapsed, iterations
        );
        assert!(
            md_per_sec > 10.0,
            "markdown extraction should exceed 10 docs/sec"
        );
        assert!(
            code_per_sec > 10.0,
            "code extraction should exceed 10 files/sec"
        );
    }

    /// Measure token reduction: raw doc tokens vs compiled context tokens.
    #[test]
    #[ignore = "benchmark: use cargo test --ignored or nightly workflow"]
    fn bench_token_reduction() {
        let md_ir = compile_markdown_string(MARKDOWN_DOC, Some("CLAUDE.md".into())).unwrap();
        let code_ir = compile_rust_source(RUST_CODE, Some("lib.rs"));
        let merged = merge_knowledge_ir(&[md_ir, code_ir]);

        let raw_tokens = (MARKDOWN_DOC.len() + RUST_CODE.len() + 3) / 4;

        let pkg = compile_context("add a constraint validation rule", &merged, 0);
        let ctx_tokens = pkg.estimated_tokens;

        let pkg2 = compile_context("implement OAuth2 authentication", &merged, 0);
        let ctx_tokens2 = pkg2.estimated_tokens;

        let reduction_pct = (1.0 - ctx_tokens as f64 / raw_tokens as f64) * 100.0;
        let reduction_pct2 = (1.0 - ctx_tokens2 as f64 / raw_tokens as f64) * 100.0;

        eprintln!("=== BENCH: Token Reduction ===");
        eprintln!(
            "  Raw documents: {} tokens ({} chars)",
            raw_tokens,
            MARKDOWN_DOC.len() + RUST_CODE.len()
        );
        eprintln!(
            "  Context (constraint task): {} tokens → {:.1}% reduction",
            ctx_tokens, reduction_pct
        );
        eprintln!(
            "  Context (auth task):      {} tokens → {:.1}% reduction",
            ctx_tokens2, reduction_pct2
        );
        eprintln!("  Target: ≥40% reduction");

        assert!(
            reduction_pct > 30.0,
            "should reduce tokens by >30% for constraint task"
        );
        assert!(
            reduction_pct2 > 30.0,
            "should reduce tokens by >30% for auth task"
        );
    }

    /// Measure context quality: relevant entities rank above irrelevant.
    #[test]
    #[ignore = "benchmark: use cargo test --ignored or nightly workflow"]
    fn bench_context_precision() {
        let md_ir = compile_markdown_string(MARKDOWN_DOC, Some("CLAUDE.md".into())).unwrap();
        let code_ir = compile_rust_source(RUST_CODE, Some("lib.rs"));
        let merged = merge_knowledge_ir(&[md_ir, code_ir]);

        // Task about constraint validation → ConstraintEngine should rank high
        let pkg = compile_context("add a constraint validation rule", &merged, 0);
        let top_entities: Vec<&str> = pkg
            .entities
            .iter()
            .take(3)
            .map(|e| e.name.as_str())
            .collect();

        eprintln!("=== BENCH: Context Precision ===");
        eprintln!("  Task: 'add a constraint validation rule'");
        eprintln!("  Top-3 entities: {:?}", top_entities);

        let has_constraint = top_entities.contains(&"ConstraintEngine");
        let has_transaction = top_entities.contains(&"TransactionEngine");
        assert!(
            has_constraint || has_transaction,
            "at least one core entity should rank top-3"
        );
    }

    /// Measure reconciliation accuracy: known change → correct affected entities.
    #[test]
    #[ignore = "benchmark: use cargo test --ignored or nightly workflow"]
    fn bench_reconciliation_accuracy() {
        let md_ir = compile_markdown_string(MARKDOWN_DOC, Some("CLAUDE.md".into())).unwrap();
        let code_ir = compile_rust_source(RUST_CODE, Some("lib.rs"));
        let merged = merge_knowledge_ir(&[md_ir, code_ir]);

        // Simulate changing the constraint.rs file
        let report = reconcile(&["crates/kernel/src/constraint.rs".to_string()], &merged);

        eprintln!("=== BENCH: Reconciliation Accuracy ===");
        eprintln!("  Changed: crates/kernel/src/constraint.rs");
        eprintln!(
            "  Affected entities: {:?}",
            report
                .affected_entities
                .iter()
                .map(|a| format!("{} ({:?})", a.entity_name, a.severity))
                .collect::<Vec<_>>()
        );
        eprintln!("  Stale facts: {}", report.potentially_stale_facts.len());
        eprintln!("  Summary: {}", report.summary);

        assert!(!report.summary.is_empty(), "should produce a summary");
    }

    /// Measure connector bridge: tables/sec conversion throughput.
    #[test]
    #[ignore = "benchmark: use cargo test --ignored or nightly workflow"]
    fn bench_connector_bridge_throughput() {
        let iterations = 200;
        let meta = discover_connector_schema(
            "postgres",
            "benchdb",
            &[
                (
                    "users",
                    &[
                        ("id", "int", true, false, true),
                        ("email", "varchar", false, false, true),
                        ("name", "varchar", false, true, false),
                    ],
                ),
                (
                    "orders",
                    &[
                        ("id", "int", true, false, true),
                        ("user_id", "int", false, false, false),
                        ("total", "numeric", false, false, false),
                    ],
                ),
                (
                    "products",
                    &[
                        ("id", "int", true, false, true),
                        ("name", "varchar", false, false, true),
                        ("price", "numeric", false, false, false),
                    ],
                ),
            ],
            &[(
                "orders",
                &["user_id"],
                "users",
                &["id"],
                Some("fk_orders_users"),
            )],
        );

        let start = Instant::now();
        for _ in 0..iterations {
            let _ir = connector_metadata_to_ir(&meta);
        }
        let elapsed = start.elapsed();
        let per_sec = iterations as f64 / elapsed.as_secs_f64();

        eprintln!("=== BENCH: Connector Bridge Throughput ===");
        eprintln!(
            "  {:.1} schema-to-IR conversions/sec ({:.2?} for {} iterations)",
            per_sec, elapsed, iterations
        );
        assert!(
            per_sec > 50.0,
            "connector bridge should exceed 50 conversions/sec"
        );
    }

    /// Measure markdown rendering: context → markdown throughput.
    #[test]
    #[ignore = "benchmark: use cargo test --ignored or nightly workflow"]
    fn bench_context_rendering() {
        let md_ir = compile_markdown_string(MARKDOWN_DOC, Some("CLAUDE.md".into())).unwrap();
        let code_ir = compile_rust_source(RUST_CODE, Some("lib.rs"));
        let merged = merge_knowledge_ir(&[md_ir, code_ir]);
        let pkg = compile_context("add constraint validation", &merged, 2000);

        let iterations = 500;
        let start = Instant::now();
        for _ in 0..iterations {
            let _md = render_context_markdown(&pkg);
        }
        let elapsed = start.elapsed();
        let per_sec = iterations as f64 / elapsed.as_secs_f64();

        eprintln!("=== BENCH: Context Markdown Rendering ===");
        eprintln!(
            "  {:.1} renders/sec ({:.2?} for {} iterations)",
            per_sec, elapsed, iterations
        );
        // Informational only — GitHub runners are non-deterministic.
        println!("  rendering throughput: {:.1} renders/sec (informational, not a CI gate)", per_sec);
    }

    /// Simulated agent task benchmark: measures completion rate, token savings,
    /// and knowledge accuracy vs raw-document baseline across representative
    /// engineering tasks.
    #[test]
    #[ignore = "benchmark: use cargo test --ignored or nightly workflow"]
    fn bench_agent_task_simulation() {
        // Prepare the knowledge graph.
        let md_ir = compile_markdown_string(MARKDOWN_DOC, Some("CLAUDE.md".into())).unwrap();
        let code_ir = compile_rust_source(RUST_CODE, Some("lib.rs"));
        let merged = merge_knowledge_ir(&[md_ir, code_ir]);

        let raw_tokens = (MARKDOWN_DOC.len() + RUST_CODE.len() + 3) / 4;

        // Define simulated agent tasks with expected answers.
        #[derive(Debug)]
        struct SimTask {
            description: &'static str,
            /// Entity names that should appear in the context for correctness.
            required_entities: &'static [&'static str],
            /// Fact substrings that should appear.
            required_facts: &'static [&'static str],
        }

        let tasks = [
            SimTask {
                description: "add a constraint validation rule that checks for circular references",
                required_entities: &["ConstraintEngine", "TransactionEngine"],
                required_facts: &["MVCC", "constraint"],
            },
            SimTask {
                description: "implement rate limiting for the authentication API",
                required_entities: &["AuthService"],
                required_facts: &["auth", "token"],
            },
            SimTask {
                description: "fix a bug in MVCC transaction isolation",
                required_entities: &["TransactionEngine"],
                required_facts: &["MVCC", "transaction"],
            },
            SimTask {
                description: "audit all places where UserId is used without validation",
                required_entities: &["AuthService"],
                required_facts: &["user", "auth"],
            },
            SimTask {
                description: "add new OAuth2 provider integration",
                required_entities: &["AuthService"],
                required_facts: &["OAuth2", "auth"],
            },
            SimTask {
                description: "write integration tests for constraint validation",
                required_entities: &["ConstraintEngine"],
                required_facts: &["constraint", "validate"],
            },
        ];

        let mut total_tokens_aikoql = 0u64;
        let mut total_tokens_raw = 0u64;
        let mut tasks_passing = 0u64;
        let mut entity_hits = 0u64;
        let mut fact_hits = 0u64;
        let mut total_required_entities = 0u64;
        let mut total_required_facts = 0u64;

        eprintln!("=== BENCH: Agent Task Simulation ===");
        for (i, task) in tasks.iter().enumerate() {
            total_required_entities += task.required_entities.len() as u64;
            total_required_facts += task.required_facts.len() as u64;

            // Compile context from aikoql.
            let pkg = compile_context(task.description, &merged, 0);
            let ctx_tokens = pkg.estimated_tokens as u64;
            total_tokens_aikoql += ctx_tokens;

            // Raw baseline: all tokens.
            total_tokens_raw += raw_tokens as u64;

            // Check entity recall.
            let ctx_entity_names: Vec<&str> =
                pkg.entities.iter().map(|e| e.name.as_str()).collect();
            let mut task_entity_hits = 0u64;
            for required in task.required_entities {
                if ctx_entity_names.contains(required) {
                    task_entity_hits += 1;
                }
            }
            entity_hits += task_entity_hits;

            // Check fact recall.
            let mut ctx_text = String::new();
            for e in &pkg.entities {
                for m in &e.mentions {
                    ctx_text.push_str(m);
                    ctx_text.push(' ');
                }
            }
            for f in &pkg.facts {
                ctx_text.push_str(&f.statement);
                ctx_text.push(' ');
            }
            let mut task_fact_hits = 0u64;
            for required in task.required_facts {
                if ctx_text.to_lowercase().contains(&required.to_lowercase()) {
                    task_fact_hits += 1;
                }
            }
            fact_hits += task_fact_hits;

            // Task passes if all required entities AND all required facts are found.
            let all_entities_found = task_entity_hits == task.required_entities.len() as u64;
            let all_facts_found = task_fact_hits == task.required_facts.len() as u64;
            let passed = all_entities_found && all_facts_found;
            if passed {
                tasks_passing += 1;
            }

            eprintln!(
                "  Task {} ({:<20}): entities={}/{}, facts={}/{}, tokens={}, {}",
                i + 1,
                if task.description.len() > 20 {
                    &task.description[..20]
                } else {
                    task.description
                },
                task_entity_hits,
                task.required_entities.len(),
                task_fact_hits,
                task.required_facts.len(),
                ctx_tokens,
                if passed { "PASS" } else { "FAIL" },
            );
        }

        let completion_rate = tasks_passing as f64 / tasks.len() as f64 * 100.0;
        let entity_recall = entity_hits as f64 / total_required_entities as f64 * 100.0;
        let fact_recall = fact_hits as f64 / total_required_facts as f64 * 100.0;
        let avg_tokens_aikoql = total_tokens_aikoql / tasks.len() as u64;
        let avg_tokens_raw = total_tokens_raw / tasks.len() as u64;
        let token_savings = (1.0 - avg_tokens_aikoql as f64 / avg_tokens_raw as f64) * 100.0;

        eprintln!("  ---");
        eprintln!(
            "  Completion rate:   {:.0}% ({}/{})",
            completion_rate,
            tasks_passing,
            tasks.len()
        );
        eprintln!(
            "  Entity recall:     {:.0}% ({}/{})",
            entity_recall, entity_hits, total_required_entities
        );
        eprintln!(
            "  Fact recall:       {:.0}% ({}/{})",
            fact_recall, fact_hits, total_required_facts
        );
        eprintln!("  Avg tokens (aikoql): {} / task", avg_tokens_aikoql);
        eprintln!("  Avg tokens (raw docs):  {} / task", avg_tokens_raw);
        eprintln!("  Token savings:     {:.1}%", token_savings);

        // Assertions: must meet quality thresholds.
        assert!(
            completion_rate >= 50.0,
            "at least 50% task completion rate (got {:.0}%)",
            completion_rate
        );
        assert!(
            entity_recall >= 50.0,
            "at least 50% entity recall (got {:.0}%)",
            entity_recall
        );
        assert!(
            token_savings >= 25.0,
            "at least 25% token savings vs raw docs (got {:.1}%)",
            token_savings
        );
    }

    /// Secret filtering performance: measure throughput for scanning
    /// typical knowledge documents.
    #[test]
    #[ignore = "benchmark: use cargo test --ignored or nightly workflow"]
    fn bench_secret_filtering_throughput() {
        let md_ir = compile_markdown_string(MARKDOWN_DOC, Some("CLAUDE.md".into())).unwrap();
        let iterations = 500;
        let start = Instant::now();
        for _ in 0..iterations {
            let (_redacted, _findings) = filter_secrets(&md_ir);
        }
        let elapsed = start.elapsed();
        let per_sec = iterations as f64 / elapsed.as_secs_f64();
        eprintln!("=== BENCH: Secret Filtering Throughput ===");
        eprintln!(
            "  {:.1} IR scans/sec ({:.2?} for {} iterations)",
            per_sec, elapsed, iterations
        );
        assert!(
            per_sec > 500.0,
            "secret filtering should exceed 500 scans/sec"
        );
    }

    /// Reconciliation workflow integration: reconcile → auto-proposals →
    /// validate → apply end-to-end.
    #[test]
    #[ignore = "benchmark: use cargo test --ignored or nightly workflow"]
    fn bench_reconciliation_workflow_e2e() {
        let md_ir = compile_markdown_string(MARKDOWN_DOC, Some("CLAUDE.md".into())).unwrap();
        let code_ir = compile_rust_source(RUST_CODE, Some("lib.rs"));
        let merged = merge_knowledge_ir(&[md_ir, code_ir]);

        // Step 1: Reconcile a change.
        let report = reconcile(&["crates/kernel/src/constraint.rs".to_string()], &merged);

        eprintln!("=== BENCH: Reconciliation Workflow E2E ===");
        eprintln!("  Affected entities: {}", report.affected_entities.len());
        eprintln!("  Stale facts: {}", report.potentially_stale_facts.len());

        // Step 2: Generate auto-proposals from affected entities.
        let stale_names: Vec<String> = report
            .affected_entities
            .iter()
            .map(|a| a.entity_name.clone())
            .collect();
        let proposals = auto_proposals_from_stale(&stale_names, "bench-agent");
        eprintln!("  Auto proposals: {}", proposals.len());

        // Step 3: Validate all proposals.
        let mut valid_count = 0;
        for prop in &proposals {
            let v = validate_proposal(prop, &merged);
            if v.valid {
                valid_count += 1;
            }
        }
        eprintln!("  Valid proposals: {}/{}", valid_count, proposals.len());

        // Step 4: Apply workflow to get final IR.
        let (final_ir, wf_report) = process_workflow(&proposals, &merged);
        eprintln!(
            "  Accepted: {}, Rejected: {}",
            wf_report.accepted, wf_report.rejected
        );
        eprintln!(
            "  Final IR entities: {} → {}",
            merged.entities.len(),
            final_ir.entities.len()
        );

        // Verify the workflow ran end-to-end.
        assert!(!report.summary.is_empty());
        if !stale_names.is_empty() {
            assert!(wf_report.total > 0);
        }
    }
}
