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
use crate::ir::{Evidence, KnowledgeIr, SemanticAnalyzer};
use crate::merge::merge_knowledge_ir;
use crate::ontology::{discover_ontology_from_ir, OntologyProposal};
use crate::resolution::{
    resolve_entities, EntityResolver, KnowledgeBaseEntry, MockEntityResolver, ResolutionResult,
};
use crate::secret_filter::{filter_secrets, SecretFinding};
use crate::{DocumentModel, PageModel};
use std::collections::{BTreeMap, HashSet};

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
    asset_dir: Option<&str>,
) -> CompilationResult {
    compile_document_with_detector(
        doc,
        analyzer,
        resolver,
        reconciler,
        projector,
        embedder,
        &RuleBoundaryDetector,
        existing_kos,
        asset_dir,
    )
}

/// `compile_document` with a pluggable boundary detector (PR-H, HLD §16/§60):
/// the D4 segmentation step runs `detector.detect(&ast)` — the seam the
/// embedding / transformer / hybrid variants plug into. The rule detector
/// remains the default.
pub fn compile_document_with_detector(
    doc: &DocumentModel,
    analyzer: &dyn SemanticAnalyzer,
    resolver: &dyn EntityResolver,
    reconciler: &dyn KnowledgeReconciler,
    projector: &dyn RetrievalProjector,
    embedder: &dyn EmbeddingProvider,
    detector: &dyn KnowledgeBoundaryDetector,
    existing_kos: &[KnowledgeBaseEntry],
    asset_dir: Option<&str>,
) -> CompilationResult {
    let t0 = time_now();

    // D3: DocumentModel → DocumentAst (+ PR-F visual classification:
    // extracted images get payloads, captioned figures re-typed).
    let t_ast = time_now();
    let mut ast = document_model_to_ast(doc);
    // PR-F: extracted images get payloads, captioned figures re-typed.
    // With an asset dir, Screenshot/ScannedText images also get an OCR
    // fill (HLD §33: OCR only if needed — provider gates itself).
    match asset_dir {
        Some(dir) => crate::visual::classify_visuals_with_assets(
            &mut ast,
            Some(dir),
            &crate::ocr::TesseractCli::new(),
        ),
        None => crate::visual::classify_visuals(&mut ast),
    };
    let dt_ast = time_now() - t_ast;

    // D4-fragments: DocumentAst → KnowledgeFragment[] (semantic segmentation).
    // Pluggable detector (rule by default); fails soft so ingestion never
    // hard-fails on it.
    let t_frag = time_now();
    let fragments = match detector.detect(&ast) {
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

    // D4: DocumentAst → KnowledgeIr — the semantic leg consumes the
    // fragment stream (HLD §57). Analyzers fall back to the AST when the
    // boundary stream is empty (degraded detection).
    let t_ir = time_now();
    let raw_ir = analyzer.analyze(&ast, &fragments);
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
    compile_document_mock_with_assets(doc, existing_kos, None)
}

/// Mock pipeline with an asset dir: Screenshot/ScannedText images OCR'd
/// from persisted assets when tesseract is available (PR-F, HLD §33).
pub fn compile_document_mock_with_assets(
    doc: &DocumentModel,
    existing_kos: &[KnowledgeBaseEntry],
    asset_dir: Option<&str>,
) -> CompilationResult {
    compile_document_mock_with_detector(doc, existing_kos, asset_dir, &RuleBoundaryDetector)
}

/// Mock pipeline with a pluggable boundary detector (PR-H, HLD §16/§60):
/// the benchmark seam that runs a detector variant end to end.
pub fn compile_document_mock_with_detector(
    doc: &DocumentModel,
    existing_kos: &[KnowledgeBaseEntry],
    asset_dir: Option<&str>,
    detector: &dyn KnowledgeBoundaryDetector,
) -> CompilationResult {
    let analyzer = crate::MockSemanticAnalyzer::new();
    let resolver = MockEntityResolver::new();
    let reconciler = MockKnowledgeReconciler::new();
    let projector = HeadingProjector::new();
    let embedder = MockEmbeddingProvider::new();
    compile_document_with_detector(
        doc,
        &analyzer,
        &resolver,
        &reconciler,
        &projector,
        &embedder,
        detector,
        existing_kos,
        asset_dir,
    )
}

// ---------------------------------------------------------------------------
// HLD §45: incremental compilation
// ---------------------------------------------------------------------------

/// Change kind for an embedded image asset.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AssetChange {
    Added,
    Changed,
    Removed,
}

/// One image asset that changed between two document revisions.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImageDelta {
    /// Page the asset lives on (1-based).
    pub page: u32,
    pub content_hash: String,
    pub change: AssetChange,
}

/// Difference between two revisions of the same document (HLD §45).
///
/// Detection granularity is the asset (page text, per-image content hash);
/// processing granularity is the page — a changed asset marks its page for
/// reprocessing. `changed_pages` includes pages added in `next`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DocumentDelta {
    pub changed_pages: Vec<u32>,
    /// Pages present in `prev` but absent from `next`.
    pub removed_pages: Vec<u32>,
    /// Image-level deltas (every entry's page is also in `changed_pages`).
    pub changed_images: Vec<ImageDelta>,
}

impl DocumentDelta {
    /// True when the revisions are identical — nothing to reprocess.
    pub fn is_empty(&self) -> bool {
        self.changed_pages.is_empty() && self.removed_pages.is_empty()
    }
}

/// Diff two document revisions at asset granularity (HLD §45).
///
/// Pages are matched by `PageModel.page_number` (extractors number pages
/// contiguously, so page identity is stable). A page changes when its text
/// or any embedded image hash changes. Image deltas are matched by
/// page + slot index: a different hash in the same slot is `Changed`,
/// extras are `Added`, missing ones `Removed`.
/// ponytail: slot matching assumes stable per-page image order (true for
/// our extractors); bbox matching if a source ever reorders images.
pub fn diff_document_models(prev: &DocumentModel, next: &DocumentModel) -> DocumentDelta {
    let mut delta = DocumentDelta::default();
    let prev_pages: BTreeMap<u32, &PageModel> =
        prev.pages.iter().map(|p| (p.page_number, p)).collect();
    let next_pages: BTreeMap<u32, &PageModel> =
        next.pages.iter().map(|p| (p.page_number, p)).collect();

    for pn in prev_pages.keys() {
        if !next_pages.contains_key(pn) {
            delta.removed_pages.push(*pn);
        }
    }

    for (pn, next_page) in &next_pages {
        let prev_page = match prev_pages.get(pn) {
            Some(p) => p,
            None => {
                delta.changed_pages.push(*pn); // page added
                continue;
            }
        };
        let text_changed = prev_page.text != next_page.text;
        let prev_hashes: Vec<&str> = prev_page
            .images
            .iter()
            .map(|i| i.asset.content_hash.as_str())
            .collect();
        let next_hashes: Vec<&str> = next_page
            .images
            .iter()
            .map(|i| i.asset.content_hash.as_str())
            .collect();
        let mut image_changed = false;
        let shared = prev_hashes.len().min(next_hashes.len());
        for i in 0..shared {
            if prev_hashes[i] != next_hashes[i] {
                delta.changed_images.push(ImageDelta {
                    page: *pn,
                    content_hash: next_hashes[i].to_string(),
                    change: AssetChange::Changed,
                });
                image_changed = true;
            }
        }
        for h in &next_hashes[shared..] {
            delta.changed_images.push(ImageDelta {
                page: *pn,
                content_hash: (*h).to_string(),
                change: AssetChange::Added,
            });
            image_changed = true;
        }
        for h in &prev_hashes[shared..] {
            delta.changed_images.push(ImageDelta {
                page: *pn,
                content_hash: (*h).to_string(),
                change: AssetChange::Removed,
            });
            image_changed = true;
        }
        if text_changed || image_changed {
            delta.changed_pages.push(*pn);
        }
    }

    delta
}

/// Compile `doc` against a previous compilation of `prev_doc` (HLD §45).
///
/// - identical revisions → the previous result is returned untouched (skip);
/// - every page changed → full `compile_document`;
/// - otherwise → page splice: only changed pages are re-parsed, classified
///   and OCR'd; unchanged pages keep their fragments, IR candidates and
///   embedded chunks from the previous run. D5–D8 re-run over the merged IR.
///
/// Unchanged pages are replaced with empty placeholders in the spliced
/// model so the AST's index-based page numbering stays identical to the
/// full document.
/// ponytail: heading context for a changed page comes only from that page
/// (boundary detection clears heading paths at page boundaries) — a heading
/// living on an unchanged page is lost for the splice. Asset-level splicing
/// is the upgrade path. The spliced compile also inherits position-based
/// fragment ids; when DocumentAst.document_id is wired to extraction the
/// spliced AST must carry the same id.
#[allow(clippy::too_many_arguments)]
pub fn compile_document_incremental(
    doc: &DocumentModel,
    prev_doc: &DocumentModel,
    prev: &CompilationResult,
    analyzer: &dyn SemanticAnalyzer,
    resolver: &dyn EntityResolver,
    reconciler: &dyn KnowledgeReconciler,
    projector: &dyn RetrievalProjector,
    embedder: &dyn EmbeddingProvider,
    existing_kos: &[KnowledgeBaseEntry],
    asset_dir: Option<&str>,
) -> CompilationResult {
    let t0 = time_now();
    let delta = diff_document_models(prev_doc, doc);
    let t_diff = time_now() - t0;

    // Document unchanged → skip (the previous compilation is the answer).
    if delta.is_empty() {
        return prev.clone();
    }

    // Every page changed → nothing worth splicing.
    if delta.changed_pages.len() == doc.pages.len() {
        return compile_document(
            doc,
            analyzer,
            resolver,
            reconciler,
            projector,
            embedder,
            existing_kos,
            asset_dir,
        );
    }

    let changed: HashSet<u32> = delta.changed_pages.iter().copied().collect();
    let removed: HashSet<u32> = delta.removed_pages.iter().copied().collect();
    let dropped: HashSet<u32> = changed.union(&removed).copied().collect();
    let kept: HashSet<u32> = prev_doc
        .pages
        .iter()
        .map(|p| p.page_number)
        .filter(|p| !dropped.contains(p))
        .collect();

    // Spliced model: changed pages carry content, unchanged pages are empty
    // placeholders so page numbering (index + 1) matches the full document.
    let spliced_pages: Vec<PageModel> = doc
        .pages
        .iter()
        .map(|p| {
            if changed.contains(&p.page_number) {
                p.clone()
            } else {
                PageModel {
                    page_number: p.page_number,
                    text: String::new(),
                    char_count: 0,
                    source: "native".into(),
                    ocr_confidence: None,
                    images: vec![],
                }
            }
        })
        .collect();
    let spliced = DocumentModel {
        page_count: doc.page_count,
        total_chars: spliced_pages.iter().map(|p| p.char_count).sum(),
        pages: spliced_pages,
        ocr_stats: None,
    };

    let fresh = compile_document(
        &spliced,
        analyzer,
        resolver,
        reconciler,
        projector,
        embedder,
        existing_kos,
        asset_dir,
    );

    // Fragments: kept pages from prev, changed pages fresh, interleaved in
    // document order; neighbor links re-stamped across the splice boundary.
    let fresh_by_page: BTreeMap<u32, Vec<KnowledgeFragment>> = fresh
        .fragments
        .iter()
        .cloned()
        .fold(BTreeMap::new(), |mut m, f| {
            if let Some(p) = f.context.page {
                m.entry(p).or_default().push(f);
            }
            m
        });
    let prev_by_page: BTreeMap<u32, Vec<KnowledgeFragment>> = prev
        .fragments
        .iter()
        .filter(|f| f.context.page.is_none_or(|p| kept.contains(&p)))
        .cloned()
        .fold(BTreeMap::new(), |mut m, f| {
            if let Some(p) = f.context.page {
                m.entry(p).or_default().push(f);
            }
            m
        });
    let mut fragments: Vec<KnowledgeFragment> = Vec::new();
    for page in 1..=doc.pages.len() as u32 {
        let src = if changed.contains(&page) {
            &fresh_by_page
        } else {
            &prev_by_page
        };
        if let Some(fs) = src.get(&page) {
            fragments.extend(fs.iter().cloned());
        }
    }
    let ids: Vec<String> = fragments.iter().map(|f| f.fragment_id.clone()).collect();
    for (i, frag) in fragments.iter_mut().enumerate() {
        let mut neighbors = Vec::with_capacity(2);
        if i > 0 {
            neighbors.push(ids[i - 1].clone());
        }
        if i + 1 < ids.len() {
            neighbors.push(ids[i + 1].clone());
        }
        frag.context.neighboring_fragments = neighbors;
    }

    // IR: kept pages from prev + fresh changed-page candidates, merged
    // (entity/fact/triple dedup collapses re-derived candidates).
    let mut kept_ir = prev.ir.clone();
    kept_ir.retain_pages(&kept);
    let merged_ir = merge_knowledge_ir(&[kept_ir, fresh.ir.clone()]);

    // D5–D8 over the merged IR.
    let t_onto = time_now();
    let ontology = discover_ontology_from_ir(&merged_ir);
    let dt_onto = time_now() - t_onto;
    let t_res = time_now();
    let resolution = resolve_entities(&merged_ir, resolver, existing_kos);
    let dt_res = time_now() - t_res;
    let t_rec = time_now();
    let commit_plan =
        reconcile_and_plan(&merged_ir, &ontology, &resolution, existing_kos, reconciler);
    let dt_rec = time_now() - t_rec;

    // D8: kept pages reuse their embedded chunks (their projection inputs
    // are unchanged); changed pages take fresh chunks, ordered by page.
    let t_chk = time_now();
    let mut embedded_chunks: Vec<EmbeddedChunk> = prev
        .embedded_chunks
        .iter()
        .filter(|c| {
            kept.contains(&c.chunk.position.start_page) && kept.contains(&c.chunk.position.end_page)
        })
        .cloned()
        .chain(fresh.embedded_chunks.iter().cloned())
        .collect();
    embedded_chunks.sort_by_key(|c| (c.chunk.position.start_page, c.chunk.position.chunk_index));
    let dt_chk = time_now() - t_chk;

    // Secrets: fail-closed — kept findings stay reported even if their page
    // was not re-scanned; fresh findings append (deduped).
    let mut secret_findings = prev.secret_findings.clone();
    for s in fresh.secret_findings {
        let dup = secret_findings
            .iter()
            .any(|e| e.kind == s.kind && e.location == s.location && e.redacted == s.redacted);
        if !dup {
            secret_findings.push(s);
        }
    }

    let evidence_trail = EvidenceTrail::from_pipeline(
        merged_ir.document_id.as_deref(),
        &merged_ir,
        &ontology,
        &resolution,
        &commit_plan,
    );

    let mut stats = PipelineStats::default();
    stats.add("D9-diff", t_diff, delta.changed_pages.len());
    stats.add("D4-ir", t_diff, merged_ir.total_candidates());
    stats.add(
        "D5-ontology",
        dt_onto,
        ontology.classes.len() + ontology.properties.len() + ontology.relationships.len(),
    );
    stats.add("D6-resolution", dt_res, resolution.stats.total_entities);
    stats.add("D7-reconcile", dt_rec, commit_plan.stats.total_actions);
    stats.add("D8-projection", dt_chk, embedded_chunks.len());
    stats.finish(time_now() - t0);

    CompilationResult {
        ir: merged_ir,
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

/// HLD §45: semantic model changed → re-run the semantic projection only.
///
/// Fragments and IR stay as compiled; chunks are re-projected and embedded
/// with the (new) provider. The caller decides when the model changed
/// (e.g. by comparing `Evidence.model` stamps against its deployed model
/// versions).
pub fn reproject_document(
    prev: &CompilationResult,
    projector: &dyn RetrievalProjector,
    embedder: &dyn EmbeddingProvider,
) -> CompilationResult {
    let t0 = time_now();
    let embedded_chunks = project_and_embed(&prev.fragments, Some(&prev.ir), projector, embedder);
    let dt = time_now() - t0;
    let mut result = prev.clone();
    result.embedded_chunks = embedded_chunks;
    result
        .stats
        .add("D8-reproject", dt, result.embedded_chunks.len());
    result.stats.finish(result.stats.total_us + dt);
    result
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
                images: vec![],
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
                images: vec![],
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
                images: vec![],
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
            None,
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
                images: vec![],
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
            None,
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

    // ── HLD §45: incremental compilation ──

    fn page(num: u32, text: &str) -> PageModel {
        PageModel {
            page_number: num,
            text: text.into(),
            char_count: text.len(),
            source: "native".into(),
            ocr_confidence: None,
            images: vec![],
        }
    }

    fn image_on_page(page: u32, hash: &str) -> crate::DocumentImage {
        crate::DocumentImage {
            asset: crate::source::VisualAssetRef {
                asset_id: format!("a-{}", hash),
                mime_type: "image/png".into(),
                content_hash: hash.into(),
                source: crate::source::SourceSpan {
                    document_id: None,
                    page,
                    start_offset: None,
                    end_offset: None,
                    bbox: None,
                    node_id: None,
                },
            },
            bbox: None,
        }
    }

    fn three_page_doc() -> DocumentModel {
        let pages = vec![
            page(
                1,
                "1. Overview\n\nAlpha Systems publishes quarterly reports.",
            ),
            page(2, "1. Financials\n\nRevenue figures from Beta Group."),
            page(3, "1. Outlook\n\nOld Page Three Corp has plans."),
        ];
        DocumentModel {
            page_count: 3,
            total_chars: pages.iter().map(|p| p.char_count).sum(),
            pages,
            ocr_stats: None,
        }
    }

    fn compile_incremental_mock(
        doc: &DocumentModel,
        prev_doc: &DocumentModel,
        prev: &CompilationResult,
    ) -> CompilationResult {
        compile_document_incremental(
            doc,
            prev_doc,
            prev,
            &crate::MockSemanticAnalyzer::new(),
            &MockEntityResolver::new(),
            &MockKnowledgeReconciler::new(),
            &HeadingProjector::new(),
            &MockEmbeddingProvider::new(),
            &[],
            None,
        )
    }

    #[test]
    fn diff_document_models_detects_page_and_image_changes() {
        let prev = three_page_doc();
        let mut next = prev.clone();

        assert!(
            diff_document_models(&prev, &next).is_empty(),
            "identical revisions produce an empty delta"
        );

        next.pages[1]
            .text
            .push_str("\nExtra paragraph on page two.");
        let d = diff_document_models(&prev, &next);
        assert_eq!(d.changed_pages, vec![2]);
        assert!(d.changed_images.is_empty());

        // Image appended on page 1 → Added, page 1 changed.
        let mut with_image = next.clone();
        with_image.pages[0]
            .images
            .push(image_on_page(1, "hash-aaa"));
        let d = diff_document_models(&prev, &with_image);
        assert!(d.changed_pages.contains(&1));
        assert!(d
            .changed_images
            .iter()
            .any(|i| i.content_hash == "hash-aaa" && i.change == AssetChange::Added));

        // Same-slot hash swap → Changed; dropping one → Removed.
        let mut swapped = with_image.clone();
        swapped.pages[0].images[0].asset.content_hash = "hash-bbb".into();
        let d = diff_document_models(&with_image, &swapped);
        assert!(d
            .changed_images
            .iter()
            .any(|i| i.content_hash == "hash-bbb" && i.change == AssetChange::Changed));
        assert!(d.changed_pages.contains(&1));
        let d = diff_document_models(&swapped, &next);
        assert!(d
            .changed_images
            .iter()
            .any(|i| i.content_hash == "hash-bbb" && i.change == AssetChange::Removed));

        // Page removed from next.
        let mut shorter = next.clone();
        shorter.pages.pop();
        shorter.page_count = 2;
        shorter.total_chars = shorter.pages.iter().map(|p| p.char_count).sum();
        let d = diff_document_models(&next, &shorter);
        assert_eq!(d.removed_pages, vec![3]);
        assert!(d.changed_pages.is_empty());
    }

    #[test]
    fn incremental_unchanged_document_returns_previous_result() {
        let doc = three_page_doc();
        let prev = compile_document_mock(&doc, &[]);
        let again = compile_incremental_mock(&doc, &doc, &prev);

        let ids: Vec<&str> = again
            .fragments
            .iter()
            .map(|f| f.fragment_id.as_str())
            .collect();
        let prev_ids: Vec<&str> = prev
            .fragments
            .iter()
            .map(|f| f.fragment_id.as_str())
            .collect();
        assert_eq!(ids, prev_ids);
        assert_eq!(again.ir.entities.len(), prev.ir.entities.len());
        assert_eq!(again.embedded_chunks.len(), prev.embedded_chunks.len());
    }

    #[test]
    fn incremental_single_page_change_splices_fragments_and_ir() {
        let prev_doc = three_page_doc();
        let prev = compile_document_mock(&prev_doc, &[]);

        let mut next_doc = prev_doc.clone();
        next_doc.pages[2] = page(
            3,
            "1. Gamma Outlook\n\nGamma Corp expects growth in Q3 2026.",
        );
        next_doc.total_chars = next_doc.pages.iter().map(|p| p.char_count).sum();

        let result = compile_incremental_mock(&next_doc, &prev_doc, &prev);

        // Kept pages (1, 2) carry their previous fragments unchanged.
        let kept_ids: Vec<&str> = result
            .fragments
            .iter()
            .filter(|f| f.context.page != Some(3))
            .map(|f| f.fragment_id.as_str())
            .collect();
        let prev_kept: Vec<&str> = prev
            .fragments
            .iter()
            .filter(|f| f.context.page != Some(3))
            .map(|f| f.fragment_id.as_str())
            .collect();
        assert_eq!(kept_ids, prev_kept);

        // Fresh entity present, stale changed-page entity gone, kept intact.
        let names: Vec<&str> = result.ir.entities.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"Gamma Corp"), "fresh entity: {:?}", names);
        assert!(names.contains(&"Alpha Systems"));
        assert!(names.contains(&"Beta Group"));
        assert!(!names.contains(&"Old Page Three Corp"));

        // Chunks for kept pages reused, changed page re-projected.
        assert!(result
            .embedded_chunks
            .iter()
            .any(|c| c.chunk.position.start_page == 3));

        // Neighbor links span the splice boundary and stay internally valid.
        let ids: std::collections::HashSet<&str> = result
            .fragments
            .iter()
            .map(|f| f.fragment_id.as_str())
            .collect();
        for f in &result.fragments {
            for n in &f.context.neighboring_fragments {
                assert!(ids.contains(n.as_str()), "dangling neighbor {}", n);
            }
        }
    }

    #[test]
    fn incremental_removed_page_drops_its_candidates() {
        let prev_doc = three_page_doc();
        let prev = compile_document_mock(&prev_doc, &[]);

        let mut next_doc = prev_doc.clone();
        next_doc.pages.pop();
        next_doc.page_count = 2;
        next_doc.total_chars = next_doc.pages.iter().map(|p| p.char_count).sum();

        let result = compile_incremental_mock(&next_doc, &prev_doc, &prev);

        let names: Vec<&str> = result.ir.entities.iter().map(|e| e.name.as_str()).collect();
        assert!(!names.contains(&"Old Page Three Corp"));
        assert!(names.contains(&"Alpha Systems"));
        assert!(result.fragments.iter().all(|f| f.context.page != Some(3)));
        assert!(result
            .embedded_chunks
            .iter()
            .all(|c| c.chunk.position.start_page != 3));
    }

    #[test]
    fn incremental_all_pages_changed_falls_back_to_full_compile() {
        let prev_doc = three_page_doc();
        let prev = compile_document_mock(&prev_doc, &[]);

        let next_doc = DocumentModel {
            page_count: 3,
            pages: vec![
                page(1, "1. New One\n\nGamma Systems here."),
                page(2, "1. New Two\n\nDelta Group here."),
                page(3, "1. New Three\n\nEpsilon Corp here."),
            ],
            total_chars: 0,
            ocr_stats: None,
        };
        let next_doc = DocumentModel {
            total_chars: next_doc.pages.iter().map(|p| p.char_count).sum(),
            ..next_doc
        };

        let result = compile_incremental_mock(&next_doc, &prev_doc, &prev);
        let full = compile_document_mock(&next_doc, &[]);

        // Full path: all seven D3–D8 phases, same entity set as a fresh run.
        assert_eq!(result.stats.phases.len(), 7);
        let names: Vec<&str> = result.ir.entities.iter().map(|e| e.name.as_str()).collect();
        let full_names: Vec<&str> = full.ir.entities.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, full_names);
        assert!(!names.contains(&"Alpha Systems"));
    }

    #[test]
    fn reproject_document_reuses_ir_with_new_embedder() {
        let doc = test_doc();
        let prev = compile_document_mock(&doc, &[]);
        let embedder = MockEmbeddingProvider::with_dimensions(32);

        let result = reproject_document(&prev, &HeadingProjector::new(), &embedder);

        assert_eq!(result.ir.entities.len(), prev.ir.entities.len());
        assert_eq!(result.fragments.len(), prev.fragments.len());
        for c in &result.embedded_chunks {
            assert_eq!(c.embedding.len(), 32);
        }
        assert!(result
            .stats
            .phases
            .iter()
            .any(|p| p.phase == "D8-reproject"));
    }
}
