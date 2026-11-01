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
use aikoql_kernel::knowledge::notify::EventFilter;
use aikoql_kernel::transaction::kernel::{Kernel, Subject};
use aikoql_kernel::KError;
use aikoql_scheduler::SchedulerJob;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Embedding providers
// ---------------------------------------------------------------------------

pub mod provider;

#[cfg(feature = "embedding-candle")]
pub use provider::CandleEmbedding;
pub use provider::EmbeddingEnricher;
pub use provider::MockEmbeddingProvider;
#[cfg(feature = "embedding-openai")]
pub use provider::OpenAiEmbeddingProvider;

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

/// Inner state shared with the background enrichment thread.
struct SemanticInner {
    water: AtomicU64,
    stop: AtomicBool,
    handle: Mutex<Option<JoinHandle<()>>>,
}

/// Background service that enriches KOs with semantic metadata.
/// Uses a pluggable `AiProvider`; the default no-op provider enriches nothing.
pub struct SemanticEngine {
    provider: Arc<dyn AiProvider>,
    inner: Arc<SemanticInner>,
}

impl SemanticEngine {
    pub fn new(provider: Arc<dyn AiProvider>) -> Self {
        SemanticEngine {
            provider,
            inner: Arc::new(SemanticInner {
                water: AtomicU64::new(0),
                stop: AtomicBool::new(false),
                handle: Mutex::new(None),
            }),
        }
    }

    /// Enrich a single KO: call the provider, build a `SemanticBlock`, and
    /// write it back via `kernel.remember()`.
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

        // Dedicated kernel path (device-eval N2): mutates ONLY the semantic
        // field. A remember-update would replace caller-created relationship
        // edges wholesale — the catch-up used to destroy the graph.
        kernel.attach_semantic(
            // System service: admin role so ACL-filtered scans see every KO
            // and the enrichment write is authorized (dogfood ingest found
            // plain "semantic-engine" was silently denied read on owned KOs).
            Subject::with_roles("semantic-engine", &["admin"]),
            ko.koid,
            semantic,
            Some(ko.version),
        )?;
        Ok(())
    }
}

impl SchedulerJob for SemanticEngine {
    fn name(&self) -> &str {
        "semantic-engine"
    }

    fn start(&self, kernel: &Kernel) -> KResult<()> {
        let subject = Subject::with_roles("semantic-engine", &["admin"]);
        // Catch-up: enrich all existing KOs that lack semantic blocks.
        // Enumerate actual types — a hardcoded whitelist misses ingested
        // directories, connectors, programs, etc. (dogfood ingest found this).
        for type_name in kernel.list_types()? {
            for ko in kernel.scan_by_type(&subject, &type_name)? {
                self.enrich_one(kernel, &ko)?;
            }
        }

        // Subscribe to live events so new remember+embed:true calls get processed.
        let rx = kernel.notify(EventFilter::default())?;
        let k = kernel.clone_handle();
        let provider = self.provider.clone();
        let inner = self.inner.clone();

        let handle = std::thread::spawn(move || loop {
            if inner.stop.load(Ordering::Relaxed) {
                break;
            }
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(ke) => {
                    // Only process creates/updates/claims — skip audit, lifecycle, forget.
                    match ke.kind {
                        EventKind::Created | EventKind::Updated | EventKind::ClaimAsserted => {}
                        _ => {
                            inner.water.store(ke.seq, Ordering::Relaxed);
                            continue;
                        }
                    }
                    match k.raw_object_at(&ke.koid, ke.commit_ts) {
                        Ok(Some(ko)) if ko.lifecycle.state != LifecycleState::Deleted => {
                            // enrich_one is idempotent (checks semantic.is_some()) —
                            // safe even if we see our own write-back events.
                            let engine = SemanticEngine {
                                provider: provider.clone(),
                                inner: inner.clone(),
                            };
                            // T-44: a query's embed can hold the model for one
                            // forward pass (try_lock fail-fast). Retry in place
                            // — a dropped event silently leaves the KO
                            // unenriched forever.
                            let mut enriched = false;
                            for _ in 0..40 {
                                match engine.enrich_one(&k, &ko) {
                                    Ok(()) => {
                                        enriched = true;
                                        break;
                                    }
                                    Err(KError::Retryable(_)) => {
                                        std::thread::sleep(Duration::from_millis(250));
                                    }
                                    Err(_) => break,
                                }
                            }
                            if enriched {
                                inner.water.store(ke.seq, Ordering::Relaxed);
                            }
                        }
                        _ => {
                            inner.water.store(ke.seq, Ordering::Relaxed);
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        });

        // justified: Mutex poison is unrecoverable
        *self.inner.handle.lock().unwrap() = Some(handle);
        Ok(())
    }

    fn shutdown(&self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        // justified: Mutex poison is unrecoverable
        if let Some(handle) = self.inner.handle.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    fn checkpoint(&self, _kernel: &Kernel, dir: &std::path::Path) -> KResult<()> {
        let tmp = dir.with_extension("tmp");
        if tmp.exists() {
            let _ = std::fs::remove_dir_all(&tmp);
        }
        std::fs::create_dir_all(&tmp)
            .map_err(|e| KError::Store(format!("checkpoint mkdir: {e}")))?;
        let water = self.inner.water.load(Ordering::Relaxed);
        std::fs::write(tmp.join("water.txt"), water.to_string())
            .map_err(|e| KError::Store(format!("checkpoint write: {e}")))?;
        std::fs::write(tmp.join("COMPLETE"), b"")
            .map_err(|e| KError::Store(format!("checkpoint complete: {e}")))?;
        if dir.exists() {
            std::fs::remove_dir_all(dir)
                .map_err(|e| KError::Store(format!("checkpoint rm old: {e}")))?;
        }
        std::fs::rename(&tmp, dir).map_err(|e| KError::Store(format!("checkpoint rename: {e}")))?;
        Ok(())
    }

    fn water(&self) -> u64 {
        self.inner.water.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aikoql_kernel::transaction::kernel::RememberRequest;
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

    // ---- AG-23b2: per-collection model bindings drive enrichment ----

    /// Records the model hints the engine passes to the provider, and echoes
    /// the requested model back as the stored `embedding_model` so tests can
    /// see which model was actually used.
    struct RecordingProvider {
        requested: std::sync::Mutex<Vec<Option<String>>>,
    }

    impl RecordingProvider {
        fn new() -> Self {
            RecordingProvider {
                requested: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn requested(&self) -> Vec<Option<String>> {
            // justified: Mutex poison is unrecoverable
            self.requested.lock().unwrap().clone()
        }
    }

    impl AiProvider for RecordingProvider {
        fn enrich(
            &self,
            ko: &KnowledgeObject,
            model: Option<&str>,
        ) -> KResult<EnrichmentResult> {
            // justified: Mutex poison is unrecoverable
            self.requested
                .lock()
                .unwrap()
                .push(model.map(String::from));
            let _ = ko;
            Ok(EnrichmentResult {
                embedding_model: Some(
                    model
                        .map(String::from)
                        .unwrap_or_else(|| "default-model".into()),
                ),
                embedding: Some(vec![0.1, 0.2, 0.3]),
                summary: None,
                confidence: None,
            })
        }
    }

    fn remember_body(k: &Kernel, subj: &Subject, body: &str) -> KOID {
        let mut props = PropertyMap::new();
        props.insert("body".into(), Value::Text(body.into()));
        k.remember(RememberRequest {
            context: subj.into(),
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
        .unwrap()
        .koid
    }

    /// A bound collection enriches with its bound model; an unbound one keeps
    /// the provider's default (hint None).
    #[test]
    fn ag23b2_bound_model_is_used_at_enrichment() {
        let k = mk();
        let subj = Subject::new("semantic-engine");
        k.catalog_set_type_model("fact", "big-model").unwrap();

        let bound_koid = remember_body(&k, &subj, "bound body");
        k.catalog_set_type_model("note", "ignored-model").unwrap(); // other type — inert here
        let unbound_koid = remember_body(&k, &subj, "unbound body");

        let provider = Arc::new(RecordingProvider::new());
        let engine = Arc::new(SemanticEngine::new(provider.clone()));
        let sched = Scheduler::new();
        sched.register(engine);
        sched.start_all(&k).unwrap(); // catch-up runs in start()

        let bound_ko = k.get(&subj, &bound_koid).unwrap();
        assert_eq!(
            bound_ko
                .semantic
                .as_ref()
                .and_then(|s| s.embedding_model.as_deref()),
            Some("big-model"),
            "a bound collection must be embedded with its bound model"
        );
        let unbound_ko = k.get(&subj, &unbound_koid).unwrap();
        assert_eq!(
            unbound_ko
                .semantic
                .as_ref()
                .and_then(|s| s.embedding_model.as_deref()),
            Some("default-model"),
            "an unbound collection keeps the provider's default"
        );
        assert!(
            provider.requested().contains(&Some("big-model".into())),
            "the provider must have been asked for the bound model: {:?}",
            provider.requested()
        );
    }

    /// Binding (or re-binding) a type migrates its existing KOs: the catch-up
    /// re-embeds anything whose stored model is not the bound one.
    #[test]
    fn ag23b2_binding_change_reembeds_existing_kos() {
        let k = mk();
        let subj = Subject::new("semantic-engine");

        // Pre-existing KO enriched under the old model.
        let koid = remember_body(&k, &subj, "already there");
        k.attach_semantic(
            subj.clone(),
            koid,
            SemanticBlock {
                embedding_model: Some("old-model".into()),
                embedding: Some(vec![0.5; 8]),
                confidence: None,
                source: Some("test".into()),
                summary: None,
            },
            Some(1),
        )
        .unwrap();

        k.catalog_set_type_model("fact", "new-model").unwrap();
        let provider = Arc::new(RecordingProvider::new());
        let engine = Arc::new(SemanticEngine::new(provider.clone()));
        let sched = Scheduler::new();
        sched.register(engine);
        sched.start_all(&k).unwrap();

        let ko = k.get(&subj, &koid).unwrap();
        assert_eq!(
            ko.semantic
                .as_ref()
                .and_then(|s| s.embedding_model.as_deref()),
            Some("new-model"),
            "the catch-up must migrate a KO whose stored model is not the bound one"
        );
    }

    /// A binding change migrates live, without a restart: the catalog row's
    /// own event triggers a rescan of the bound collection.
    #[test]
    fn ag23b2_live_binding_change_migrates_without_restart() {
        let k = mk();
        let subj = Subject::new("semantic-engine");

        let provider = Arc::new(RecordingProvider::new());
        let engine = Arc::new(SemanticEngine::new(provider.clone()));
        let sched = Scheduler::new();
        sched.register(engine);
        sched.start_all(&k).unwrap();

        let koid = remember_body(&k, &subj, "live body"); // enriched under default
        k.catalog_set_type_model("fact", "live-model").unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let ko = k.get(&subj, &koid).unwrap();
            if ko.semantic
                .as_ref()
                .and_then(|s| s.embedding_model.as_deref())
                == Some("live-model")
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the live binding change must migrate the collection without a restart"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Migration survives a kill: the catch-up re-runs on the next start and
    /// only finishes the remainder — already-migrated KOs stay untouched.
    #[test]
    fn ag23b2_migration_survives_a_kill() {
        let engine_handle: Arc<dyn aikoql_kernel::StorageEngine> =
            Arc::new(MemoryEngine::new());
        let k = Kernel::open(
            engine_handle.clone(),
            Arc::new(ManualClock::new(10_000)),
            0xC0FFEE,
        )
        .unwrap();
        let subj = Subject::new("semantic-engine");

        // Two KOs under the old model; the process "dies" before any
        // migration runs (no engine was ever started on this kernel).
        let koid_a = remember_body(&k, &subj, "a");
        let koid_b = remember_body(&k, &subj, "b");
        for koid in [koid_a, koid_b] {
            k.attach_semantic(
                subj.clone(),
                koid,
                SemanticBlock {
                    embedding_model: Some("old-model".into()),
                    embedding: Some(vec![0.5; 8]),
                    confidence: None,
                    source: Some("test".into()),
                    summary: None,
                },
                Some(1),
            )
            .unwrap();
        }
        k.catalog_set_type_model("fact", "new-model").unwrap();

        // Restart: a fresh kernel over the same engine + a fresh engine job.
        let k2 = Kernel::open(
            engine_handle,
            Arc::new(ManualClock::new(20_000)),
            0xC0FFEE,
        )
        .unwrap();
        let provider = Arc::new(RecordingProvider::new());
        let sched = Scheduler::new();
        sched.register(Arc::new(SemanticEngine::new(provider.clone())));
        sched.start_all(&k2).unwrap();

        for koid in [koid_a, koid_b] {
            let ko = k2.get(&subj, &koid).unwrap();
            assert_eq!(
                ko.semantic
                    .as_ref()
                    .and_then(|s| s.embedding_model.as_deref()),
                Some("new-model"),
                "restart must finish the interrupted migration"
            );
        }

        // A second engine on the same kernel must not re-migrate: versions
        // stay put (idempotent skip).
        let sched2 = Scheduler::new();
        sched2.register(Arc::new(SemanticEngine::new(provider)));
        sched2.start_all(&k2).unwrap();
        let ko = k2.get(&subj, &koid_a).unwrap();
        assert_eq!(ko.version, 3, "migrated KOs must not be re-embedded");
    }
}
