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
    embed_chunks, project_and_embed, ChunkPosition, ChunkStructure, ChunkingStrategy,
    DocumentChunk, EmbeddedChunk, HeadingProjector, RetrievalProjector,
};

// PR-C: knowledge fragments + semantic boundary detection (D4).
mod fragment;
pub use fragment::{FragmentContent, FragmentContext, FragmentModality, KnowledgeFragment};

mod boundary;
pub use boundary::{BoundaryError, KnowledgeBoundaryDetector, RuleBoundaryDetector};

mod visual;
pub use visual::{
    classify_visuals, ChartAnalyzer, DiagramAnalyzer, ImageAnalyzer, MockChartAnalyzer,
    MockDiagramAnalyzer, MockImageAnalyzer, MockVisualClassifier, VisualClassification,
    VisualClassifier, MODEL_CHART, MODEL_DIAGRAM, MODEL_FORMULA, MODEL_IMAGE, MODEL_VISUAL,
};

mod pipeline;
pub use pipeline::{
    compile_document, compile_document_mock, CompilationResult, EvidenceNode, EvidenceTrail,
    PhaseStats, PipelineStats,
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
    // text, and pdf-extract can reject unusual encodings — neither should
    // prevent asset extraction. Failure degrades to empty native pages.
    let text = match pdf_extract::extract_text(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "pdf text extraction failed: {} — continuing without native text",
                e
            );
            String::new()
        }
    };

    // pdf-extract joins pages with formfeed (\u{c}) — split on it.
    // Keep ALL pages (including empty ones) — empty pages may need OCR.
    let mut native_pages: Vec<PageModel> = text
        .split('\u{c}')
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

    // PR-F: embedded-image extraction via lopdf (already in the graph through
    // pdf-extract — no new dependency). DCTDecode streams are raw JPEG bytes,
    // the dominant case for photos/charts in real PDFs. FlateDecode raw-pixel
    // images and vector graphics stay deferred (see IMPLEMENTATION-PLAN.md).
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
    let text = strip_xml_tags(&xml);

    // PR-F: images from word/media/*, in document order via the rId → target
    // relationships of word/_rels/document.xml.rels (HLD §30 relationships +
    // media). Images never referenced by document.xml (headers, leftovers)
    // are not attached.
    let images = extract_docx_images(&mut archive, &xml, asset_dir);

    let trimmed = text.trim().to_string();
    let char_count = trimmed.len();
    Ok(DocumentModel {
        page_count: if trimmed.is_empty() { 0 } else { 1 },
        pages: vec![PageModel {
            page_number: 1,
            char_count,
            text: trimmed,
            source: "native".into(),
            ocr_confidence: None,
            images,
        }],
        total_chars: char_count,
        ocr_stats: None,
    })
}

/// Ordered embedded images of a DOCX, per document.xml drawing references.
///
/// The rels file maps relationship ids to media targets; document.xml embeds
/// images as `<a:blip r:embed="rIdN"/>`. Order = order of `r:embed`
/// appearances (deduplicated). Fail-soft: missing rels/media entries are
/// skipped, never fatal.
fn extract_docx_images(
    archive: &mut zip::ZipArchive<std::fs::File>,
    document_xml: &str,
    asset_dir: Option<&str>,
) -> Vec<DocumentImage> {
    let mut out = Vec::new();
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

    // r:embed appearances in document order.
    let mut seen: Vec<&str> = Vec::new();
    let mut scan = document_xml;
    while let Some(pos) = scan.find("r:embed=\"") {
        let after = &scan[pos + "r:embed=\"".len()..];
        let end = after.find('"').unwrap_or(after.len());
        let rid = &after[..end];
        if !seen.contains(&rid) {
            seen.push(rid);
        }
        scan = &after[end..];
    }

    for rid in seen {
        let target = targets
            .iter()
            .find(|(id, _)| id == rid)
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
        out.push(DocumentImage {
            asset: VisualAssetRef {
                asset_id: hash.clone(),
                mime_type: crate::asset_store::mime_from_extension(&target),
                content_hash: hash,
                source: SourceSpan {
                    document_id: None,
                    page: 1,
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

/// Embedded images per PDF page: DCTDecode XObjects are raw JPEG bytes.
/// FlateDecode/JPX2000/vector graphics are not decoded here (see
/// IMPLEMENTATION-PLAN.md — pixel encoding needs an image codec).
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
        let resources = match page
            .get(b"Resources")
            .ok()
            .and_then(|o| o.as_dict().ok())
            .cloned()
        {
            Some(d) => d,
            None => continue,
        };
        let xobjects = match resources
            .get(b"XObject")
            .ok()
            .and_then(|o| o.as_dict().ok())
            .cloned()
        {
            Some(d) => d,
            None => continue,
        };
        let mut images = Vec::new();
        for (_name, obj) in xobjects.iter() {
            let stream = match obj {
                lopdf::Object::Reference(id) => match doc.get_object(*id) {
                    Ok(lopdf::Object::Stream(s)) => s.clone(),
                    _ => continue,
                },
                lopdf::Object::Stream(s) => s.clone(),
                _ => continue,
            };
            let dict = stream.dict;
            let is_image = dict
                .get(b"Subtype")
                .ok()
                .and_then(|o| o.as_name().ok())
                .map(|n| n == b"Image")
                .unwrap_or(false);
            if !is_image {
                continue;
            }
            // DCTDecode streams are complete JPEG bytes — store as-is.
            let is_jpeg = dict
                .get(b"Filter")
                .ok()
                .map(|f| match f {
                    lopdf::Object::Name(n) => n == b"DCTDecode",
                    lopdf::Object::Array(arr) => arr.iter().any(|e| match e {
                        lopdf::Object::Name(n) => n == b"DCTDecode",
                        _ => false,
                    }),
                    _ => false,
                })
                .unwrap_or(false);
            if !is_jpeg {
                continue;
            }
            let bytes = stream.content.clone();
            let hash = crate::asset_store::content_hash(&bytes);
            if let Some(dir) = asset_dir {
                if let Err(e) = crate::asset_store::store_asset(dir, &bytes) {
                    eprintln!("asset store failed for pdf image: {}", e);
                }
            }
            images.push(DocumentImage {
                asset: VisualAssetRef {
                    asset_id: hash.clone(),
                    mime_type: "image/jpeg".into(),
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
            });
        }
        pages.push(images);
    }
    pages
}

fn extract_html(path: &str) -> Result<DocumentModel, String> {
    let html = std::fs::read_to_string(path).map_err(|e| format!("read html: {}", e))?;
    let text = strip_xml_tags(&html);
    let trimmed = text.trim().to_string();
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
