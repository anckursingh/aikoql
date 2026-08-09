//! Mnemosyne Document Ingestion Plugin SDK — Phase 5 Multi-Modal.
//!
//! Defines the `IngestionPlugin` trait for document-to-KO pipelines.
//! Reference implementations (PDF → OCR → KO) live in separate crates
//! so the kernel stays free of heavy dependencies (poppler, tesseract, etc.).
//!
//! The AIKOQL `INGEST` statement compiles to a workflow that calls these plugins.

use mnemosyne_kernel::knowledge::kom::*;
use mnemosyne_kernel::transaction::kernel::Kernel;

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

mod ast;
pub use ast::{
    classify_blocks_enriched, document_model_to_ast, document_model_to_ast_enriched, AstNode,
    BlockType, BoundingBox, DocumentAst,
};

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
    chunk_and_embed, embed_chunks, ChunkPosition, ChunkStructure, ChunkingStrategy, DocumentChunk,
    DocumentChunker, EmbeddedChunk, MockDocumentChunker,
};

mod pipeline;
pub use pipeline::{
    compile_document, compile_document_mock, CompilationResult, EvidenceNode, EvidenceTrail,
    PhaseStats, PipelineStats,
};

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

/// Extract text from a document file on disk.
/// Returns page-level text for PDFs, single-page for everything else.
pub fn extract_document(file_path: &str, mime_type: &str) -> Result<DocumentModel, String> {
    match mime_type {
        "application/pdf" => extract_pdf(file_path),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            extract_docx(file_path)
        }
        "text/html" => extract_html(file_path),
        t if t.starts_with("text/") => extract_text(file_path),
        _ => Err(format!("unsupported mime type: {}", mime_type)),
    }
}

fn extract_pdf(path: &str) -> Result<DocumentModel, String> {
    let text = pdf_extract::extract_text(path).map_err(|e| format!("pdf extract: {}", e))?;

    // pdf-extract joins pages with formfeed (\u{c}) — split on it.
    // Keep ALL pages (including empty ones) — empty pages may need OCR.
    let native_pages: Vec<PageModel> = text
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
            }
        })
        .collect();

    // D2: For pages with insufficient native text, attempt OCR.
    let work_dir = std::env::temp_dir().join(format!(
        "mnemosyne-ocr-{}",
        std::path::Path::new(path)
            .file_stem()
            .map(|n| n.to_string_lossy())
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

fn extract_docx(path: &str) -> Result<DocumentModel, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open docx: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("zip: {}", e))?;
    let doc = archive
        .by_name("word/document.xml")
        .map_err(|e| format!("docx document.xml: {}", e))?;
    let xml = std::io::read_to_string(doc).map_err(|e| format!("read xml: {}", e))?;
    let text = strip_xml_tags(&xml);

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
        }],
        total_chars: char_count,
        ocr_stats: None,
    })
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
        let dir = std::env::temp_dir().join("mnemosyne-d1-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.txt");
        std::fs::write(&path, "Hello world\n\nThis is a test.\n").unwrap();
        let doc = extract_document(&path.to_string_lossy(), "text/plain").unwrap();
        assert_eq!(doc.page_count, 1);
        assert_eq!(doc.pages[0].text, "Hello world\n\nThis is a test.");
        assert!(doc.total_chars > 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_html_strips_tags() {
        let dir = std::env::temp_dir().join("mnemosyne-d1-html");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.html");
        std::fs::write(
            &path,
            "<html><body><h1>Title</h1><p>Content here.</p></body></html>",
        )
        .unwrap();
        let doc = extract_document(&path.to_string_lossy(), "text/html").unwrap();
        assert_eq!(doc.page_count, 1);
        assert!(doc.pages[0].text.contains("Title"));
        assert!(doc.pages[0].text.contains("Content here"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_unsupported_mime() {
        let result = extract_document("nonexistent.bin", "application/octet-stream");
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

    #[test]
    fn extract_docx_text() {
        let dir = std::env::temp_dir().join("mnemosyne-d1-docx");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.docx");
        write_minimal_docx(&path);
        let doc = extract_document(
            &path.to_string_lossy(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .unwrap();
        assert_eq!(doc.page_count, 1);
        assert!(doc.pages[0].text.contains("Hello DOCX"));
        assert!(doc.total_chars > 0);
        std::fs::remove_dir_all(&dir).ok();
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
            if path.extension().map_or(false, |e| e == "pdf") {
                found = true;
                let doc = extract_document(&path.to_string_lossy(), "application/pdf").unwrap();
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
            if p.extension().map_or(false, |ext| ext == "pdf") {
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

        let work_dir = std::env::temp_dir().join("mnemosyne-real-ocr-test");
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
