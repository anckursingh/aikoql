//! Embedding providers: OpenAI-compatible (Ollama, OpenAI) and Candle (local).
//!
//! Implements `aikoql_kernel::EmbeddingProvider` so the kernel can generate
//! query-time vectors for `MATCH ... USING EMBEDDING`.
//!
//! ponytail: one trait, two impls. Candle behind `embedding-candle` feature.

use aikoql_kernel::knowledge::kom::{KnowledgeObject, Value};
use aikoql_kernel::EmbeddingProvider;
use aikoql_kernel::{KError, KResult};

// ---------------------------------------------------------------------------
// EmbeddingEnricher — wraps an EmbeddingProvider as an AiProvider
// ---------------------------------------------------------------------------

/// Adapts an `EmbeddingProvider` into the `AiProvider` interface so one
/// config flag drives both query-time ANN and background KO enrichment.
///
/// Extracts text from KO properties, calls `EmbeddingProvider::embed()`,
/// and returns an `EnrichmentResult` with the embedding vector.
pub struct EmbeddingEnricher {
    provider: std::sync::Arc<dyn EmbeddingProvider>,
    model: String,
}

impl EmbeddingEnricher {
    pub fn new(provider: std::sync::Arc<dyn EmbeddingProvider>, model: &str) -> Self {
        EmbeddingEnricher {
            provider,
            model: model.to_string(),
        }
    }
}

impl crate::AiProvider for EmbeddingEnricher {
    fn enrich(&self, ko: &KnowledgeObject) -> KResult<crate::EnrichmentResult> {
        let text: String = ko
            .properties
            .values()
            .filter_map(|v| match v {
                Value::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() {
            return Ok(crate::EnrichmentResult {
                embedding_model: None,
                embedding: None,
                summary: None,
                confidence: None,
            });
        }
        let embedding = self.provider.embed(&text, Some(&self.model))?;
        Ok(crate::EnrichmentResult {
            embedding_model: Some(self.model.clone()),
            embedding: Some(embedding),
            summary: None,
            confidence: None,
        })
    }
}

// ---------------------------------------------------------------------------
// MockEmbeddingProvider — for tests
// ---------------------------------------------------------------------------

/// Fixed-vector provider for testing. Returns `[0.1; DIM]`.
pub struct MockEmbeddingProvider {
    dim: usize,
}

impl Default for MockEmbeddingProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MockEmbeddingProvider {
    pub fn new() -> Self {
        MockEmbeddingProvider { dim: 384 }
    }

    pub fn with_dim(dim: usize) -> Self {
        MockEmbeddingProvider { dim }
    }
}

impl EmbeddingProvider for MockEmbeddingProvider {
    fn embed(&self, _text: &str, _model: Option<&str>) -> KResult<Vec<f32>> {
        Ok(vec![0.1; self.dim])
    }
}

// ---------------------------------------------------------------------------
// OpenAiCompatible — Ollama, OpenAI, any /v1/embeddings endpoint
// ---------------------------------------------------------------------------

#[cfg(feature = "embedding-openai")]
pub struct OpenAiEmbeddingProvider {
    base_url: String, // e.g. "http://localhost:11434" (no /v1 suffix)
    model: String,    // e.g. "nomic-embed-text"
    api_key: Option<String>,
}

#[cfg(feature = "embedding-openai")]
impl OpenAiEmbeddingProvider {
    /// `base_url` without `/v1` — we append `/v1/embeddings`.
    pub fn new(base_url: &str, model: &str, api_key: Option<&str>) -> Self {
        OpenAiEmbeddingProvider {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key: api_key.map(|s| s.to_string()),
        }
    }
}

#[cfg(feature = "embedding-openai")]
impl EmbeddingProvider for OpenAiEmbeddingProvider {
    fn embed(&self, text: &str, model: Option<&str>) -> KResult<Vec<f32>> {
        let body = serde_json::json!({
            "model": model.unwrap_or(&self.model),
            "input": text,
        });
        let body_str = serde_json::to_string(&body)
            .map_err(|e| KError::Store(format!("json serialize: {e}")))?;
        let url = format!("{}/v1/embeddings", self.base_url);
        let mut req = ureq::post(&url).set("Content-Type", "application/json");
        if let Some(ref key) = self.api_key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let resp = req
            .send_string(&body_str)
            .map_err(|e| KError::Store(format!("embedding request failed: {e}")))?;
        let resp_str = resp
            .into_string()
            .map_err(|e| KError::Store(format!("embedding read response: {e}")))?;
        let json: serde_json::Value = serde_json::from_str(&resp_str)
            .map_err(|e| KError::Store(format!("embedding parse error: {e}")))?;
        let vec: Vec<f32> = json["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| KError::Store("embedding response missing data[0].embedding".into()))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();
        Ok(vec)
    }
}

// ---------------------------------------------------------------------------
// CandleEmbedding — local BERT inference, no network at query time
// ---------------------------------------------------------------------------

#[cfg(feature = "embedding-candle")]
pub struct CandleEmbedding {
    model: std::sync::Mutex<candle_transformers::models::bert::BertModel>,
    tokenizer: tokenizers::Tokenizer,
    device: candle_core::Device,
}

#[cfg(feature = "embedding-candle")]
impl CandleEmbedding {
    /// Load `sentence-transformers/all-MiniLM-L6-v2` from HF Hub.
    /// First call downloads ~90MB and caches locally.
    pub fn new() -> KResult<Self> {
        // hf-hub 0.4 tokio API (rustls) — the sync API pulls native-tls/openssl.
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| KError::Store(format!("tokio-runtime: {e}")))?;
        let (config_path, tokenizer_path, weights_path) = rt.block_on(async {
            let api = hf_hub::api::tokio::Api::new()
                .map_err(|e| KError::Store(format!("hf-api: {e}")))?;
            let repo = api.model("sentence-transformers/all-MiniLM-L6-v2".into());
            Ok::<_, KError>((
                repo.get("config.json")
                    .await
                    .map_err(|e| KError::Store(format!("hf-config: {e}")))?,
                repo.get("tokenizer.json")
                    .await
                    .map_err(|e| KError::Store(format!("hf-tokenizer: {e}")))?,
                repo.get("model.safetensors")
                    .await
                    .map_err(|e| KError::Store(format!("hf-weights: {e}")))?,
            ))
        })?;

        let config_raw = std::fs::read_to_string(&config_path)
            .map_err(|e| KError::Store(format!("read config: {e}")))?;
        let config: candle_transformers::models::bert::Config =
            serde_json::from_str(&config_raw)
                .map_err(|e| KError::Store(format!("parse config: {e}")))?;
        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| KError::Store(format!("load tokenizer: {e}")))?;

        let device = candle_core::Device::Cpu;
        let vb = unsafe {
            candle_nn::VarBuilder::from_mmaped_safetensors(
                &[weights_path],
                candle_core::DType::F32,
                &device,
            )
        }
        .map_err(|e| KError::Store(format!("load weights: {e}")))?;
        let model = candle_transformers::models::bert::BertModel::load(vb, &config)
            .map_err(|e| KError::Store(format!("init model: {e}")))?;

        Ok(CandleEmbedding {
            model: std::sync::Mutex::new(model),
            tokenizer,
            device,
        })
    }
}

#[cfg(feature = "embedding-candle")]
impl EmbeddingProvider for CandleEmbedding {
    fn embed(&self, text: &str, model: Option<&str>) -> KResult<Vec<f32>> {
        // Candle provider ignores model override — we only have one loaded.
        let _ = model;

        let tokens = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| KError::Store(format!("tokenize: {e}")))?;

        let ids = candle_core::Tensor::new(tokens.get_ids(), &self.device)
            .map_err(|e| KError::Store(format!("ids tensor: {e}")))?
            .unsqueeze(0)
            .map_err(|e| KError::Store(format!("ids batch: {e}")))?;
        let mask = candle_core::Tensor::new(tokens.get_attention_mask(), &self.device)
            .map_err(|e| KError::Store(format!("mask tensor: {e}")))?
            .unsqueeze(0)
            .map_err(|e| KError::Store(format!("mask batch: {e}")))?;
        let type_ids = candle_core::Tensor::new(tokens.get_type_ids(), &self.device)
            .map_err(|e| KError::Store(format!("typeids tensor: {e}")))?
            .unsqueeze(0)
            .map_err(|e| KError::Store(format!("typeids batch: {e}")))?;

        let output = self
            .model
            .lock()
            .unwrap()
            .forward(&ids, &mask, Some(&type_ids))
            .map_err(|e| KError::Store(format!("forward: {e}")))?;

        // Mean-pool over sequence dim, masked by attention to exclude padding.
        let mask_f32 = mask
            .to_dtype(candle_core::DType::F32)
            .map_err(|e| KError::Store(format!("mask cast: {e}")))?;
        let mask_expanded = mask_f32
            .unsqueeze(2)
            .map_err(|e| KError::Store(format!("mask unsq: {e}")))?;
        let masked = output
            .broadcast_mul(&mask_expanded)
            .map_err(|e| KError::Store(format!("mask mul: {e}")))?;
        let sum = masked
            .sum(1)
            .map_err(|e| KError::Store(format!("sum: {e}")))?;
        let count = mask_f32
            .sum(1)
            .map_err(|e| KError::Store(format!("count: {e}")))?;
        let pooled = sum
            .broadcast_div(&count)
            .map_err(|e| KError::Store(format!("div: {e}")))?;

        // L2 normalize.
        let sqr = pooled
            .sqr()
            .map_err(|e| KError::Store(format!("sqr: {e}")))?;
        let norm_sum = sqr
            .sum(1)
            .map_err(|e| KError::Store(format!("norm sum: {e}")))?;
        let norm = norm_sum
            .sqrt()
            .map_err(|e| KError::Store(format!("sqrt: {e}")))?;
        let normalized = pooled
            .broadcast_div(&norm)
            .map_err(|e| KError::Store(format!("norm div: {e}")))?;

        let vec = normalized
            .to_vec1()
            .map_err(|e| KError::Store(format!("to_vec: {e}")))?;
        Ok(vec)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AiProvider;
    use aikoql_kernel::{
        ExtensionMap, Lifecycle, LifecycleState, Metadata, Origin, PropertyMap, SecurityDescriptor,
        KOID,
    };
    use std::sync::Arc;

    #[test]
    fn mock_returns_fixed_vector() {
        let p = MockEmbeddingProvider::with_dim(3);
        let v = p.embed("anything", None).unwrap();
        assert_eq!(v, vec![0.1, 0.1, 0.1]);
    }

    #[test]
    #[cfg(feature = "embedding-openai")]
    fn openai_config_validation() {
        let p = OpenAiEmbeddingProvider::new("http://localhost:99999", "test-model", None);
        // Should fail to connect — not a real server at that point.
        let result = p.embed("test", None);
        assert!(result.is_err());
    }

    // ---- EmbeddingEnricher tests ----

    fn test_ko(props: PropertyMap) -> KnowledgeObject {
        KnowledgeObject {
            koid: KOID::from_bytes([0u8; 16]),
            version: 1,
            commit_ts: 0,
            metadata: Metadata {
                type_name: "note".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            event_refs: vec![],
            security: SecurityDescriptor {
                owner: "test".into(),
                acl: vec![],
                classification: None,
            },
            lifecycle: Lifecycle {
                state: LifecycleState::Draft,
                origin: Origin::Human,
            },
            extensions: ExtensionMap::new(),
        }
    }

    #[test]
    fn enricher_extracts_text_and_embeds() {
        let mock = Arc::new(MockEmbeddingProvider::with_dim(3));
        let enricher = EmbeddingEnricher::new(mock, "mock-model");

        let mut props = PropertyMap::new();
        props.insert("body".into(), Value::Text("hello world".into()));
        let ko = test_ko(props);

        let result = enricher.enrich(&ko).unwrap();
        assert_eq!(result.embedding_model.as_deref(), Some("mock-model"));
        assert_eq!(result.embedding, Some(vec![0.1, 0.1, 0.1]));
        assert!(result.summary.is_none());
    }

    #[test]
    fn enricher_skips_empty_ko() {
        let mock = Arc::new(MockEmbeddingProvider::new());
        let enricher = EmbeddingEnricher::new(mock, "mock-model");

        let ko = test_ko(PropertyMap::new());

        let result = enricher.enrich(&ko).unwrap();
        // No text properties → no embedding.
        assert!(result.embedding.is_none());
        assert!(result.embedding_model.is_none());
    }
}
