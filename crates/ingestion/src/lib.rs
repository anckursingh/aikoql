//! Aikoql Document Ingestion Plugin SDK — Phase 5 Multi-Modal.
#![allow(clippy::new_without_default)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
//!
//! Defines the `IngestionPlugin` trait for document-to-KO pipelines.
//! Reference implementations (PDF → OCR → KO) live in separate crates
//! so the kernel stays free of heavy dependencies (poppler, tesseract, etc.).
//!
//! The aikoql `INGEST` statement compiles to a workflow that calls these plugins.

use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::transaction::kernel::Kernel;

/// Result of ingesting one document.
#[derive(Clone, Debug)]
pub struct IngestionResult {
    /// Knowledge objects extracted from the document.
    pub objects: Vec<KnowledgeObject>,
    /// Relationships discovered between objects.
    pub relationships: Vec<RelationshipRef>,
    /// Warnings (non-fatal issues, e.g. low OCR confidence).
    pub warnings: Vec<String>,
}

/// A plugin that ingests a document and produces knowledge objects.
/// Implementations handle specific formats (PDF, image, HTML, etc.).
pub trait IngestionPlugin: Send + Sync {
    /// Human-readable name (e.g. "pdf-ocr", "html-scraper").
    fn name(&self) -> &str;

    /// MIME types this plugin supports (e.g. ["application/pdf"]).
    fn supported_types(&self) -> &[&str];

    /// Ingest a document from `path` and emit knowledge objects.
    /// The `kernel` reference allows referencing existing KOs (e.g. for
    /// relationship targets or deduplication).
    fn ingest(&self, path: &str, kernel: &Kernel) -> KResult<IngestionResult>;
}

// ---------------------------------------------------------------------------
// Stub implementations for testing
// ---------------------------------------------------------------------------

/// A stub plugin that ingests plain-text files.
/// Each line becomes a KO of type `text_line`.
pub struct TextLineIngester;

impl IngestionPlugin for TextLineIngester {
    fn name(&self) -> &str {
        "text-line"
    }

    fn supported_types(&self) -> &[&str] {
        &["text/plain"]
    }

    fn ingest(&self, path: &str, _kernel: &Kernel) -> KResult<IngestionResult> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| KError::Store(format!("read {}: {}", path, e)))?;
        let mut objects = Vec::new();
        for (i, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let mut props = PropertyMap::new();
            props.insert("text".into(), Value::Text(line.to_string()));
            props.insert("line_number".into(), Value::Int(i as i64 + 1));
            objects.push(KnowledgeObject {
                koid: KOID::ZERO,
                version: 0,
                commit_ts: 0,
                metadata: Metadata {
                    type_name: "text_line".into(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                event_refs: vec![],
                security: SecurityDescriptor {
                    owner: "ingester".into(),
                    acl: vec![],
                    classification: None,
                },
                lifecycle: Lifecycle {
                    state: LifecycleState::Draft,
                    origin: Origin::Human,
                },
                extensions: ExtensionMap::new(),
            });
        }
        Ok(IngestionResult {
            objects,
            relationships: vec![],
            warnings: vec![],
        })
    }
}

// ---------------------------------------------------------------------------
// D1: Native text extraction
// ---------------------------------------------------------------------------

mod ocr;
#[cfg(feature = "vlm")]
pub mod vlm;
// PR-J: transformer boundary scorer (HLD §16 Phase 3). Optional — never in
// the default build (HLD §56/DoD row 10: no mandatory heavyweight AI).
#[cfg(feature = "transform")]
pub mod transform;
pub use ocr::{
    page_needs_ocr, tool_available, BlockBbox, OcrProvider, OcrStats, OcrWord, TesseractCli,
};

mod asset_store;
pub use asset_store::{content_hash, load_asset, mime_from_extension, store_asset};

mod ast;
pub use ast::{
    classify_blocks_enriched, document_model_to_ast, document_model_to_ast_enriched,
    table_payload_from_node, AstNode, AstPayload, Axis, BlockType, BoundingBox, ChartPayload,
    ChartPoint, ChartSeries, ChartType, DetectedObject, DiagramEdge, DiagramNode, DiagramPayload,
    DocumentAst, FormulaPayload, ImagePayload, ScalarValue, TableCell, TableHeader, TablePayload,
    TableRow,
};

// PR-A: typed provenance for multimodal sources.
mod source;
pub use source::{EvidenceSource, SourceSpan, VisualAssetRef};

mod ir;
pub use ir::{
    document_model_to_ir, EntityCandidate, EventCandidate, Evidence, FactCandidate, KnowledgeIr,
    MockSemanticAnalyzer, RelationCandidate, SemanticAnalyzer, TemporalAssertion,
};

mod ontology;
pub use ontology::{
    discover_ontology_from_ir, merge_proposals, ClassProposal, OntologyProposal, PropertyProposal,
    RelationshipProposal,
};

mod embedding;
pub use embedding::{cosine_similarity, EmbeddingProvider, MockEmbeddingProvider};

mod resolution;
pub use resolution::{
    resolve_entities, EntityResolver, KnowledgeBaseEntry, MatchScore, MockEntityResolver,
    ResolutionCandidate, ResolutionResult, ResolutionStats, VectorEntityResolver,
};

mod commit;
pub use commit::{
    reconcile_and_plan, CommitAction, CommitStats, Conflict, ConflictSeverity, KnowledgeCommitPlan,
    KnowledgeReconciler, MockKnowledgeReconciler,
};

mod chunking;
pub use chunking::{
    embed_chunks, fragment_text, project_and_embed, ChunkPosition, ChunkStructure,
    ChunkingStrategy, DocumentChunk, EmbeddedChunk, HeadingProjector, RetrievalProjector,
};

// PR-C: knowledge fragments + semantic boundary detection (D4).
mod fragment;
pub use fragment::{FragmentContent, FragmentContext, FragmentModality, KnowledgeFragment};

mod boundary;
pub use boundary::{
    BoundaryError, BoundaryScore, BoundaryScorer, EmbeddingBoundaryDetector,
    HybridBoundaryDetector, KnowledgeBoundaryDetector, RuleBoundaryDetector,
    TransformerBoundaryDetector,
};

mod visual;
pub use visual::{
    classify_visuals, ChartAnalyzer, DiagramAnalyzer, ImageAnalyzer, MockChartAnalyzer,
    MockDiagramAnalyzer, MockImageAnalyzer, MockVisualClassifier, VisualClassification,
    VisualClassifier, MODEL_CHART, MODEL_DIAGRAM, MODEL_FORMULA, MODEL_IMAGE, MODEL_VISUAL,
};

mod pipeline;
pub use pipeline::{
    compile_document, compile_document_incremental, compile_document_mock,
    compile_document_mock_with_assets, compile_document_mock_with_detector,
    compile_document_with_detector, diff_document_models, reproject_document, AssetChange,
    CompilationResult, DocumentDelta, EvidenceNode, EvidenceTrail, ImageDelta, PhaseStats,
    PipelineStats,
};

// Phase A1: Markdown-to-Knowledge Compiler
mod markdown;
pub use markdown::{
    compile_markdown_file, compile_markdown_string, detect_instruction_injection, is_instruction,
    render_ir_to_markdown, MarkdownSemanticAnalyzer, SectionKind,
};

// Phase A2: Code-to-Knowledge Compiler
mod code;
pub use code::{compile_rust_file, compile_rust_source};

// Phase A3: Multi-source Knowledge Graph merging
mod merge;
pub use merge::{evidence_trail, merge_knowledge_ir};

// Phase A4: Staleness detection
mod staleness;
pub use staleness::{detect_staleness, StalenessWarning};

// Phase A5: Context Compiler
mod context;
pub use context::{
    compile_context, compile_context_cached, compile_context_cached_semantic,
    compile_context_semantic, compile_context_semantic_with, context_cache_stats, expand_entity,
    expand_relationship, expand_source, invalidate_context_cache, render_context_markdown,
    ContextPackage, EntityExpansion, RankedEntity, RankedFact, RankedRelation,
};

mod reconcile;
pub use reconcile::{
    reconcile, stale_entities, AffectedEntity, ImpactSeverity, ReconciliationReport,
};

mod connector_bridge;
pub use connector_bridge::{
    connector_metadata_to_ir, discover_connector_schema, ConnectorMetadata, ContainerInfo,
    FieldInfo, ReferenceInfo,
};

mod secret_filter;
pub use secret_filter::{filter_secrets, SecretFinding, SecretKind};

mod reconciliation_workflow;
pub use reconciliation_workflow::{
    apply_proposal, auto_proposals_from_stale, process_workflow, validate_proposal, ApplyResult,
    ValidatedProposal, ValidationResult, WorkflowReport,
};

mod ingest_dir;
pub use ingest_dir::{
    build_report, collect_file_paths, compile_file, format_report, ingest_directory,
    parallel_ingest_directory, IngestReport, IngestResult, IngestStats,
};

mod ingest_incremental;
pub use ingest_incremental::{incremental_diff_ingest, incremental_ingest_directory, TrackState};

mod aikoql_ops;
pub use aikoql_ops::{
    explain_component, explain_decision, find_conflicts, find_stale_documentation,
    propose_knowledge_update, trace_requirement, validate_change, AffectedKnowledgeInfo,
    ChangeValidation, ComponentExplanation, ConflictReport, ContradictoryClaim,
    DecisionExplanation, KnowledgeProposal, ProposalAction, ProposalStatus, RequirementTrace,
    StaleDocumentationReport, StaleEntityInfo,
};

/// An original visual asset (embedded image) attached to a page of the
/// extracted document. The bytes are content-addressed: `asset.content_hash`
/// is the identity; persistence happens in the extractor when an asset
/// directory is provided.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DocumentImage {
    pub asset: VisualAssetRef,
    /// Page region, where the source format exposes geometry (PDF content
    /// streams are not parsed yet — images carry page placement only).
    #[serde(default)]
    pub bbox: Option<BoundingBox>,
}

/// A single page of extracted text.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PageModel {
    pub page_number: u32,
    pub text: String,
    pub char_count: usize,
    /// How the text was obtained: "native" (pdf-extract) or "ocr" (Tesseract).
    #[serde(default = "default_source")]
    pub source: String,
    /// Average word-level OCR confidence (0.0–100.0), if OCR was used.
    #[serde(default)]
    pub ocr_confidence: Option<f32>,
    /// Embedded images on this page (PR-F image extraction).
    #[serde(default)]
    pub images: Vec<DocumentImage>,
}

fn default_source() -> String {
    "native".into()
}

/// Extracted document content.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DocumentModel {
    pub page_count: u32,
    pub pages: Vec<PageModel>,
    pub total_chars: usize,
    /// OCR statistics, populated when OCR was attempted (even if no pages needed it).
    #[serde(default)]
    pub ocr_stats: Option<OcrStats>,
}

/// Extract content from a document file on disk.
/// Returns page-level text for PDFs, single-page for everything else.
///
/// `asset_dir` enables asset persistence: embedded images (PDF DCTDecode,
/// DOCX media) are content-addressed and stored under `{asset_dir}/{hash}.bin`
/// when provided. Extraction never hard-fails on asset problems — the
/// `VisualAssetRef` still carries the content hash either way.
pub fn extract_document(
    file_path: &str,
    mime_type: &str,
    asset_dir: Option<&str>,
) -> Result<DocumentModel, String> {
    match mime_type {
        "application/pdf" => extract_pdf(file_path, asset_dir),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            extract_docx(file_path, asset_dir)
        }
        "text/html" => extract_html(file_path),
        t if t.starts_with("text/") => extract_text(file_path),
        _ => Err(format!("unsupported mime type: {}", mime_type)),
    }
}

fn extract_pdf(path: &str, asset_dir: Option<&str>) -> Result<DocumentModel, String> {
    // Text extraction is best-effort: an image-only (scanned) PDF produces no
    // text, and unusual encodings can fail — neither should prevent asset
    // extraction. Failure degrades to empty native pages, and the whole call
    // is panic-contained: ingestion never hard-fails on a PDF.
    //
    // Per-page extraction via lopdf::extract_text_chunks. (pdf-extract wraps
    // lopdf but joins pages without a separator — the old formfeed split here
    // was dead code and every multi-page PDF merged into one page. The DoD 19
    // golden fixtures exposed it; root-cause fix is per-page extraction.)
    let mut text_pages: Vec<String> = match std::panic::catch_unwind(|| {
        let doc = lopdf::Document::load(path).map_err(|e| e.to_string())?;
        let page_count = doc.get_pages().len();
        let mut pages = Vec::with_capacity(page_count);
        // extract_text_chunks returns one chunk per font run (Tf), not per
        // page — join each page's chunks ourselves.
        for page_number in 1..=page_count as u32 {
            let mut text = String::new();
            for r in doc.extract_text_chunks(&[page_number]) {
                match r {
                    Ok(t) => text.push_str(&t),
                    Err(e) => {
                        eprintln!(
                            "pdf page text extraction failed: {e} — continuing without native text for that page"
                        );
                    }
                }
            }
            pages.push(text);
        }
        Ok::<_, String>(pages)
    }) {
        Ok(Ok(pages)) => pages,
        Ok(Err(e)) => {
            eprintln!("pdf load failed: {e} — continuing without native text");
            Vec::new()
        }
        Err(_) => {
            eprintln!(
                "pdf text extraction panicked (unsupported font/encoding) — continuing without native text"
            );
            Vec::new()
        }
    };

    // Keep ALL pages (including empty ones) — empty pages may need OCR.
    // A load failure still yields one empty page so image extraction can
    // attach (parity with the old single-string path).
    if text_pages.is_empty() {
        text_pages.push(String::new());
    }
    let mut native_pages: Vec<PageModel> = text_pages
        .into_iter()
        .enumerate()
        .map(|(i, page_text)| {
            let trimmed = page_text.trim().to_string();
            PageModel {
                page_number: i as u32 + 1,
                char_count: trimmed.len(),
                text: trimmed,
                source: "native".into(),
                ocr_confidence: None,
                images: Vec::new(),
            }
        })
        .collect();

    // HLD §29: embedded-image extraction via lopdf. Raster streams (JPEG/JPX/
    // CCITT/Flate) and vector-only content streams all become persisted,
    // content-addressed assets.
    let images_by_page = extract_pdf_images(path, asset_dir);
    for (page_idx, images) in images_by_page.into_iter().enumerate() {
        if let Some(page) = native_pages.get_mut(page_idx) {
            page.images = images;
        }
    }

    // D2: For pages with insufficient native text, attempt OCR.
    let work_dir = std::env::temp_dir().join(format!(
        "aikoql-ocr-{}",
        std::path::Path::new(path)
            .file_stem()
            .map(|n| n.to_string_lossy())
            // justified: path without a file name → empty work-dir suffix
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&work_dir).ok();

    let result = ocr::ocr_pdf_pages(path, &native_pages, &work_dir.to_string_lossy());

    // Clean up work dir (best-effort).
    std::fs::remove_dir_all(&work_dir).ok();

    // If OCR processing fails, fall back to native pages (filter empty).
    match result {
        Ok((mut doc, stats)) => {
            doc.ocr_stats = Some(stats);
            Ok(doc)
        }
        Err(e) => {
            eprintln!("OCR pipeline error: {} — using native text only", e);
            let pages: Vec<PageModel> = native_pages
                .into_iter()
                .filter(|p| !p.text.is_empty())
                .collect();
            let total_chars: usize = pages.iter().map(|p| p.char_count).sum();
            Ok(DocumentModel {
                page_count: pages.len() as u32,
                pages,
                total_chars,
                ocr_stats: None,
            })
        }
    }
}

fn extract_docx(path: &str, asset_dir: Option<&str>) -> Result<DocumentModel, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open docx: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("zip: {}", e))?;
    let doc = archive
        .by_name("word/document.xml")
        .map_err(|e| format!("docx document.xml: {}", e))?;
    let xml = std::io::read_to_string(doc).map_err(|e| format!("read xml: {}", e))?;
    let styles = archive
        .by_name("word/styles.xml")
        .ok()
        .and_then(|s| std::io::read_to_string(s).ok());

    // HLD §30: structured walk of document.xml — paragraphs, runs, headings
    // (pStyle + styles.xml), tables, hyperlinks, drawings, page breaks.
    let (page_texts, image_refs) = parse_docx_structure(&xml, styles.as_deref());

    // Images resolve through the rId → media target relationships, mapped to
    // the page where the drawing appears (page breaks renumber).
    let images_by_page = resolve_docx_images(&mut archive, &image_refs, asset_dir);

    let mut pages: Vec<PageModel> = Vec::new();
    let mut total_chars = 0usize;
    for (idx, text) in page_texts.iter().enumerate() {
        let trimmed = text.trim().to_string();
        total_chars += trimmed.len();
        pages.push(PageModel {
            page_number: idx as u32 + 1,
            char_count: trimmed.len(),
            text: trimmed,
            source: "native".into(),
            ocr_confidence: None,
            images: images_by_page.get(idx).cloned().unwrap_or_default(),
        });
    }
    // Fallback (HLD §30 allows): structured walk produced nothing but the
    // document has content — degrade to the minimal tag strip, one page.
    // Not when images were found: an image-only document has empty text by
    // design, not because parsing failed.
    if total_chars == 0 && !xml.trim().is_empty() && images_by_page.is_empty() {
        let text = strip_xml_tags(&xml);
        let trimmed = text.trim().to_string();
        let char_count = trimmed.len();
        return Ok(DocumentModel {
            page_count: if trimmed.is_empty() { 0 } else { 1 },
            pages: vec![PageModel {
                page_number: 1,
                char_count,
                text: trimmed,
                source: "native".into(),
                ocr_confidence: None,
                images: images_by_page.first().cloned().unwrap_or_default(),
            }],
            total_chars: char_count,
            ocr_stats: None,
        });
    }
    // Drop trailing empty pages (a final page break with nothing after it).
    while pages.len() > 1 && pages.last().map(|p| p.text.is_empty()).unwrap_or(false) {
        pages.pop();
    }
    let page_count = pages.len() as u32;
    Ok(DocumentModel {
        page_count,
        pages,
        total_chars,
        ocr_stats: None,
    })
}

/// One drawing reference: relationship id + the page it appears on.
#[derive(Clone)]
struct DocxImageRef {
    rid: String,
    page: u32,
}

/// HLD §30 structured DOCX walk: page texts + ordered image references.
///
/// Output dialect matches `classify_blocks` in ast.rs: paragraphs separated
/// by blank lines, headings prefixed "# " * level, tables as pipe rows,
/// hyperlinks as "[text](url)". Page breaks (w:br type=page,
/// w:lastRenderedPageBreak) split pages; each page is one text.
fn parse_docx_structure(
    document_xml: &str,
    styles_xml: Option<&str>,
) -> (Vec<String>, Vec<DocxImageRef>) {
    let style_map = styles_xml.map(parse_docx_styles).unwrap_or_default();
    let mut pages: Vec<String> = vec![String::new()];
    let mut images: Vec<DocxImageRef> = Vec::new();

    let body_start = document_xml.find("<w:body").unwrap_or(0);
    let mut rest = &document_xml[body_start..];

    while let Some((kind, offset)) = next_docx_block(rest) {
        rest = &rest[offset..];
        let end = docx_block_end(rest, kind);
        let (segment, remainder) = rest.split_at(end);
        match kind {
            DocxBlock::Paragraph => {
                docx_paragraph(segment, &style_map, &mut pages, &mut images);
            }
            DocxBlock::Table => {
                docx_table(segment, &mut pages);
            }
            DocxBlock::BodyEnd => break,
        }
        rest = remainder;
    }
    (pages, images)
}

#[derive(Clone, Copy, PartialEq)]
enum DocxBlock {
    Paragraph,
    Table,
    BodyEnd,
}

/// Find the next top-level block start: <w:p>, <w:tbl>, or the body end.
fn next_docx_block(xml: &str) -> Option<(DocxBlock, usize)> {
    let mut best: Option<(DocxBlock, usize)> = None;
    for (needle, kind) in [
        ("<w:p>", DocxBlock::Paragraph),
        ("<w:p ", DocxBlock::Paragraph),
        ("<w:tbl>", DocxBlock::Table),
        ("<w:tbl ", DocxBlock::Table),
        ("</w:body>", DocxBlock::BodyEnd),
    ] {
        if let Some(pos) = xml.find(needle) {
            if best.map(|(_, b)| pos < b).unwrap_or(true) {
                best = Some((kind, pos));
            }
        }
    }
    best
}

/// End offset of the block starting at xml[0]: next block start for
/// paragraphs (they never nest), matching </w:tbl> (depth-counted — tables
/// can nest inside cells) for tables.
fn docx_block_end(xml: &str, kind: DocxBlock) -> usize {
    match kind {
        DocxBlock::Paragraph => next_docx_block(&xml[1..])
            .map(|(_, off)| off + 1)
            .unwrap_or(xml.len()),
        DocxBlock::Table => {
            let mut depth = 0usize;
            let mut rest = xml;
            loop {
                let (open, close) = (
                    rest.find("<w:tbl").unwrap_or(usize::MAX),
                    rest.find("</w:tbl>").unwrap_or(usize::MAX),
                );
                if close == usize::MAX {
                    return xml.len();
                }
                if open < close {
                    depth += 1;
                    rest = &rest[open + 6..];
                } else {
                    depth -= 1;
                    if depth == 0 {
                        return rest.as_ptr() as usize - xml.as_ptr() as usize
                            + close
                            + "</w:tbl>".len();
                    }
                    rest = &rest[close + 8..];
                }
            }
        }
        DocxBlock::BodyEnd => xml.len(),
    }
}

/// Paragraph → one block: heading prefix, run text, hyperlinks, images,
/// page breaks. Appends to the current page text with a blank-line
/// separator so `classify_blocks` sees clean paragraph boundaries.
fn docx_paragraph(
    p_xml: &str,
    style_map: &std::collections::HashMap<String, String>,
    pages: &mut Vec<String>,
    images: &mut Vec<DocxImageRef>,
) {
    // A trailing body-level <w:sectPr> lands inside the last paragraph
    // segment; it is metadata, not content.
    let p_xml = match p_xml.find("<w:sectPr") {
        Some(pos) => &p_xml[..pos],
        None => p_xml,
    };
    let pstyle = attr_value_in_tag(p_xml, "w:pStyle");
    let heading = pstyle
        .as_deref()
        .and_then(|s| docx_heading_level(s, style_map));
    let mut buf = String::new();

    let mut rest = p_xml;
    while let Some(tag_pos) = rest.find('<') {
        rest = &rest[tag_pos..];
        if let Some(after) = rest.strip_prefix("<w:t") {
            if after.starts_with('>') || after.starts_with(' ') {
                let content_start = after.find('>').map(|e| e + 1).unwrap_or(0);
                let end = after[content_start..]
                    .find("</w:t>")
                    .map(|e| content_start + e)
                    .unwrap_or(after.len());
                buf.push_str(&xml_unescape(&after[content_start..end]));
                rest = &after[end + "</w:t>".len()..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix("<w:hyperlink") {
            let end = after.find("</w:hyperlink>").unwrap_or(after.len());
            let body = &after[..end];
            let url = attr_value(body, "w:anchor").or_else(|| attr_value(body, "w:history"));
            let label = docx_runs_text(body);
            match url {
                Some(u) if !label.trim().is_empty() => {
                    buf.push_str(&format!("[{}]({})", label.trim(), u))
                }
                _ => buf.push_str(&label),
            }
            rest = &after[end + "</w:hyperlink>".len()..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("<w:br") {
            let tag_end = after.find('>').map(|e| e + 1).unwrap_or(after.len());
            let tag = &after[..tag_end];
            if attr_value(tag, "w:type").as_deref() == Some("page") {
                flush_docx_buf(&mut buf, pages);
                pages.push(String::new());
            } else {
                buf.push('\n');
            }
            rest = &after[tag_end..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("<w:lastRenderedPageBreak") {
            let tag_end = after.find('>').map(|e| e + 1).unwrap_or(after.len());
            flush_docx_buf(&mut buf, pages);
            pages.push(String::new());
            rest = &after[tag_end..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("<w:drawing") {
            let end = after.find("</w:drawing>").unwrap_or(after.len());
            if let Some(rid) = attr_value(&after[..end], "r:embed") {
                images.push(DocxImageRef {
                    rid,
                    page: pages.len() as u32,
                });
            }
            rest = &after[end + "</w:drawing>".len()..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("<w:tab") {
            let tag_end = after.find('>').map(|e| e + 1).unwrap_or(after.len());
            buf.push('\t');
            rest = &after[tag_end..];
            continue;
        }
        // Any other tag (w:r, w:pPr, w:proofErr, …): skip to its close.
        if let Some(close) = rest.find('>') {
            rest = &rest[close + 1..];
        } else {
            break;
        }
    }

    if let Some(level) = heading {
        let text = buf.trim().to_string();
        buf.clear();
        if !text.is_empty() {
            let current = pages.last_mut().expect("at least one page");
            if !current.is_empty() && !current.ends_with("\n\n") {
                current.push('\n');
            }
            current.push_str(&"#".repeat(level as usize));
            current.push(' ');
            current.push_str(&text);
            current.push_str("\n\n");
        }
    } else {
        flush_docx_buf(&mut buf, pages);
    }
}

/// Concatenate w:t run text within a segment (hyperlink bodies, cells).
fn docx_runs_text(segment: &str) -> String {
    let mut out = String::new();
    let mut rest = segment;
    while let Some(pos) = rest.find("<w:t") {
        let after = &rest[pos..];
        if after.starts_with("<w:t>") || after.starts_with("<w:t ") {
            let start = after.find('>').map(|e| e + 1).unwrap_or(0);
            let end = after[start..]
                .find("</w:t>")
                .map(|e| start + e)
                .unwrap_or(after.len());
            out.push_str(&xml_unescape(&after[start..end]));
            rest = &after[end + "</w:t>".len()..];
        } else {
            rest = &after[4..];
        }
    }
    out
}

/// Append paragraph text to the current page (blank-line separated).
fn flush_docx_buf(buf: &mut String, pages: &mut [String]) {
    let text = buf.trim().to_string();
    if !text.is_empty() {
        let current = pages.last_mut().expect("at least one page");
        if !current.is_empty() && !current.ends_with("\n\n") {
            current.push('\n');
        }
        current.push_str(&text);
        current.push_str("\n\n");
    }
    buf.clear();
}

/// Heading level from pStyle: "HeadingN" convention first, then styles.xml
/// name lookup ("heading N", "title").
fn docx_heading_level(
    pstyle: &str,
    style_map: &std::collections::HashMap<String, String>,
) -> Option<u8> {
    if let Some(rest) = pstyle.strip_prefix("Heading") {
        if let Some(n) = rest.chars().next().and_then(|c| c.to_digit(10)) {
            return Some(n.clamp(1, 9) as u8);
        }
    }
    if pstyle.eq_ignore_ascii_case("Title") {
        return Some(1);
    }
    let name = style_map.get(pstyle)?.to_lowercase();
    if name == "title" {
        return Some(1);
    }
    if let Some(rest) = name.strip_prefix("heading ") {
        if let Some(n) = rest.chars().next().and_then(|c| c.to_digit(10)) {
            return Some(n.clamp(1, 9) as u8);
        }
    }
    None
}

/// styles.xml → styleId → style name (paragraph styles only need apply —
/// the caller checks heading names). `w:styleId`/`w:type` are attributes
/// on the `<w:style>` opening tag; `w:name` is a child tag.
fn parse_docx_styles(xml: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let mut rest = xml;
    while let Some(pos) = rest.find("<w:style ") {
        rest = &rest[pos..];
        let tag_end = rest.find('>').map(|e| e + 1).unwrap_or(rest.len());
        let opening = &rest[..tag_end];
        if attr_value(opening, "w:type").as_deref() != Some("paragraph") {
            rest = &rest[tag_end..];
            continue;
        }
        let end = rest.find("</w:style>").unwrap_or(rest.len());
        let segment = &rest[..end];
        if let (Some(id), Some(name)) = (
            attr_value(opening, "w:styleId"),
            attr_value_in_tag(segment, "w:name"),
        ) {
            map.insert(id, name);
        }
        rest = &rest[end + "</w:style>".len()..];
    }
    map
}

/// Table → pipe-markdown rows (the classify_blocks dialect). Header row
/// first; w:gridSpan repeats a cell value to keep column alignment.
fn docx_table(tbl_xml: &str, pages: &mut [String]) {
    let mut lines: Vec<String> = Vec::new();
    let mut rest = tbl_xml;
    while let Some(pos) = rest.find("<w:tr") {
        let after = &rest[pos..];
        if !(after.starts_with("<w:tr>") || after.starts_with("<w:tr ")) {
            rest = &after[5..];
            continue;
        }
        let end = after.find("</w:tr>").unwrap_or(after.len());
        let row = &after[..end];
        let mut cells: Vec<String> = Vec::new();
        let mut cell_rest = row;
        while let Some(cpos) = cell_rest.find("<w:tc") {
            let c_after = &cell_rest[cpos..];
            if !(c_after.starts_with("<w:tc>") || c_after.starts_with("<w:tc ")) {
                cell_rest = &c_after[5..];
                continue;
            }
            let c_end = c_after.find("</w:tc>").unwrap_or(c_after.len());
            let cell = &c_after[..c_end];
            let text = docx_runs_text(cell).trim().to_string();
            let span = attr_value_in_tag(cell, "w:gridSpan")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1);
            cells.push(text);
            for _ in 1..span {
                cells.push(String::new());
            }
            cell_rest = &c_after[c_end + "</w:tc>".len()..];
        }
        if !cells.is_empty() {
            lines.push(format!("| {} |", cells.join(" | ")));
        }
        rest = &after[end + "</w:tr>".len()..];
    }
    if !lines.is_empty() {
        let current = pages.last_mut().expect("at least one page");
        if !current.is_empty() && !current.ends_with("\n\n") {
            current.push('\n');
        }
        current.push_str(&lines.join("\n"));
        current.push_str("\n\n");
    }
}

/// `w:val="…"` on the named tag (e.g. `<w:pStyle w:val="Heading1"/>`).
fn attr_value_in_tag(segment: &str, tag: &str) -> Option<String> {
    let needle = format!("<{} ", tag);
    let pos = segment.find(&needle)?;
    let after = &segment[pos + needle.len()..];
    let end = after.find('>').unwrap_or(after.len());
    attr_value(&after[..end], "w:val")
}

/// Ordered embedded images of a DOCX: rIds resolved through the rels file
/// (rId → media target) with the page each drawing appeared on.
///
/// Fail-soft: missing rels/media entries are skipped, never fatal.
fn resolve_docx_images(
    archive: &mut zip::ZipArchive<std::fs::File>,
    refs: &[DocxImageRef],
    asset_dir: Option<&str>,
) -> Vec<Vec<DocumentImage>> {
    let mut out: Vec<Vec<DocumentImage>> = Vec::new();
    let rels = match archive.by_name("word/_rels/document.xml.rels") {
        Ok(r) => std::io::read_to_string(r).unwrap_or_default(),
        Err(_) => return out,
    };

    // rId → media target (e.g. "media/image1.png"); values are relative to
    // word/. Targets may be absolute ("/word/media/…") or escape with "..".
    let mut targets: Vec<(String, String)> = Vec::new();
    let mut rest = rels.as_str();
    while let Some(start) = rest.find("<Relationship ") {
        rest = &rest[start..];
        let end = rest.find("/>").map(|e| e + 2).unwrap_or(rest.len());
        let tag = &rest[..end];
        let id = attr_value(tag, "Id");
        let target = attr_value(tag, "Target");
        if let (Some(id), Some(target)) = (id, target) {
            targets.push((id, normalize_zip_path(&format!("word/{}", target))));
        }
        rest = &rest[end..];
    }

    for docx_ref in refs {
        let target = targets
            .iter()
            .find(|(id, _)| id == &docx_ref.rid)
            .map(|(_, t)| t.clone());
        let Some(target) = target else { continue };
        let mut entry = match archive.by_name(&target) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let mut bytes = Vec::new();
        if std::io::Read::read_to_end(&mut entry, &mut bytes).is_err() {
            continue;
        }
        let hash = crate::asset_store::content_hash(&bytes);
        if let Some(dir) = asset_dir {
            if let Err(e) = crate::asset_store::store_asset(dir, &bytes) {
                eprintln!("asset store failed for {}: {}", target, e);
            }
        }
        let page_idx = (docx_ref.page - 1) as usize;
        while out.len() <= page_idx {
            out.push(Vec::new());
        }
        out[page_idx].push(DocumentImage {
            asset: VisualAssetRef {
                asset_id: hash.clone(),
                mime_type: crate::asset_store::mime_from_extension(&target),
                content_hash: hash,
                source: SourceSpan {
                    document_id: None,
                    page: docx_ref.page,
                    start_offset: None,
                    end_offset: None,
                    bbox: None,
                    node_id: None,
                },
            },
            bbox: None,
        });
    }
    out
}

/// Extract `attr="value"` from an XML open tag by string scan (the docx
/// rels format is simple; no XML parser dependency).
fn attr_value(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{}=\"", attr);
    let pos = tag.find(&needle)?;
    let after = &tag[pos + needle.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

/// Basic XML entity unescape for run text (WordXML/HTML escape these).
fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// Normalize a zip-internal path: drop a leading "/", resolve ".." segments.
fn normalize_zip_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Embedded images per PDF page (HLD §29 image + vector-graphics extraction).
///
/// Raster XObjects by last filter: DCTDecode = raw JPEG, JPXDecode = raw
/// JPX2000, CCITTFaxDecode = raw G3/G4 bitstream, FlateDecode/LZW (or no
/// filter) = decompressed pixels — wrapped as PPM/PGM when DeviceRGB/Gray at
/// 8 bpc so the asset stays viewable, raw otherwise. Unknown filters keep the
/// raw stream (asset retention over decoding).
///
/// Vector graphics: content streams containing path-drawing operators but no
/// text operators are persisted as vector assets. Streams with text (or that
/// only invoke XObjects) are not vector-only and are skipped.
fn extract_pdf_images(path: &str, asset_dir: Option<&str>) -> Vec<Vec<DocumentImage>> {
    let mut pages: Vec<Vec<DocumentImage>> = Vec::new();
    let doc = match lopdf::Document::load(path) {
        Ok(d) => d,
        Err(_) => return pages,
    };

    // get_pages() is a BTreeMap keyed by page number — iteration order is
    // page order. Images are attached by index to the native pages above.
    for (page_idx, page_id) in doc.get_pages().values().enumerate() {
        let page_num = page_idx as u32 + 1;
        let page = match doc.get_object(*page_id) {
            Ok(lopdf::Object::Dictionary(d)) => d.clone(),
            _ => continue,
        };
        let mut images = Vec::new();
        if let Some(resources) = page
            .get(b"Resources")
            .ok()
            .and_then(|o| o.as_dict().ok())
            .cloned()
        {
            if let Some(xobjects) = resources
                .get(b"XObject")
                .ok()
                .and_then(|o| o.as_dict().ok())
                .cloned()
            {
                for (_name, obj) in xobjects.iter() {
                    let stream = match obj {
                        lopdf::Object::Reference(id) => match doc.get_object(*id) {
                            Ok(lopdf::Object::Stream(s)) => s.clone(),
                            _ => continue,
                        },
                        lopdf::Object::Stream(s) => s.clone(),
                        _ => continue,
                    };
                    let is_image = stream
                        .dict
                        .get(b"Subtype")
                        .ok()
                        .and_then(|o| o.as_name().ok())
                        .map(|n| n == b"Image")
                        .unwrap_or(false);
                    if !is_image {
                        continue;
                    }
                    let (mime, bytes) = pdf_image_asset(&stream);
                    images.push(pdf_asset_image(
                        &bytes,
                        mime,
                        page_num,
                        asset_dir,
                        "pdf image",
                    ));
                }
            }
        }
        // Vector graphics: vector-only content streams (drawing operators,
        // no text operators, no XObject invocations).
        if let Some(contents) = page.get(b"Contents").ok().cloned() {
            let streams: Vec<lopdf::Stream> = match contents {
                lopdf::Object::Reference(id) => match doc.get_object(id) {
                    Ok(lopdf::Object::Stream(s)) => vec![s.clone()],
                    _ => vec![],
                },
                lopdf::Object::Stream(s) => vec![s.clone()],
                lopdf::Object::Array(arr) => arr
                    .iter()
                    .filter_map(|e| match e {
                        lopdf::Object::Reference(id) => doc.get_object(*id).ok(),
                        _ => None,
                    })
                    .filter_map(|o| match o {
                        lopdf::Object::Stream(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => vec![],
            };
            for stream in streams {
                let bytes = stream
                    .get_plain_content()
                    .unwrap_or_else(|_| stream.content.clone());
                if is_vector_only_content(&bytes) {
                    images.push(pdf_asset_image(
                        &bytes,
                        "application/x-pdf-vector",
                        page_num,
                        asset_dir,
                        "pdf vector graphics",
                    ));
                }
            }
        }
        pages.push(images);
    }
    pages
}

/// Decode one image XObject into (mime, bytes) without adding an image-codec
/// dependency. Encoded streams (JPEG/JPX/CCITT) are stored as-is — they are
/// the original asset; pixel streams are wrapped in a PPM/PGM header so the
/// stored asset is a standard format.
fn pdf_image_asset(stream: &lopdf::Stream) -> (&'static str, Vec<u8>) {
    let last_filter = stream.dict.get(b"Filter").ok().and_then(|f| match f {
        lopdf::Object::Name(n) => Some(n.clone()),
        lopdf::Object::Array(arr) => arr.iter().rev().find_map(|e| match e {
            lopdf::Object::Name(n) => Some(n.clone()),
            _ => None,
        }),
        _ => None,
    });
    match last_filter.as_deref() {
        Some(b"DCTDecode") => ("image/jpeg", stream.content.clone()),
        Some(b"JPXDecode") => ("image/jp2", stream.content.clone()),
        Some(b"CCITTFaxDecode") => ("image/x-ccitt", stream.content.clone()),
        Some(b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode") | None => {
            let pixels = stream
                .get_plain_content()
                .unwrap_or_else(|_| stream.content.clone());
            let dict = &stream.dict;
            let rgb = dict
                .get(b"ColorSpace")
                .ok()
                .map(|c| match c {
                    lopdf::Object::Name(n) => n == b"DeviceRGB",
                    lopdf::Object::Array(a) => a
                        .first()
                        .map(|e| match e {
                            lopdf::Object::Name(n) => n == b"DeviceRGB",
                            _ => false,
                        })
                        .unwrap_or(false),
                    _ => false,
                })
                .unwrap_or(false);
            let gray = !rgb
                && dict
                    .get(b"ColorSpace")
                    .ok()
                    .map(|c| match c {
                        lopdf::Object::Name(n) => n == b"DeviceGray",
                        lopdf::Object::Array(a) => a
                            .first()
                            .map(|e| match e {
                                lopdf::Object::Name(n) => n == b"DeviceGray",
                                _ => false,
                            })
                            .unwrap_or(false),
                        _ => false,
                    })
                    .unwrap_or(false);
            let bpc = dict
                .get(b"BitsPerComponent")
                .ok()
                .and_then(|o| o.as_i64().ok())
                .unwrap_or(8);
            let (width, height) = (
                dict.get(b"Width")
                    .ok()
                    .and_then(|o| o.as_i64().ok())
                    .unwrap_or(0),
                dict.get(b"Height")
                    .ok()
                    .and_then(|o| o.as_i64().ok())
                    .unwrap_or(0),
            );
            match (rgb, gray, bpc, width, height) {
                (true, _, 8, w, h) if w > 0 && h > 0 => (
                    "image/x-portable-pixmap",
                    wrap_pnm(&pixels, w as u32, h as u32, true),
                ),
                (_, true, 8, w, h) if w > 0 && h > 0 => (
                    "image/x-portable-graymap",
                    wrap_pnm(&pixels, w as u32, h as u32, false),
                ),
                _ => ("application/octet-stream", pixels),
            }
        }
        _ => ("application/octet-stream", stream.content.clone()),
    }
}

/// Wrap raw 8-bpc pixels in a PPM (P6) or PGM (P5) header — the minimal
/// standard format, no image codec involved.
fn wrap_pnm(pixels: &[u8], width: u32, height: u32, rgb: bool) -> Vec<u8> {
    let magic = if rgb { b"P6\n" } else { b"P5\n" };
    let mut out = Vec::with_capacity(pixels.len() + 32);
    out.extend_from_slice(magic);
    out.extend_from_slice(format!("{} {}\n255\n", width, height).as_bytes());
    out.extend_from_slice(pixels);
    out
}

/// A content stream is vector-only when it draws paths (m/l/c/re) and
/// contains no text operators (BT/Tj/TJ) and no XObject invocations (Do) —
/// otherwise the drawing is decoration on a text page, not an asset.
fn is_vector_only_content(content: &[u8]) -> bool {
    let path_ops: [&[u8]; 4] = [b" m", b" l", b" re", b" c"];
    let has_path_ops = path_ops
        .iter()
        .any(|op| content.windows(op.len()).any(|w| w == *op));
    let has_text = content.windows(2).any(|w| w == b"BT")
        || content.windows(2).any(|w| w == b"Tj")
        || content.windows(2).any(|w| w == b"TJ");
    let has_xobjects = content.windows(3).any(|w| w == b" Do");
    has_path_ops && !has_text && !has_xobjects
}

/// Hash + persist bytes and build the page-level image entry.
fn pdf_asset_image(
    bytes: &[u8],
    mime: &str,
    page_num: u32,
    asset_dir: Option<&str>,
    store_label: &str,
) -> DocumentImage {
    let hash = crate::asset_store::content_hash(bytes);
    if let Some(dir) = asset_dir {
        if let Err(e) = crate::asset_store::store_asset(dir, bytes) {
            eprintln!("asset store failed for {}: {}", store_label, e);
        }
    }
    DocumentImage {
        asset: VisualAssetRef {
            asset_id: hash.clone(),
            mime_type: mime.into(),
            content_hash: hash,
            source: SourceSpan {
                document_id: None,
                page: page_num,
                start_offset: None,
                end_offset: None,
                bbox: None,
                node_id: None,
            },
        },
        bbox: None,
    }
}

fn extract_html(path: &str) -> Result<DocumentModel, String> {
    let html = std::fs::read_to_string(path).map_err(|e| format!("read html: {}", e))?;
    // HLD §31: all source formats converge on the same canonical AST —
    // strip-first extraction is replaced by the structural walk.
    let text = parse_html_structure(&html).trim().to_string();
    let char_count = text.len();
    Ok(DocumentModel {
        page_count: if text.is_empty() { 0 } else { 1 },
        pages: vec![PageModel {
            page_number: 1,
            char_count,
            text,
            source: "native".into(),
            ocr_confidence: None,
            images: Vec::new(),
        }],
        total_chars: char_count,
        ocr_stats: None,
    })
}

/// HLD §31: HTML → the canonical classify_blocks dialect — ATX headings,
/// pipe tables, `- `/`N. ` lists, `[text](url)` links, `![alt](src)`
/// images. script/style/head/title/noscript/template content is skipped
/// entirely. Unknown inline tags (b/em/span/…) are transparent; unknown
/// block tags flow as text. Missing close tags get implied closes when the
/// next sibling opens (loose real-world HTML).
///
/// Note: single-item ordered lists classify as headings downstream (the
/// existing numeric-prefix heuristic) — multi-item lists are the common
/// case and group correctly.
fn parse_html_structure(html: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Block,
        Heading(u8),
        List { ordered: bool, index: usize },
        Li { ordered: bool, index: usize },
        Row,
        Cell,
        Link,
    }
    struct Ctx {
        kind: Kind,
        text: String,
        href: Option<String>,
        cells: Vec<String>,
    }

    /// Collapse whitespace + unescape entities, appending to a flow buffer.
    fn append_flow(target: &mut String, chunk: &str) {
        let collapsed: String = chunk.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            return;
        }
        // Inline punctuation joins without a space ("guide</a>." → "guide].").
        let needs_space = !target.is_empty()
            && !target.ends_with([' ', '\n', '('])
            && !collapsed.starts_with([',', '.', ';', ':', '!', '?', ')', ']', '}']);
        if needs_space {
            target.push(' ');
        }
        target.push_str(&xml_unescape(&collapsed));
    }

    /// Append an already-formatted block to the innermost buffer (verbatim —
    /// no whitespace collapse) or to the output.
    fn emit(stack: &mut [Ctx], out: &mut String, s: &str) {
        if s.is_empty() {
            return;
        }
        if let Some(top) = stack.last_mut() {
            if !top.text.is_empty() && !top.text.ends_with('\n') && !s.starts_with('\n') {
                top.text.push(' ');
            }
            top.text.push_str(s);
        } else {
            out.push_str(s);
        }
    }

    fn close_top(stack: &mut Vec<Ctx>, out: &mut String) {
        let Some(ctx) = stack.pop() else { return };
        let text = ctx.text.trim().to_string();
        match ctx.kind {
            Kind::Block => {
                if !text.is_empty() {
                    emit(stack, out, &format!("{}\n\n", text));
                }
            }
            Kind::Heading(level) => {
                if !text.is_empty() {
                    emit(
                        stack,
                        out,
                        &format!("{} {}\n\n", "#".repeat(level as usize), text),
                    );
                }
            }
            Kind::List { .. } => {
                if !text.is_empty() {
                    emit(stack, out, &format!("{}\n\n", text));
                }
            }
            Kind::Li { ordered, index } => {
                if !text.is_empty() {
                    let prefix = if ordered {
                        format!("{}. ", index)
                    } else {
                        "- ".into()
                    };
                    emit(stack, out, &format!("{}{}\n", prefix, text));
                }
            }
            Kind::Link => {
                let s = match ctx.href {
                    Some(h) if !text.is_empty() => format!("[{}]({})", text, h),
                    _ => text,
                };
                emit(stack, out, &s);
            }
            Kind::Cell => {
                if let Some(row) = stack.last_mut() {
                    if row.kind == Kind::Row {
                        row.cells.push(text);
                        return;
                    }
                }
                emit(stack, out, &text);
            }
            Kind::Row => emit(stack, out, &format!("| {} |\n", ctx.cells.join(" | "))),
        }
    }

    let mut out = String::new();
    let mut stack: Vec<Ctx> = Vec::new();
    let mut rest = html;

    while let Some(pos) = rest.find('<') {
        let before = &rest[..pos];
        if let Some(top) = stack.last_mut() {
            append_flow(&mut top.text, before);
        } else {
            append_flow(&mut out, before);
        }
        rest = &rest[pos..];
        let Some(tag_end) = rest.find('>') else { break };
        let tag = &rest[..tag_end + 1];
        rest = &rest[tag_end + 1..];

        let inner = tag
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim_end_matches('/');
        let (name, closing) = match inner.strip_prefix('/') {
            Some(n) => (n, true),
            None => (inner, false),
        };
        let bare: &str = name
            .trim_start()
            .split(|c: char| c.is_whitespace())
            .next()
            .unwrap_or("");
        let lower = bare.to_ascii_lowercase();

        if !closing {
            match lower.as_str() {
                // Script/style/head content is not text: jump to the close.
                "script" | "style" | "head" | "noscript" | "title" | "template" => {
                    let needle = format!("</{}", lower);
                    let lc = rest.to_ascii_lowercase();
                    if let Some(p) = lc.find(&needle) {
                        let end = rest[p..].find('>').map(|e| p + e + 1).unwrap_or(rest.len());
                        rest = &rest[end..];
                    }
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let level = lower.as_bytes()[1] - b'0';
                    stack.push(Ctx {
                        kind: Kind::Heading(level),
                        text: String::new(),
                        href: None,
                        cells: Vec::new(),
                    });
                }
                "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "aside"
                | "figure" | "figcaption" | "blockquote" | "pre" | "details" | "summary" => {
                    // Implied close of a sibling block/li (loose HTML).
                    if matches!(
                        stack.last().map(|c| c.kind),
                        Some(Kind::Block) | Some(Kind::Li { .. })
                    ) {
                        close_top(&mut stack, &mut out);
                    }
                    stack.push(Ctx {
                        kind: Kind::Block,
                        text: String::new(),
                        href: None,
                        cells: Vec::new(),
                    });
                }
                "ul" | "ol" => {
                    if matches!(
                        stack.last().map(|c| c.kind),
                        Some(Kind::Block) | Some(Kind::Li { .. })
                    ) {
                        close_top(&mut stack, &mut out);
                    }
                    stack.push(Ctx {
                        kind: Kind::List {
                            ordered: lower == "ol",
                            index: 0,
                        },
                        text: String::new(),
                        href: None,
                        cells: Vec::new(),
                    });
                }
                "li" => {
                    if matches!(stack.last().map(|c| c.kind), Some(Kind::Li { .. })) {
                        close_top(&mut stack, &mut out);
                    }
                    // Numbering comes from the innermost enclosing list.
                    let mut ordered = false;
                    let mut index = 0usize;
                    for c in stack.iter_mut().rev() {
                        if let Kind::List {
                            ordered: o,
                            index: i,
                        } = &mut c.kind
                        {
                            *i += 1;
                            ordered = *o;
                            index = *i;
                            break;
                        }
                    }
                    stack.push(Ctx {
                        kind: Kind::Li { ordered, index },
                        text: String::new(),
                        href: None,
                        cells: Vec::new(),
                    });
                }
                "tr" => {
                    if matches!(stack.last().map(|c| c.kind), Some(Kind::Cell)) {
                        close_top(&mut stack, &mut out);
                    }
                    stack.push(Ctx {
                        kind: Kind::Row,
                        text: String::new(),
                        href: None,
                        cells: Vec::new(),
                    });
                }
                "td" | "th" => {
                    if matches!(stack.last().map(|c| c.kind), Some(Kind::Cell)) {
                        close_top(&mut stack, &mut out);
                    }
                    stack.push(Ctx {
                        kind: Kind::Cell,
                        text: String::new(),
                        href: None,
                        cells: Vec::new(),
                    });
                }
                "a" => stack.push(Ctx {
                    kind: Kind::Link,
                    text: String::new(),
                    href: html_attr(tag, "href"),
                    cells: Vec::new(),
                }),
                "br" => {
                    let target = match stack.last_mut() {
                        Some(t) => &mut t.text,
                        None => &mut out,
                    };
                    target.push('\n');
                }
                "hr" => emit(&mut stack, &mut out, "\n\n"),
                "img" => {
                    let alt = html_attr(tag, "alt").unwrap_or_default();
                    let src = html_attr(tag, "src").unwrap_or_default();
                    emit(&mut stack, &mut out, &format!("![{}]({})", alt, src));
                }
                _ => {}
            }
        } else {
            // Closing tag: pop only when it matches the open context.
            let should_close = match lower.as_str() {
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    matches!(stack.last().map(|c| c.kind), Some(Kind::Heading(_)))
                }
                "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "aside"
                | "figure" | "figcaption" | "blockquote" | "pre" | "details" | "summary" => {
                    matches!(stack.last().map(|c| c.kind), Some(Kind::Block))
                }
                "ul" | "ol" => matches!(stack.last().map(|c| c.kind), Some(Kind::List { .. })),
                "li" => matches!(stack.last().map(|c| c.kind), Some(Kind::Li { .. })),
                "tr" => matches!(stack.last().map(|c| c.kind), Some(Kind::Row)),
                "td" | "th" => matches!(stack.last().map(|c| c.kind), Some(Kind::Cell)),
                "a" => matches!(stack.last().map(|c| c.kind), Some(Kind::Link)),
                "table" => {
                    if matches!(stack.last().map(|c| c.kind), Some(Kind::Cell)) {
                        close_top(&mut stack, &mut out);
                    }
                    if matches!(stack.last().map(|c| c.kind), Some(Kind::Row)) {
                        close_top(&mut stack, &mut out);
                    }
                    // Blank line after the table block (classify_blocks
                    // groups pipe rows into one Table node).
                    emit(&mut stack, &mut out, "\n\n");
                    false
                }
                _ => false,
            };
            if should_close {
                close_top(&mut stack, &mut out);
            }
        }
    }

    // Trailing text, then close any unclosed contexts (EOF implied closes).
    if let Some(top) = stack.last_mut() {
        append_flow(&mut top.text, rest);
    } else {
        append_flow(&mut out, rest);
    }
    while !stack.is_empty() {
        close_top(&mut stack, &mut out);
    }
    out.trim().to_string()
}

/// Case-insensitive attribute lookup on an HTML tag (`href="…"`, `alt='…'`).
// ponytail: `to_ascii_lowercase` offsets can shift for non-ASCII attribute
// *values* earlier in the tag (rare); byte-exact for ASCII-only tags.
fn html_attr(tag: &str, attr: &str) -> Option<String> {
    let lc = tag.to_ascii_lowercase();
    let needle = format!("{}=\"", attr.to_ascii_lowercase());
    if let Some(pos) = lc.find(&needle) {
        let rest_t = &tag[pos + needle.len()..];
        return rest_t.find('"').map(|e| xml_unescape(&rest_t[..e]));
    }
    let needle = format!("{}='", attr.to_ascii_lowercase());
    if let Some(pos) = lc.find(&needle) {
        let rest_t = &tag[pos + needle.len()..];
        return rest_t.find('\'').map(|e| xml_unescape(&rest_t[..e]));
    }
    None
}

fn extract_text(path: &str) -> Result<DocumentModel, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read text: {}", e))?;
    let trimmed = content.trim().to_string();
    let char_count = trimmed.len();
    Ok(DocumentModel {
        page_count: if trimmed.is_empty() { 0 } else { 1 },
        pages: vec![PageModel {
            page_number: 1,
            char_count,
            text: trimmed,
            source: "native".into(),
            ocr_confidence: None,
            images: Vec::new(),
        }],
        total_chars: char_count,
        ocr_stats: None,
    })
}

/// Strip XML/HTML tags — simple state machine, no regex dep.
fn strip_xml_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(ch);
        }
    }
    // Collapse whitespace
    let collapsed: Vec<&str> = out.split_whitespace().collect();
    collapsed.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_line_ingester_has_name() {
        let p = TextLineIngester;
        assert_eq!(p.name(), "text-line");
        assert!(p.supported_types().contains(&"text/plain"));
    }

    #[test]
    fn extract_plain_text() {
        let dir = std::env::temp_dir().join("aikoql-d1-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.txt");
        std::fs::write(&path, "Hello world\n\nThis is a test.\n").unwrap();
        let doc = extract_document(&path.to_string_lossy(), "text/plain", None).unwrap();
        assert_eq!(doc.page_count, 1);
        assert_eq!(doc.pages[0].text, "Hello world\n\nThis is a test.");
        assert!(doc.total_chars > 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_html_strips_tags() {
        let dir = std::env::temp_dir().join("aikoql-d1-html");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.html");
        std::fs::write(
            &path,
            "<html><body><h1>Title</h1><p>Content here.</p></body></html>",
        )
        .unwrap();
        let doc = extract_document(&path.to_string_lossy(), "text/html", None).unwrap();
        assert_eq!(doc.page_count, 1);
        assert!(doc.pages[0].text.contains("Title"));
        assert!(doc.pages[0].text.contains("Content here"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_html_preserves_structure() {
        // HLD §31: headings, tables (loose td), lists, links, img+alt,
        // sections — with script/style/head content dropped.
        let dir = std::env::temp_dir().join("aikoql-d1-html-struct");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("struct.html");
        std::fs::write(
            &path,
            r#"<html><head><title>ignored</title><style>.x { }</style></head><body>
<section><h1>Main Title</h1><p>Intro <b>bold</b> &amp; scope.</p></section>
<h2>Metrics</h2>
<table><thead><tr><th>Name</th><th>Value</th></tr></thead>
<tr><td>A<td>B</tr></table>
<ul><li>first</li><li>second</li></ul>
<ol><li>Alpha</li><li>Beta</li></ol>
<p>See the <a href="https://example.com">guide</a>.</p>
<figure><img src="img/logo.png" alt="logo"></figure>
<script>var x = 1 < 2;</script>
</body></html>"#,
        )
        .unwrap();
        let doc = extract_document(&path.to_string_lossy(), "text/html", None).unwrap();
        assert_eq!(doc.page_count, 1);
        let text = &doc.pages[0].text;
        assert!(text.contains("# Main Title"), "h1 → ATX: {}", text);
        assert!(text.contains("## Metrics"), "h2 → ATX");
        assert!(
            text.contains("Intro bold & scope."),
            "inline tags + entities: {}",
            text
        );
        assert!(
            text.contains("| Name | Value |"),
            "thead header row: {}",
            text
        );
        assert!(
            text.contains("| A | B |"),
            "loose td implied close: {}",
            text
        );
        assert!(text.contains("- first\n- second"), "ul list: {}", text);
        assert!(text.contains("1. Alpha\n2. Beta"), "ol list: {}", text);
        assert!(
            text.contains("[guide](https://example.com)"),
            "link: {}",
            text
        );
        assert!(
            text.contains("![logo](img/logo.png)"),
            "img + alt: {}",
            text
        );
        assert!(!text.contains("var x"), "script content dropped");
        assert!(!text.contains("ignored"), "head content dropped");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_unsupported_mime() {
        let result = extract_document("nonexistent.bin", "application/octet-stream", None);
        assert!(result.is_err());
    }

    // PDF extraction is tested via acceptance test with real fixture files —
    // hand-crafted minimal PDFs don't satisfy pdf-extract's font requirements.

    fn write_minimal_docx(path: &std::path::Path) {
        use std::io::Write;
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("[Content_Types].xml", options).unwrap();
        zip.write_all(
            b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>",
        )
        .unwrap();
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>Hello DOCX</w:t></w:r></w:p></w:body></w:document>").unwrap();
        zip.finish().unwrap();
    }

    /// PR-F: docx with one embedded PNG — document.xml draws it via
    /// `r:embed="rId5"`, the rels map rId5 → media/image1.png (HLD §30).
    const DOCX_IMAGE_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n embedded test image";

    fn write_docx_with_image(path: &std::path::Path) {
        use std::io::Write;
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("[Content_Types].xml", options).unwrap();
        zip.write_all(
            b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>",
        )
        .unwrap();
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(
            b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body><w:p><w:r><w:t>Hello DOCX</w:t></w:r></w:p><w:p><w:r><w:drawing><wp:inline><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed=\"rId5\"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:body></w:document>",
        )
        .unwrap();
        zip.start_file("word/_rels/document.xml.rels", options)
            .unwrap();
        zip.write_all(
            b"<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId5\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/image1.png\"/></Relationships>",
        )
        .unwrap();
        zip.start_file("word/media/image1.png", options).unwrap();
        zip.write_all(DOCX_IMAGE_BYTES).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn extract_docx_text() {
        let dir = std::env::temp_dir().join("aikoql-d1-docx");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.docx");
        write_minimal_docx(&path);
        let doc = extract_document(
            &path.to_string_lossy(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            None,
        )
        .unwrap();
        assert_eq!(doc.page_count, 1);
        assert!(doc.pages[0].text.contains("Hello DOCX"));
        assert!(doc.total_chars > 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_docx_extracts_embedded_images_and_persists() {
        let dir = std::env::temp_dir().join("aikoql-d1-docx-image");
        let assets = dir.join("assets");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("img.docx");
        write_docx_with_image(&path);
        let doc = extract_document(
            &path.to_string_lossy(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Some(&assets.to_string_lossy()),
        )
        .unwrap();
        assert_eq!(doc.pages[0].images.len(), 1, "one embedded image");
        let img = &doc.pages[0].images[0];
        assert_eq!(img.asset.mime_type, "image/png");
        assert_eq!(img.asset.content_hash, asset_store_hash(DOCX_IMAGE_BYTES));
        assert!(
            assets
                .join(format!("{}.bin", img.asset.content_hash))
                .exists(),
            "asset persisted content-addressed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// PR-F: structured DOCX fixture — Heading1/Heading2 via pStyle +
    /// styles.xml, gridSpan-merged table cell, hyperlink, caption, page
    /// break, image on page 2 (HLD §30).
    fn write_structured_docx(path: &std::path::Path) {
        use std::io::Write;
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("[Content_Types].xml", options).unwrap();
        zip.write_all(
            b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>",
        )
        .unwrap();
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(
            b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body>\
<w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr><w:r><w:t>Report Title</w:t></w:r></w:p>\
<w:p><w:r><w:t>Intro &amp; scope paragraph.</w:t></w:r></w:p>\
<w:p><w:pPr><w:pStyle w:val=\"Heading2\"/></w:pPr><w:r><w:t>Metrics</w:t></w:r></w:p>\
<w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr><w:p><w:r><w:t>Merged header</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc></w:tr></w:tbl>\
<w:p><w:r><w:t>See the </w:t></w:r><w:hyperlink w:anchor=\"https://example.com\"><w:r><w:t>guide</w:t></w:r></w:hyperlink><w:r><w:t>.</w:t></w:r></w:p>\
<w:p><w:pPr><w:pStyle w:val=\"Caption\"/></w:pPr><w:r><w:t>Figure 1: overview</w:t></w:r></w:p>\
<w:p><w:r><w:br w:type=\"page\"/></w:r><w:r><w:t>Page two text.</w:t></w:r></w:p>\
<w:p><w:r><w:drawing><wp:inline><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed=\"rId5\"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>\
</w:body></w:document>",
        )
        .unwrap();
        zip.start_file("word/styles.xml", options).unwrap();
        zip.write_all(
            b"<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading2\"><w:name w:val=\"heading 2\"/></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Caption\"><w:name w:val=\"caption\"/></w:style>\
</w:styles>",
        )
        .unwrap();
        zip.start_file("word/_rels/document.xml.rels", options)
            .unwrap();
        zip.write_all(
            b"<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId5\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/image1.png\"/></Relationships>",
        )
        .unwrap();
        zip.start_file("word/media/image1.png", options).unwrap();
        zip.write_all(DOCX_IMAGE_BYTES).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn extract_docx_preserves_structure_headings_tables_links() {
        let dir = std::env::temp_dir().join("aikoql-d1-docx-struct");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("struct.docx");
        write_structured_docx(&path);
        let doc = extract_document(
            &path.to_string_lossy(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            None,
        )
        .unwrap();
        assert_eq!(doc.page_count, 2, "page break splits pages");
        let p1 = &doc.pages[0].text;
        assert!(
            p1.contains("# Report Title"),
            "Heading1 → ATX level 1: {}",
            p1
        );
        assert!(p1.contains("## Metrics"), "Heading2 → ATX level 2");
        assert!(
            p1.contains("Intro & scope paragraph."),
            "XML entities unescaped"
        );
        assert!(
            p1.contains("| Merged header |  |"),
            "gridSpan cell padded: {}",
            p1
        );
        assert!(p1.contains("| A | B |"), "table rows: {}", p1);
        assert!(
            p1.contains("[guide](https://example.com)"),
            "hyperlink: {}",
            p1
        );
        assert!(p1.contains("Figure 1: overview"), "caption paragraph");
        assert!(doc.pages[0].images.is_empty(), "no image on page 1");
        assert!(doc.pages[1].text.contains("Page two text."));
        assert_eq!(doc.pages[1].images.len(), 1, "image lands on page 2");
        assert_eq!(doc.pages[1].images[0].asset.mime_type, "image/png");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_docx_falls_back_to_strip_xml_tags() {
        // No w:p/w:tbl structure (altChunk content): the structured walk
        // yields nothing and the minimal tag strip takes over (HLD §30
        // fallback).
        let dir = std::env::temp_dir().join("aikoql-d1-docx-fallback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fallback.docx");
        use std::io::Write;
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("[Content_Types].xml", options).unwrap();
        zip.write_all(
            b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>",
        )
        .unwrap();
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(
            b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:altChunk><raw>Raw text content</raw></w:altChunk></w:body></w:document>",
        )
        .unwrap();
        zip.finish().unwrap();
        let doc = extract_document(
            &path.to_string_lossy(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            None,
        )
        .unwrap();
        assert_eq!(doc.page_count, 1);
        assert!(doc.pages[0].text.contains("Raw text content"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_pdf_extracts_dctdecode_images_and_persists() {
        // Minimal hand-built PDF: one page with one DCTDecode image XObject.
        // pdf-extract fails soft on it (no fonts) — text stays empty, the
        // image extraction still runs.
        let jpeg = b"\xff\xd8\xff\xe0fakejpeg\xff\xd9";
        let mut doc = lopdf::Document::with_version("1.4");
        let img_dict = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"XObject".to_vec())),
            ("Subtype", lopdf::Object::Name(b"Image".to_vec())),
            ("Width", lopdf::Object::Integer(1)),
            ("Height", lopdf::Object::Integer(1)),
            ("ColorSpace", lopdf::Object::Name(b"DeviceRGB".to_vec())),
            ("BitsPerComponent", lopdf::Object::Integer(8)),
            ("Filter", lopdf::Object::Name(b"DCTDecode".to_vec())),
        ]);
        let img_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            img_dict,
            jpeg.to_vec(),
        )));
        let xobj = lopdf::Dictionary::from_iter([("Im1", lopdf::Object::Reference(img_id))]);
        let resources =
            lopdf::Dictionary::from_iter([("XObject", lopdf::Object::Dictionary(xobj))]);
        let contents_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            Vec::new(),
        )));
        let page_dict = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Page".to_vec())),
            (
                "MediaBox",
                lopdf::Object::Array(vec![
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(100),
                    lopdf::Object::Integer(100),
                ]),
            ),
            ("Resources", lopdf::Object::Dictionary(resources)),
            ("Contents", lopdf::Object::Reference(contents_id)),
        ]);
        let page_id = doc.add_object(lopdf::Object::Dictionary(page_dict));
        let pages_dict = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Pages".to_vec())),
            (
                "Kids",
                lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
            ),
            ("Count", lopdf::Object::Integer(1)),
        ]);
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages_dict));
        let catalog = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Catalog".to_vec())),
            ("Pages", lopdf::Object::Reference(pages_id)),
        ]);
        let catalog_id = doc.add_object(lopdf::Object::Dictionary(catalog));
        doc.trailer
            .set("Root", lopdf::Object::Reference(catalog_id));

        let dir = std::env::temp_dir().join("aikoql-d1-pdf-image");
        let assets = dir.join("assets");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("one-image.pdf");
        doc.save(&path).unwrap();

        let doc = extract_document(
            &path.to_string_lossy(),
            "application/pdf",
            Some(&assets.to_string_lossy()),
        )
        .expect("extract succeeds (text leg fails soft)");
        assert_eq!(doc.page_count, 1);
        assert_eq!(doc.pages[0].images.len(), 1, "one DCTDecode image");
        let img = &doc.pages[0].images[0];
        assert_eq!(img.asset.mime_type, "image/jpeg");
        assert_eq!(img.asset.content_hash, asset_store_hash(jpeg));
        assert!(assets
            .join(format!("{}.bin", img.asset.content_hash))
            .exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Minimal one-page PDF: given image XObjects (name, dict, content) +
    /// content stream bytes. All objects live in the one document.
    fn build_minimal_pdf(
        dir: &std::path::Path,
        name: &str,
        images: Vec<(&str, lopdf::Dictionary, Vec<u8>)>,
        content_streams: Vec<Vec<u8>>,
    ) -> std::path::PathBuf {
        let mut doc = lopdf::Document::with_version("1.4");
        let xobjects: Vec<(&str, lopdf::Object)> = images
            .iter()
            .map(|(name, dict, content)| {
                let id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
                    dict.clone(),
                    content.clone(),
                )));
                (*name, lopdf::Object::Reference(id))
            })
            .collect();
        let xobj_dict = lopdf::Dictionary::from_iter(xobjects);
        let resources =
            lopdf::Dictionary::from_iter([("XObject", lopdf::Object::Dictionary(xobj_dict))]);
        let contents: Vec<lopdf::Object> = content_streams
            .iter()
            .map(|c| {
                let id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
                    lopdf::Dictionary::new(),
                    c.clone(),
                )));
                lopdf::Object::Reference(id)
            })
            .collect();
        let contents_obj = if contents.len() == 1 {
            contents[0].clone()
        } else {
            lopdf::Object::Array(contents)
        };
        let page_dict = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Page".to_vec())),
            (
                "MediaBox",
                lopdf::Object::Array(vec![
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(100),
                    lopdf::Object::Integer(100),
                ]),
            ),
            ("Resources", lopdf::Object::Dictionary(resources)),
            ("Contents", contents_obj),
        ]);
        let page_id = doc.add_object(lopdf::Object::Dictionary(page_dict));
        let pages_dict = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Pages".to_vec())),
            (
                "Kids",
                lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
            ),
            ("Count", lopdf::Object::Integer(1)),
        ]);
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages_dict));
        let catalog = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Catalog".to_vec())),
            ("Pages", lopdf::Object::Reference(pages_id)),
        ]);
        let catalog_id = doc.add_object(lopdf::Object::Dictionary(catalog));
        doc.trailer
            .set("Root", lopdf::Object::Reference(catalog_id));
        let path = dir.join(name);
        doc.save(&path).unwrap();
        path
    }

    #[test]
    fn extract_pdf_flate_image_wraps_pixels_as_pgm() {
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        // FlateDecode 1x2 grayscale pixels — decompressed and wrapped as PGM.
        let gray: Vec<u8> = vec![0x00, 0xFF];
        let mut enc = ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&gray).unwrap();
        let compressed = enc.finish().unwrap();

        let img_dict = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"XObject".to_vec())),
            ("Subtype", lopdf::Object::Name(b"Image".to_vec())),
            ("Width", lopdf::Object::Integer(1)),
            ("Height", lopdf::Object::Integer(2)),
            ("ColorSpace", lopdf::Object::Name(b"DeviceGray".to_vec())),
            ("BitsPerComponent", lopdf::Object::Integer(8)),
            ("Filter", lopdf::Object::Name(b"FlateDecode".to_vec())),
        ]);
        let dir = std::env::temp_dir().join("aikoql-d1-pdf-flate");
        let assets = dir.join("assets");
        std::fs::create_dir_all(&dir).unwrap();
        let path = build_minimal_pdf(
            &dir,
            "flate.pdf",
            vec![("Im1", img_dict, compressed.clone())],
            vec![],
        );

        let doc = extract_document(
            &path.to_string_lossy(),
            "application/pdf",
            Some(&assets.to_string_lossy()),
        )
        .expect("extract succeeds");
        assert_eq!(doc.pages[0].images.len(), 1, "one flate image");
        let img = &doc.pages[0].images[0];
        assert_eq!(img.asset.mime_type, "image/x-portable-graymap");
        // P5 header + the two gray pixels.
        let expected = "P5\n1 2\n255\n".to_string().into_bytes();
        let expected: Vec<u8> = expected.into_iter().chain(gray).collect();
        assert_eq!(img.asset.content_hash, asset_store_hash(&expected));
        assert!(assets
            .join(format!("{}.bin", img.asset.content_hash))
            .exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_pdf_jpx_and_ccitt_stored_raw() {
        let jpx = b"\x00\x00\x00\x0c\x6a\x50\x20\x20fake-jpx";
        let jpx_dict = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"XObject".to_vec())),
            ("Subtype", lopdf::Object::Name(b"Image".to_vec())),
            ("Width", lopdf::Object::Integer(1)),
            ("Height", lopdf::Object::Integer(1)),
            ("Filter", lopdf::Object::Name(b"JPXDecode".to_vec())),
        ]);
        let ccitt = b"\x00\x10\xfa\x00fake-g4";
        let ccitt_dict = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"XObject".to_vec())),
            ("Subtype", lopdf::Object::Name(b"Image".to_vec())),
            ("Width", lopdf::Object::Integer(1)),
            ("Height", lopdf::Object::Integer(1)),
            ("Filter", lopdf::Object::Name(b"CCITTFaxDecode".to_vec())),
        ]);
        let dir = std::env::temp_dir().join("aikoql-d1-pdf-jpx");
        std::fs::create_dir_all(&dir).unwrap();
        let path = build_minimal_pdf(
            &dir,
            "jpx-ccitt.pdf",
            vec![
                ("Im1", jpx_dict, jpx.to_vec()),
                ("Im2", ccitt_dict, ccitt.to_vec()),
            ],
            vec![],
        );

        let doc = extract_document(&path.to_string_lossy(), "application/pdf", None)
            .expect("extract succeeds");
        let by_mime: Vec<(&str, &str)> = doc.pages[0]
            .images
            .iter()
            .map(|i| (i.asset.mime_type.as_str(), i.asset.content_hash.as_str()))
            .collect();
        assert_eq!(by_mime.len(), 2);
        assert!(by_mime.contains(&("image/jp2", asset_store_hash(jpx).as_str())));
        assert!(by_mime.contains(&("image/x-ccitt", asset_store_hash(ccitt).as_str())));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_pdf_vector_only_content_streams_become_assets() {
        let vector = b"0 0 m\n100 100 l\nS\n".to_vec();
        let text = b"BT /F1 12 Tf (hello) Tj ET\n".to_vec();
        let dir = std::env::temp_dir().join("aikoql-d1-pdf-vector");
        let assets = dir.join("assets");
        std::fs::create_dir_all(&dir).unwrap();
        let path = build_minimal_pdf(&dir, "vector.pdf", vec![], vec![vector.clone(), text]);

        let doc = extract_document(
            &path.to_string_lossy(),
            "application/pdf",
            Some(&assets.to_string_lossy()),
        )
        .expect("extract succeeds");
        let vectors: Vec<&DocumentImage> = doc.pages[0]
            .images
            .iter()
            .filter(|i| i.asset.mime_type == "application/x-pdf-vector")
            .collect();
        assert_eq!(vectors.len(), 1, "only the vector-only stream qualifies");
        assert_eq!(vectors[0].asset.content_hash, asset_store_hash(&vector));
        assert!(assets
            .join(format!("{}.bin", vectors[0].asset.content_hash))
            .exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_pdf_text_mixed_content_is_not_vector_only() {
        // A page that draws shapes AND writes text: the content stream is not
        // vector-only, so it must not become a vector asset.
        let mixed = b"BT /F1 12 Tf (x) Tj ET\n0 0 m 100 100 l S\n".to_vec();
        let dir = std::env::temp_dir().join("aikoql-d1-pdf-mixed");
        std::fs::create_dir_all(&dir).unwrap();
        let path = build_minimal_pdf(&dir, "mixed.pdf", vec![], vec![mixed]);

        let doc = extract_document(&path.to_string_lossy(), "application/pdf", None)
            .expect("extract succeeds");
        assert!(
            doc.pages[0].images.is_empty(),
            "text+vector content stream is decoration, not an asset"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn asset_store_hash(bytes: &[u8]) -> String {
        crate::asset_store::content_hash(bytes)
    }

    // -- Real-world invoice tests (requires files at known path) --

    const INVOICE_DIR: &str =
        "C:/Users/ancku/CascadeProjects/ai-crm-platform/services/billing-processor/output";

    #[test]
    fn real_invoices_extract_native_text() {
        let dir = std::path::Path::new(INVOICE_DIR);
        if !dir.exists() {
            eprintln!("Skipping: invoice dir not found at {}", INVOICE_DIR);
            return;
        }

        let mut found = false;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "pdf") {
                found = true;
                let doc =
                    extract_document(&path.to_string_lossy(), "application/pdf", None).unwrap();
                let fname = path.file_name().unwrap().to_string_lossy();

                // All invoices should have native text (they're text-based PDFs).
                assert!(doc.page_count > 0, "{}: expected at least 1 page", fname);
                assert!(
                    doc.total_chars > 100,
                    "{}: expected substantial text, got {} chars",
                    fname,
                    doc.total_chars
                );

                // All pages should be native (not OCR'd) since these have native text.
                for page in &doc.pages {
                    assert_eq!(
                        page.source, "native",
                        "{} page {}: expected native source",
                        fname, page.page_number
                    );
                    assert!(
                        page.ocr_confidence.is_none(),
                        "{} page {}: expected no OCR confidence for native page",
                        fname,
                        page.page_number
                    );
                }

                // Verify OCR stats: no OCR attempted because native text is sufficient.
                if let Some(ref stats) = doc.ocr_stats {
                    assert_eq!(
                        stats.pages_ocr_attempted, 0,
                        "{}: expected 0 OCR attempts for text-based PDF",
                        fname
                    );
                    assert_eq!(stats.status(), "extracted");
                }

                eprintln!(
                    "{}: {} pages, {} chars, status=extracted",
                    fname, doc.page_count, doc.total_chars
                );
            }
        }
        assert!(found, "expected at least one PDF in invoice dir");
    }

    #[test]
    fn real_invoices_ocr_on_rasterized_page() {
        // Verify OCR produces meaningful text from a rasterized invoice page.
        let dir = std::path::Path::new(INVOICE_DIR);
        if !dir.exists() {
            eprintln!("Skipping: invoice dir not found at {}", INVOICE_DIR);
            return;
        }

        // Find first PDF.
        let pdf = std::fs::read_dir(dir).unwrap().find_map(|e| {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|ext| ext == "pdf") {
                Some(p)
            } else {
                None
            }
        });
        let pdf = match pdf {
            Some(p) => p,
            None => {
                eprintln!("Skipping: no PDF found in invoice dir");
                return;
            }
        };

        let provider = TesseractCli::new();
        if !provider.available() {
            eprintln!("Skipping: Tesseract or pdftoppm not available");
            return;
        }

        let work_dir = std::env::temp_dir().join("aikoql-real-ocr-test");
        std::fs::create_dir_all(&work_dir).unwrap();

        // Rasterize page 1 and OCR it with confidence.
        let png = ocr::rasterize_pdf_page(&pdf.to_string_lossy(), 1, &work_dir.to_string_lossy())
            .unwrap();

        let result = provider
            .recognize(&png, "eng", &work_dir.to_string_lossy())
            .unwrap();

        eprintln!(
            "OCR result: {} chars, {} words, avg confidence {:.1}%",
            result.text.len(),
            result.word_count,
            result.confidence
        );

        // OCR should produce substantial text.
        assert!(
            result.text.len() > 100,
            "OCR should extract >100 chars from invoice, got {}",
            result.text.len()
        );
        assert!(result.word_count > 10, "should recognize >10 words");
        assert!(
            result.confidence > 0.0,
            "should have positive average confidence"
        );

        // Verify key invoice fields are present in OCR output.
        assert!(
            result.text.contains("TAX INVOICE") || result.text.contains("INVOICE"),
            "OCR should find INVOICE text"
        );
        assert!(
            result.text.contains("GSTIN") || result.text.contains("GST"),
            "OCR should find GSTIN"
        );

        std::fs::remove_dir_all(&work_dir).ok();
    }
}
