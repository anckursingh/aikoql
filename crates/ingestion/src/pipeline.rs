//! D9: Compiler Pipeline — end-to-end document knowledge compilation.
//!
//! Wires D1–D8 together into a single `compile_document()` call. Produces a
//! `CompilationResult` with the commit plan, embedded chunks, phase-level
//! statistics, and a full evidence trail for explainability.
//!
//! # Architecture
//! - `CompilationResult` — the output of a full compilation run
//! - `PipelineStats` — per-phase timing and counts
//! - `EvidenceTrail` — provenance chain from raw text to committed fact
//! - `compile_document()` — one-call pipeline: DocumentModel → result

use crate::ast::document_model_to_ast;
use crate::boundary::{KnowledgeBoundaryDetector, RuleBoundaryDetector};
use crate::chunking::{project_and_embed, EmbeddedChunk, HeadingProjector, RetrievalProjector};
use crate::commit::{
    reconcile_and_plan, CommitAction, KnowledgeCommitPlan, KnowledgeReconciler,
    MockKnowledgeReconciler,
};
use crate::embedding::{EmbeddingProvider, MockEmbeddingProvider};
use crate::fragment::KnowledgeFragment;
use crate::ir::{document_model_to_ir, Evidence, KnowledgeIr, SemanticAnalyzer};
use crate::ontology::{discover_ontology_from_ir, OntologyProposal};
use crate::resolution::{
    resolve_entities, EntityResolver, KnowledgeBaseEntry, MockEntityResolver, ResolutionResult,
};
use crate::secret_filter::{filter_secrets, SecretFinding};
use crate::DocumentModel;

// ---------------------------------------------------------------------------
// Pipeline stats
// ---------------------------------------------------------------------------

/// Per-phase timing and counts for a compilation run.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PhaseStats {
    /// Phase name (e.g. "D3-ast", "D4-ir").
    pub phase: String,
    /// Wall-clock duration in microseconds.
    pub duration_us: u64,
    /// Items produced (entities, chunks, actions, etc.).
    pub item_count: usize,
}

/// Aggregate statistics for a full compilation run.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PipelineStats {
    /// Per-phase stats in execution order.
    pub phases: Vec<PhaseStats>,
    /// Total wall-clock duration in microseconds.
    pub total_us: u64,
}

impl PipelineStats {
    fn add(&mut self, phase: &str, duration_us: u64, item_count: usize) {
        self.phases.push(PhaseStats {
            phase: phase.to_string(),
            duration_us,
            item_count,
        });
    }

    fn finish(&mut self, total_us: u64) {
        self.total_us = total_us;
    }
}

// ---------------------------------------------------------------------------
// Compilation result
// ---------------------------------------------------------------------------

/// The output of a full document knowledge compilation run (D1–D8).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CompilationResult {
    /// Intermediate knowledge IR from semantic analysis (secrets redacted).
    pub ir: KnowledgeIr,
    /// Ontology proposals discovered from the document.
    pub ontology: OntologyProposal,
    /// Entity resolution against the knowledge base.
    pub resolution: ResolutionResult,
    /// Reconciled commit plan (what to create/update/skip/review).
    pub commit_plan: KnowledgeCommitPlan,
    /// Embedded document chunks for vector retrieval.
    pub embedded_chunks: Vec<EmbeddedChunk>,
    /// D4-fragments: modality-preserving knowledge fragments (canonical
    /// segmentation). Chunks are a retrieval projection of these, never
    /// their replacement (HLD §59).
    #[serde(default)]
    pub fragments: Vec<KnowledgeFragment>,
    /// Full evidence trail for explainability.
    pub evidence_trail: EvidenceTrail,
    /// Phase-level statistics.
    pub stats: PipelineStats,
    /// Secrets/PII detected and redacted during compilation (R8 remediation).
    pub secret_findings: Vec<SecretFinding>,
}

impl CompilationResult {
    /// Count of entities ready for automatic commit (matched + unambiguous creates).
    pub fn auto_commit_count(&self) -> usize {
        self.commit_plan.stats.creates + self.commit_plan.stats.updates
    }

    /// Count of entities needing human review.
    pub fn review_count(&self) -> usize {
        self.commit_plan.stats.needs_review
    }

    /// True when the document was fully processed with no review needed.
    pub fn is_clean(&self) -> bool {
        self.commit_plan.stats.needs_review == 0 && self.commit_plan.stats.total_conflicts == 0
    }
}

// ---------------------------------------------------------------------------
// Evidence trail — provenance from raw text to committed fact
// ---------------------------------------------------------------------------

/// A single node in the evidence chain.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EvidenceNode {
    /// What happened at this step (e.g. "extracted entity 'Acme Corp' from page 3").
    pub step: String,
    /// The pipeline phase that produced this evidence.
    pub phase: String,
    /// Source evidence from the document.
    pub source: Vec<Evidence>,
    /// Entities involved.
    pub entities: Vec<String>,
}

/// Full provenance chain from raw document text through every pipeline phase.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct EvidenceTrail {
    /// Ordered chain of evidence nodes.
    pub nodes: Vec<EvidenceNode>,
    /// Source document identifier.
    pub document_id: Option<String>,
}

impl EvidenceTrail {
    /// Build an evidence trail from all pipeline outputs.
    pub fn from_pipeline(
        document_id: Option<&str>,
        ir: &KnowledgeIr,
        ontology: &OntologyProposal,
        resolution: &ResolutionResult,
        commit_plan: &KnowledgeCommitPlan,
    ) -> Self {
        let mut nodes: Vec<EvidenceNode> = Vec::new();

        let doc_id = document_id.map(|s| s.to_string());

        // D4: Entities extracted.
        if !ir.entities.is_empty() {
            nodes.push(EvidenceNode {
                step: format!(
                    "Extracted {} entit{} from document text",
                    ir.entities.len(),
                    if ir.entities.len() == 1 { "y" } else { "ies" }
                ),
                phase: "D4-semantic-ir".into(),
                source: ir.entities.iter().map(|e| e.evidence.clone()).collect(),
                entities: ir.entities.iter().map(|e| e.name.clone()).collect(),
            });
        }

        // D4: Facts extracted.
        if !ir.facts.is_empty() {
            nodes.push(EvidenceNode {
                step: format!(
                    "Extracted {} fact{} from document structure",
                    ir.facts.len(),
                    if ir.facts.len() == 1 { "" } else { "s" }
                ),
                phase: "D4-semantic-ir".into(),
                source: ir.facts.iter().map(|f| f.evidence.clone()).collect(),
                entities: vec![],
            });
        }

        // D4: Temporal assertions.
        if !ir.temporal.is_empty() {
            nodes.push(EvidenceNode {
                step: format!(
                    "Found {} temporal assertion{}",
                    ir.temporal.len(),
                    if ir.temporal.len() == 1 { "" } else { "s" }
                ),
                phase: "D4-semantic-ir".into(),
                source: ir.temporal.iter().map(|t| t.evidence.clone()).collect(),
                entities: vec![],
            });
        }

        // D5: Ontology proposals.
        nodes.push(EvidenceNode {
            step: format!(
                "Discovered {} class{}, {} propert{}, {} relationship{}",
                ontology.classes.len(),
                if ontology.classes.len() == 1 {
                    ""
                } else {
                    "es"
                },
                ontology.properties.len(),
                if ontology.properties.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                ontology.relationships.len(),
                if ontology.relationships.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            phase: "D5-ontology".into(),
            source: vec![],
            entities: vec![],
        });

        // D6: Entity resolution.
        nodes.push(EvidenceNode {
            step: format!(
                "Resolved {} entities: {} matched, {} ambiguous, {} unmatched",
                resolution.stats.total_entities,
                resolution.stats.matched_count,
                resolution.stats.ambiguous_count,
                resolution.stats.unmatched_count
            ),
            phase: "D6-resolution".into(),
            source: vec![],
            entities: resolution
                .matched
                .iter()
                .map(|m| m.entity_name.clone())
                .chain(resolution.ambiguous.iter().map(|a| a.entity_name.clone()))
                .chain(resolution.unmatched.iter().map(|u| u.entity_name.clone()))
                .collect(),
        });

        // D7: Reconciliation.
        nodes.push(EvidenceNode {
            step: format!(
                "Reconciled: {} create{}, {} update{}, {} skip{}, {} need{} review, {} conflict{}",
                commit_plan.stats.creates,
                if commit_plan.stats.creates == 1 {
                    ""
                } else {
                    "s"
                },
                commit_plan.stats.updates,
                if commit_plan.stats.updates == 1 {
                    ""
                } else {
                    "s"
                },
                commit_plan.stats.skips,
                if commit_plan.stats.skips == 1 {
                    ""
                } else {
                    "s"
                },
                commit_plan.stats.needs_review,
                if commit_plan.stats.needs_review == 1 {
                    "s"
                } else {
                    ""
                },
                commit_plan.stats.total_conflicts,
                if commit_plan.stats.total_conflicts == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            phase: "D7-reconcile".into(),
            source: commit_plan
                .actions
                .iter()
                .filter_map(|a| match a {
                    CommitAction::NeedsReview { conflicts, .. } => Some(
                        conflicts
                            .iter()
                            .flat_map(|c| c.evidence.clone())
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .flatten()
                .collect(),
            entities: commit_plan
                .actions
                .iter()
                .map(|a| match a {
                    CommitAction::CreateKO { entity_name, .. } => entity_name.clone(),
                    CommitAction::UpdateKO { entity_name, .. } => entity_name.clone(),
                    CommitAction::Skip { entity_name, .. } => entity_name.clone(),
                    CommitAction::NeedsReview { entity_name, .. } => entity_name.clone(),
                })
                .collect(),
        });

        EvidenceTrail {
            nodes,
            document_id: doc_id,
        }
    }

    /// Iterate nodes from a specific phase.
    pub fn nodes_for_phase(&self, phase: &str) -> Vec<&EvidenceNode> {
        self.nodes.iter().filter(|n| n.phase == phase).collect()
    }

    /// Total evidence items across all nodes.
    pub fn total_evidence_items(&self) -> usize {
        self.nodes.iter().map(|n| n.source.len()).sum()
    }
}

// ---------------------------------------------------------------------------
// Full compiler pipeline
// ---------------------------------------------------------------------------

/// Run the full document knowledge compiler: DocumentModel → CompilationResult.
///
/// This is the single-call entry point for document ingestion. It wires together
/// every phase from D3 (AST) through D8 (chunking + embedding), producing a
/// complete compilation result with commit plan, embedded chunks, and evidence trail.
///
/// All phases use mock implementations by default. Swap in real implementations
/// (LLM semantic analyzer, production embedding provider, etc.) for production use.
pub fn compile_document(
    doc: &DocumentModel,
    analyzer: &dyn SemanticAnalyzer,
    resolver: &dyn EntityResolver,
    reconciler: &dyn KnowledgeReconciler,
    projector: &dyn RetrievalProjector,
    embedder: &dyn EmbeddingProvider,
    existing_kos: &[KnowledgeBaseEntry],
) -> CompilationResult {
    let t0 = time_now();

    // D3: DocumentModel → DocumentAst
    let t_ast = time_now();
    let ast = document_model_to_ast(doc);
    let dt_ast = time_now() - t_ast;

    // D4-fragments: DocumentAst → KnowledgeFragment[] (semantic segmentation).
    // Rule-based now; fails soft so ingestion never hard-fails on it.
    let t_frag = time_now();
    let fragments = match RuleBoundaryDetector.detect(&ast) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "boundary detection degraded: {} — continuing without fragments",
                e
            );
            Vec::new()
        }
    };
    let dt_frag = time_now() - t_frag;

    // D4: DocumentAst → KnowledgeIr
    let t_ir = time_now();
    let raw_ir = document_model_to_ir(doc, analyzer);
    let dt_ir = time_now() - t_ir;

    // R8: scan and redact secrets/PII before IR flows into commit planning
    let (ir, secret_findings) = filter_secrets(&raw_ir);

    // D5: KnowledgeIr → OntologyProposal
    let t_onto = time_now();
    let ontology = discover_ontology_from_ir(&ir);
    let dt_onto = time_now() - t_onto;

    // D6: Entity resolution
    let t_res = time_now();
    let resolution = resolve_entities(&ir, resolver, existing_kos);
    let dt_res = time_now() - t_res;

    // D7: Reconciliation → KnowledgeCommitPlan
    let t_rec = time_now();
    let commit_plan = reconcile_and_plan(&ir, &ontology, &resolution, existing_kos, reconciler);
    let dt_rec = time_now() - t_rec;

    // D8: Retrieval projection + embedding — chunks derive from canonical
    // fragments (PR-E), never from the raw AST.
    let t_chk = time_now();
    let embedded_chunks = project_and_embed(&fragments, Some(&ir), projector, embedder);
    let dt_chk = time_now() - t_chk;

    // Evidence trail
    let evidence_trail = EvidenceTrail::from_pipeline(
        ir.document_id.as_deref(),
        &ir,
        &ontology,
        &resolution,
        &commit_plan,
    );

    // Stats
    let total = time_now() - t0;
    let mut stats = PipelineStats::default();
    stats.add("D3-ast", dt_ast, ast.pages.len());
    stats.add("D4-fragments", dt_frag, fragments.len());
    stats.add("D4-ir", dt_ir, ir.total_candidates());
    stats.add(
        "D5-ontology",
        dt_onto,
        ontology.classes.len() + ontology.properties.len() + ontology.relationships.len(),
    );
    stats.add("D6-resolution", dt_res, resolution.stats.total_entities);
    stats.add("D7-reconcile", dt_rec, commit_plan.stats.total_actions);
    stats.add("D8-projection", dt_chk, embedded_chunks.len());
    stats.finish(total);

    CompilationResult {
        ir,
        ontology,
        resolution,
        commit_plan,
        embedded_chunks,
        fragments,
        evidence_trail,
        stats,
        secret_findings,
    }
}

/// Convenience: run the full pipeline with all-mock implementations.
pub fn compile_document_mock(
    doc: &DocumentModel,
    existing_kos: &[KnowledgeBaseEntry],
) -> CompilationResult {
    let analyzer = crate::MockSemanticAnalyzer::new();
    let resolver = MockEntityResolver::new();
    let reconciler = MockKnowledgeReconciler::new();
    let projector = HeadingProjector::new();
    let embedder = MockEmbeddingProvider::new();
    compile_document(
        doc,
        &analyzer,
        &resolver,
        &reconciler,
        &projector,
        &embedder,
        existing_kos,
    )
}

/// Monotonic timer in microseconds (not wall-clock — never goes backward).
/// ponytail: uses std Instant, sufficient for pipeline profiling.
fn time_now() -> u64 {
    use std::time::Instant;
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    start.elapsed().as_micros() as u64
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PageModel;

    fn test_doc() -> DocumentModel {
        DocumentModel {
            page_count: 1,
            pages: vec![PageModel {
                page_number: 1,
                text: "Acme Corporation\n\nAnnual Report for Fiscal Year 2024\n\n\
                       Prepared by Globex Industries in January 2025.\n\n\
                       The Board of Directors approved the report on 2025-02-15.\n\n\
                       Revenue reached $10M for Q3 2025."
                    .into(),
                char_count: 230,
                source: "native".into(),
                ocr_confidence: None,
            }],
            total_chars: 230,
            ocr_stats: None,
        }
    }

    fn sample_kb() -> Vec<KnowledgeBaseEntry> {
        vec![KnowledgeBaseEntry {
            koid: "ko-acme".into(),
            name: "Acme Corporation".into(),
            type_name: "Organization".into(),
            aliases: vec!["Acme Corp".into()],
            properties: vec![],
        }]
    }

    // ── Full pipeline ──

    #[test]
    fn compile_document_produces_non_empty_result() {
        let doc = test_doc();
        let result = compile_document_mock(&doc, &sample_kb());

        assert!(!result.ir.is_empty());
        assert!(!result.embedded_chunks.is_empty());
        assert_eq!(result.stats.phases.len(), 7);
    }

    #[test]
    fn compile_document_stats_cover_all_phases() {
        let doc = test_doc();
        let result = compile_document_mock(&doc, &[]);

        let phase_names: Vec<&str> = result
            .stats
            .phases
            .iter()
            .map(|p| p.phase.as_str())
            .collect();
        assert!(phase_names.contains(&"D3-ast"));
        assert!(phase_names.contains(&"D4-fragments"));
        assert!(phase_names.contains(&"D4-ir"));
        assert!(phase_names.contains(&"D5-ontology"));
        assert!(phase_names.contains(&"D6-resolution"));
        assert!(phase_names.contains(&"D7-reconcile"));
        assert!(phase_names.contains(&"D8-projection"));
        assert!(result.stats.total_us > 0);
    }

    #[test]
    fn compile_document_with_existing_kb_matches() {
        let doc = test_doc();
        let result = compile_document_mock(&doc, &sample_kb());

        // Acme Corporation is in the KB → should be matched (resolved).
        let acme_resolved = result
            .resolution
            .matched
            .iter()
            .any(|r| r.entity_name == "Acme Corporation");
        assert!(acme_resolved);
    }

    #[test]
    fn compile_document_tracks_entities_through_pipeline() {
        let doc = test_doc();
        let result = compile_document_mock(&doc, &[]);

        // Entities found in IR should appear in resolution.
        let ir_names: Vec<&str> = result.ir.entities.iter().map(|e| e.name.as_str()).collect();
        let res_names: std::collections::HashSet<&str> = result
            .resolution
            .matched
            .iter()
            .map(|r| r.entity_name.as_str())
            .chain(
                result
                    .resolution
                    .unmatched
                    .iter()
                    .map(|r| r.entity_name.as_str()),
            )
            .chain(
                result
                    .resolution
                    .ambiguous
                    .iter()
                    .map(|r| r.entity_name.as_str()),
            )
            .collect();

        for name in &ir_names {
            assert!(
                res_names.contains(name),
                "entity '{}' not in resolution",
                name
            );
        }
    }

    // ── CompilationResult helpers ──

    #[test]
    fn auto_commit_count_sums_creates_and_updates() {
        let doc = test_doc();
        let result = compile_document_mock(&doc, &sample_kb());

        let expected = result.commit_plan.stats.creates + result.commit_plan.stats.updates;
        assert_eq!(result.auto_commit_count(), expected);
    }

    #[test]
    fn is_clean_when_no_conflicts() {
        // A doc with no KB entries — all unmatched → creates, no conflicts.
        let doc = DocumentModel {
            page_count: 1,
            pages: vec![PageModel {
                page_number: 1,
                text: "Hello World Corp.".into(),
                char_count: 20,
                source: "native".into(),
                ocr_confidence: None,
            }],
            total_chars: 20,
            ocr_stats: None,
        };
        let result = compile_document_mock(&doc, &[]);
        assert!(result.is_clean());
    }

    #[test]
    fn review_count_matches_needs_review() {
        let doc = test_doc();
        let result = compile_document_mock(&doc, &[]);
        assert_eq!(result.review_count(), result.commit_plan.stats.needs_review);
    }

    // ── Evidence trail ──

    #[test]
    fn evidence_trail_covers_all_phases() {
        let doc = test_doc();
        let result = compile_document_mock(&doc, &sample_kb());

        let trail = &result.evidence_trail;
        assert!(
            trail.nodes.len() >= 5,
            "expected >=5 nodes, got {}",
            trail.nodes.len()
        );

        let phases: Vec<&str> = trail.nodes.iter().map(|n| n.phase.as_str()).collect();
        assert!(phases.contains(&"D4-semantic-ir"));
        assert!(phases.contains(&"D5-ontology"));
        assert!(phases.contains(&"D6-resolution"));
        assert!(phases.contains(&"D7-reconcile"));
    }

    #[test]
    fn evidence_trail_nodes_for_phase_filters() {
        let doc = test_doc();
        let result = compile_document_mock(&doc, &[]);

        let d4_nodes: Vec<&EvidenceNode> = result.evidence_trail.nodes_for_phase("D4-semantic-ir");
        assert!(!d4_nodes.is_empty());
    }

    #[test]
    fn evidence_trail_total_items() {
        let doc = test_doc();
        let result = compile_document_mock(&doc, &[]);

        let total = result.evidence_trail.total_evidence_items();
        assert!(total > 0);
    }

    #[test]
    fn evidence_trail_default_is_empty() {
        let trail = EvidenceTrail::default();
        assert!(trail.nodes.is_empty());
        assert_eq!(trail.total_evidence_items(), 0);
    }

    // ── Edge cases ──

    #[test]
    fn empty_document_compiles() {
        let doc = DocumentModel {
            page_count: 0,
            pages: vec![],
            total_chars: 0,
            ocr_stats: None,
        };
        let result = compile_document_mock(&doc, &[]);
        assert!(result.ir.is_empty());
        assert!(result.embedded_chunks.is_empty());
    }

    #[test]
    fn whitespace_only_document_compiles() {
        let doc = DocumentModel {
            page_count: 1,
            pages: vec![PageModel {
                page_number: 1,
                text: "   \n\n   ".into(),
                char_count: 7,
                source: "native".into(),
                ocr_confidence: None,
            }],
            total_chars: 7,
            ocr_stats: None,
        };
        let result = compile_document_mock(&doc, &[]);
        // No capitalised entities, no facts → empty IR.
        assert!(result.ir.is_empty());
    }

    // ── Integration: compile_document with custom implementations ──

    #[test]
    fn compile_with_custom_embedder() {
        let doc = test_doc();
        let embedder = MockEmbeddingProvider::with_dimensions(64);

        let result = compile_document(
            &doc,
            &crate::MockSemanticAnalyzer::new(),
            &MockEntityResolver::new(),
            &MockKnowledgeReconciler::new(),
            &HeadingProjector::new(),
            &embedder,
            &[],
        );

        for ec in &result.embedded_chunks {
            assert_eq!(ec.embedding.len(), 64);
        }
    }

    #[test]
    fn compile_with_strict_resolver_ambiguous() {
        let doc = DocumentModel {
            page_count: 1,
            pages: vec![PageModel {
                page_number: 1,
                text: "Acme Corp and Acme Corporation are related.".into(),
                char_count: 50,
                source: "native".into(),
                ocr_confidence: None,
            }],
            total_chars: 50,
            ocr_stats: None,
        };
        let kb = vec![KnowledgeBaseEntry {
            koid: "ko-acme-corp".into(),
            name: "Acme Corp".into(),
            type_name: "Organization".into(),
            aliases: vec![],
            properties: vec![],
        }];

        let resolver = MockEntityResolver::with_thresholds(0.7, 0.1);
        let result = compile_document(
            &doc,
            &crate::MockSemanticAnalyzer::new(),
            &resolver,
            &MockKnowledgeReconciler::new(),
            &HeadingProjector::new(),
            &MockEmbeddingProvider::new(),
            &kb,
        );

        // "Acme Corp" exact match → matched. "Acme Corporation" substring → matched (score >= 0.7).
        // Both should be resolved.
        assert!(result.resolution.ambiguous.is_empty() || result.resolution.unmatched.is_empty());
    }

    // ── Pipeline stats ──

    #[test]
    fn pipeline_stats_default() {
        let stats = PipelineStats::default();
        assert!(stats.phases.is_empty());
        assert_eq!(stats.total_us, 0);
    }
}
