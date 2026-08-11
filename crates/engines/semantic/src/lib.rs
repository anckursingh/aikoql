//! Aikoql Semantic Engine — AI-powered knowledge enrichment.
//!
//! Consumes Knowledge Events, calls pluggable AI providers, and writes back
//! provenance-tagged `SemanticBlock` enrichments with `origin=SemanticEnrichment`.
//! Implements `SchedulerJob` so it plugs into the Scheduler's event loop.
//!
//! MRFC-0005 §Knowledge Services: Embedding generation, summarization,
//! classification, NER. All work is async, off the commit path, and writes
//! back versioned claims — never silent mutation (Determinism Law).

use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::transaction::kernel::{Kernel, RememberRequest, Subject};
use aikoql_scheduler::SchedulerJob;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Embedding providers
// ---------------------------------------------------------------------------

pub mod provider;

pub use provider::EmbeddingEnricher;
pub use provider::MockEmbeddingProvider;
#[cfg(feature = "embedding-openai")]
pub use provider::OpenAiEmbeddingProvider;
#[cfg(feature = "embedding-candle")]
pub use provider::CandleEmbedding;

// ---------------------------------------------------------------------------
// AI Provider plugin interface
// ---------------------------------------------------------------------------

/// Result from an AI provider for a single KO enrichment.
#[derive(Clone, Debug)]
pub struct EnrichmentResult {
    pub embedding_model: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub summary: Option<String>,
    pub confidence: Option<f32>,
}

/// Pluggable AI provider. Implementations can be local (ONNX, candle) or
/// remote (OpenAI-compatible endpoints). The engine calls `enrich` once per
/// KO that needs enrichment.
pub trait AiProvider: Send + Sync {
    /// Generate embeddings, summary, or classification for a KO's text content.
    fn enrich(&self, ko: &KnowledgeObject) -> KResult<EnrichmentResult>;
}

// ---------------------------------------------------------------------------
// SemanticEngine
// ---------------------------------------------------------------------------

/// Background service that enriches KOs with semantic metadata.
/// Uses a pluggable `AiProvider`; the default no-op provider enriches nothing.
pub struct SemanticEngine {
    provider: Arc<dyn AiProvider>,
}

impl SemanticEngine {
    pub fn new(provider: Arc<dyn AiProvider>) -> Self {
        SemanticEngine { provider }
    }

    /// Enrich a single KO: call the provider, build a `SemanticBlock`, and
    /// write it back via `kernel.remember()` with `origin=SemanticEnrichment`.
    fn enrich_one(&self, kernel: &Kernel, ko: &KnowledgeObject) -> KResult<()> {
        // Skip if already enriched (ponytail: check model name for multi-model).
        if ko.semantic.is_some() {
            return Ok(());
        }

        let enrichment = self.provider.enrich(ko)?;
        if enrichment.embedding.is_none() && enrichment.summary.is_none() {
            return Ok(()); // provider chose not to enrich
        }

        let semantic = SemanticBlock {
            embedding_model: enrichment.embedding_model,
            embedding: enrichment.embedding,
            confidence: enrichment.confidence,
            source: Some("semantic-engine".into()),
            summary: enrichment.summary,
        };

        let mut req = RememberRequest::update(
            Subject::new("semantic-engine"),
            ko.koid,
            ko.metadata.clone(),
        );
        req.properties = ko.properties.clone();
        req.semantic = Some(semantic);
        req.expected_version = Some(ko.version);
        req.note = Some("semantic enrichment".into());

        kernel.remember(req)?;
        Ok(())
    }
}

impl SchedulerJob for SemanticEngine {
    fn name(&self) -> &str {
        "semantic-engine"
    }

    fn start(&self, kernel: &Kernel) -> KResult<()> {
        let subject = Subject::new("semantic-engine");
        // Enrich all existing KOs that lack semantic blocks.
        // ponytail: scan_by_type per type; a scan_all_readable would avoid
        // the type-whitelist problem. Add when more than 2-3 types exist.
        for type_name in &["fact", "note", "claim", "evidence", "document"] {
            for ko in kernel.scan_by_type(&subject, type_name)? {
                self.enrich_one(kernel, &ko)?;
            }
        }
        Ok(())
    }

    fn shutdown(&self) {
        // Stateless — no background thread.
    }

    fn checkpoint(&self, _dir: &std::path::Path) -> KResult<()> {
        Ok(())
    }

    fn water(&self) -> u64 {
        0 // Stateless; enrichment is idempotent via version check.
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aikoql_kernel::{ManualClock, MemoryEngine, Metadata};
    use aikoql_scheduler::Scheduler;
    use std::sync::Arc;

    struct MockProvider;
    impl AiProvider for MockProvider {
        fn enrich(&self, ko: &KnowledgeObject) -> KResult<EnrichmentResult> {
            let body = ko
                .properties
                .get("body")
                .and_then(|v| match v {
                    Value::Text(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            Ok(EnrichmentResult {
                embedding_model: Some("mock-model".into()),
                embedding: Some(vec![0.1, 0.2, 0.3]),
                summary: Some(format!("summary: {}", body)),
                confidence: Some(0.95),
            })
        }
    }

    fn mk() -> Kernel {
        let clock = Arc::new(ManualClock::new(20_000));
        Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xB0CAFE).unwrap()
    }

    #[test]
    fn semantic_engine_enriches_ko() {
        let k = mk();
        let subj = Subject::new("semantic-engine");

        // Create a KO without semantic metadata.
        let mut props = PropertyMap::new();
        props.insert("body".into(), Value::Text("cats are great".into()));
        let r = k
            .remember(RememberRequest {
                context: (&subj).into(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: "fact".into(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                security: None,
                extensions: ExtensionMap::new(),
                origin: Origin::Human,
                note: None,
                referential_policy: ReferentialPolicy::default(),
            })
            .unwrap();

        let engine = Arc::new(SemanticEngine::new(Arc::new(MockProvider)));
        let sched = Scheduler::new();
        sched.register(engine);
        sched.start_all(&k).unwrap();

        // KO should now have semantic metadata.
        let koid = r.koid;
        let enriched = k.get(&subj, &koid).unwrap();
        assert!(enriched.semantic.is_some());
        let sem = enriched.semantic.as_ref().unwrap();
        assert_eq!(sem.embedding_model.as_deref(), Some("mock-model"));
        assert_eq!(sem.embedding.as_ref().unwrap().len(), 3);
        assert_eq!(sem.summary.as_deref(), Some("summary: cats are great"));
        assert_eq!(sem.source.as_deref(), Some("semantic-engine"));
        // Origin stays as Human — it reflects initial provenance.
        // SemanticBlock.source tracks who enriched it.
    }

    #[test]
    fn semantic_engine_skips_already_enriched() {
        let k = mk();
        let subj = Subject::new("semantic-engine");

        // Create a KO that already has semantic metadata.
        let mut props = PropertyMap::new();
        props.insert("body".into(), Value::Text("dogs are fine".into()));
        let r = k
            .remember(RememberRequest {
                context: (&subj).into(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: "fact".into(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: Some(SemanticBlock {
                    embedding_model: Some("existing-model".into()),
                    embedding: Some(vec![0.5; 128]),
                    confidence: Some(0.8),
                    source: Some("semantic-engine".into()),
                    summary: Some("existing summary".into()),
                }),
                relationships: vec![],
                security: None,
                extensions: ExtensionMap::new(),
                origin: Origin::SemanticEnrichment,
                note: None,
                referential_policy: ReferentialPolicy::default(),
            })
            .unwrap();

        let engine = Arc::new(SemanticEngine::new(Arc::new(MockProvider)));
        let sched = Scheduler::new();
        sched.register(engine);
        sched.start_all(&k).unwrap();

        // KO should NOT be re-enriched (version stays at 1).
        let koid2 = r.koid;
        let ko = k.get(&subj, &koid2).unwrap();
        assert_eq!(ko.version, 1);
        assert_eq!(
            ko.semantic.as_ref().unwrap().embedding_model.as_deref(),
            Some("existing-model")
        );
    }
}
