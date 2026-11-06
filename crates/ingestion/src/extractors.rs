//! HLD §27: the extractor registry. Each source format is a
//! `DocumentExtractor`; `ExtractorRegistry` picks the first that supports a
//! mime type. `extract_document` is a thin wrapper over the default
//! registry, so its callers are unchanged.

use crate::DocumentModel;

/// What an extractor reads: a file on disk, its mime type, and where
/// content-addressed assets (images) are persisted, if anywhere.
#[derive(Clone, Copy, Debug)]
pub struct DocumentSource<'a> {
    pub path: &'a str,
    pub mime_type: &'a str,
    pub asset_dir: Option<&'a str>,
    /// HLD §54/§55 limits and enable flags the extraction runs under.
    pub policy: &'a crate::ProcessingPolicy,
}

/// Physical extraction for one family of formats (HLD §27/§28: no model,
/// no semantic interpretation — that is the analyzer's job).
pub trait DocumentExtractor: Send + Sync {
    fn name(&self) -> &str;
    fn supports(&self, mime_type: &str) -> bool;
    fn extract(&self, source: &DocumentSource) -> Result<DocumentModel, String>;
}

/// Ordered extractors; the first whose `supports` matches wins.
pub struct ExtractorRegistry {
    extractors: Vec<Box<dyn DocumentExtractor>>,
}

impl ExtractorRegistry {
    pub fn new() -> Self {
        ExtractorRegistry {
            extractors: Vec::new(),
        }
    }

    /// The built-in extractors. Order matters: CSV/TSV and HTML before the
    /// `text/*` catch-all.
    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        r.extractors = vec![
            Box::new(PdfExtractor),
            Box::new(DocxExtractor),
            Box::new(crate::office::XlsxExtractor),
            Box::new(crate::office::PptxExtractor),
            Box::new(HtmlExtractor),
            Box::new(crate::office::CsvExtractor),
            Box::new(TextExtractor),
            Box::new(ImageExtractor),
        ];
        r
    }

    /// Add an extractor ahead of the existing ones (it can override a
    /// built-in for the mime types it supports).
    pub fn register(&mut self, extractor: Box<dyn DocumentExtractor>) {
        self.extractors.insert(0, extractor);
    }

    pub fn extractor_for(&self, mime_type: &str) -> Option<&dyn DocumentExtractor> {
        self.extractors
            .iter()
            .find(|e| e.supports(mime_type))
            .map(|e| e.as_ref())
    }

    pub fn extract(&self, source: &DocumentSource) -> Result<DocumentModel, String> {
        let doc = match self.extractor_for(source.mime_type) {
            Some(e) => e.extract(source)?,
            None => return Err(format!("unsupported mime type: {}", source.mime_type)),
        };
        // Every format meets the document-level limits (pages, images).
        source.policy.check_document(&doc)?;
        Ok(doc)
    }
}

impl Default for ExtractorRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

struct PdfExtractor;
impl DocumentExtractor for PdfExtractor {
    fn name(&self) -> &str {
        "pdf"
    }
    fn supports(&self, mime: &str) -> bool {
        mime == "application/pdf"
    }
    fn extract(&self, s: &DocumentSource) -> Result<DocumentModel, String> {
        crate::extract_pdf(s.path, s.asset_dir, s.policy)
    }
}

struct DocxExtractor;
impl DocumentExtractor for DocxExtractor {
    fn name(&self) -> &str {
        "docx"
    }
    fn supports(&self, mime: &str) -> bool {
        mime == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    }
    fn extract(&self, s: &DocumentSource) -> Result<DocumentModel, String> {
        crate::extract_docx(s.path, s.asset_dir)
    }
}

struct HtmlExtractor;
impl DocumentExtractor for HtmlExtractor {
    fn name(&self) -> &str {
        "html"
    }
    fn supports(&self, mime: &str) -> bool {
        mime == "text/html"
    }
    fn extract(&self, s: &DocumentSource) -> Result<DocumentModel, String> {
        crate::extract_html(s.path)
    }
}

struct TextExtractor;
impl DocumentExtractor for TextExtractor {
    fn name(&self) -> &str {
        "text"
    }
    fn supports(&self, mime: &str) -> bool {
        mime.starts_with("text/")
    }
    fn extract(&self, s: &DocumentSource) -> Result<DocumentModel, String> {
        crate::extract_text(s.path)
    }
}

struct ImageExtractor;
impl DocumentExtractor for ImageExtractor {
    fn name(&self) -> &str {
        "image"
    }
    fn supports(&self, mime: &str) -> bool {
        crate::is_raster_image(mime)
    }
    fn extract(&self, s: &DocumentSource) -> Result<DocumentModel, String> {
        crate::extract_image_with(
            &crate::TesseractCli::new(),
            s.path,
            s.mime_type,
            s.asset_dir,
        )
    }
}
