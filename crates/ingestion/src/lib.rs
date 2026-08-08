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
                security: SecurityDescriptor { owner: "ingester".into(), acl: vec![], classification: None },
                lifecycle: Lifecycle { state: LifecycleState::Draft, origin: Origin::Human },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_line_ingester_has_name() {
        let p = TextLineIngester;
        assert_eq!(p.name(), "text-line");
        assert!(p.supported_types().contains(&"text/plain"));
    }
}
