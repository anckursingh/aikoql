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

/// Model id of the bundled offline embedding model (PRR-3).
pub const DEFAULT_MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// Local-store directory name for a model id (its last path segment).
pub fn model_slug(model_id: &str) -> &str {
    model_id.rsplit('/').next().unwrap_or(model_id)
}

/// hf-hub download of the three files a Bert model needs. The ONLY network
/// path (PRR-3) — used by `install` and `new`, never by the runtime.
#[cfg(feature = "embedding-candle")]
fn hf_download(
    model_id: &str,
) -> KResult<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
    // hf-hub 0.4 tokio API (rustls) — the sync API pulls native-tls/openssl.
    let rt =
        tokio::runtime::Runtime::new().map_err(|e| KError::Store(format!("tokio-runtime: {e}")))?;
    rt.block_on(async {
        let api =
            hf_hub::api::tokio::Api::new().map_err(|e| KError::Store(format!("hf-api: {e}")))?;
        let repo = api.model(model_id.into());
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
    })
}

#[cfg(feature = "embedding-candle")]
fn sha256_hex(path: &std::path::Path) -> KResult<String> {
    use sha2::Digest;
    let bytes =
        std::fs::read(path).map_err(|e| KError::Store(format!("read {}: {e}", path.display())))?;
    Ok(format!("{:x}", sha2::Sha256::digest(&bytes)))
}

/// R6 (review round 3): install-time integrity record — model id, embedding
/// dimension (from config.json `hidden_size`), and sha256 per model file.
#[cfg(feature = "embedding-candle")]
fn write_manifest(dir: &std::path::Path, model_id: &str) -> KResult<()> {
    let config_raw = std::fs::read_to_string(dir.join("config.json"))
        .map_err(|e| KError::Store(format!("read config.json: {e}")))?;
    let config: serde_json::Value = serde_json::from_str(&config_raw)
        .map_err(|e| KError::Store(format!("parse config.json: {e}")))?;
    let hidden_size = config
        .get("hidden_size")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| KError::Store("config.json has no hidden_size".into()))?;
    let mut files = serde_json::Map::new();
    for name in ["config.json", "tokenizer.json", "model.safetensors"] {
        files.insert(
            name.to_string(),
            serde_json::json!(sha256_hex(&dir.join(name))?),
        );
    }
    let manifest = serde_json::json!({
        "format": 1,
        "model_id": model_id,
        "embedding_dimension": hidden_size,
        "files": files,
    });
    let pretty = serde_json::to_string_pretty(&manifest)
        .map_err(|e| KError::Store(format!("serialize manifest: {e}")))?;
    std::fs::write(dir.join("manifest.json"), pretty)
        .map_err(|e| KError::Store(format!("write manifest.json: {e}")))
}

/// R6: verify an installed model against its manifest before loading.
/// Only the three known file names are ever checked (whitelist, not the
/// manifest's own keys) — a crafted manifest cannot point the hash check at
/// arbitrary paths.
#[cfg(feature = "embedding-candle")]
fn verify_install(dir: &std::path::Path) -> KResult<()> {
    let manifest_path = dir.join("manifest.json");
    let raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
        KError::Store(format!(
            "model at {} has no readable manifest.json ({e}) — reinstall with `aikoql model install`",
            dir.display()
        ))
    })?;
    let m: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| KError::Store(format!("parse {}: {e}", manifest_path.display())))?;
    let files = m.get("files").and_then(|f| f.as_object()).ok_or_else(|| {
        KError::Store(format!(
            "manifest {} is missing the files map — reinstall with `aikoql model install`",
            manifest_path.display()
        ))
    })?;
    for name in ["config.json", "tokenizer.json", "model.safetensors"] {
        let expected = files.get(name).and_then(|v| v.as_str()).ok_or_else(|| {
            KError::Store(format!(
                "manifest {} is missing {name} — reinstall with `aikoql model install`",
                manifest_path.display()
            ))
        })?;
        let actual = sha256_hex(&dir.join(name))?;
        if actual != expected {
            return Err(KError::Store(format!(
                "model file {name} checksum mismatch (installed file differs from the install-time record) — reinstall with `aikoql model install`"
            )));
        }
    }
    let config_raw = std::fs::read_to_string(dir.join("config.json"))
        .map_err(|e| KError::Store(format!("read config.json: {e}")))?;
    let config: serde_json::Value = serde_json::from_str(&config_raw)
        .map_err(|e| KError::Store(format!("parse config.json: {e}")))?;
    let hidden_size = config
        .get("hidden_size")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| KError::Store("config.json has no hidden_size".into()))?;
    let recorded = m.get("embedding_dimension").and_then(|v| v.as_u64());
    if recorded != Some(hidden_size) {
        return Err(KError::Store(format!(
            "model embedding dimension changed ({hidden_size} != {}) — reinstall with `aikoql model install`",
            recorded.map(|r| r.to_string()).unwrap_or_else(|| "unrecorded".into())
        )));
    }
    Ok(())
}

#[cfg(feature = "embedding-candle")]
impl CandleEmbedding {
    /// Download `sentence-transformers/all-MiniLM-L6-v2` from HF Hub and load
    /// it. Explicit install/test path only — the runtime must use
    /// `from_local` and never downloads (PRR-3).
    pub fn new() -> KResult<Self> {
        let (config_path, tokenizer_path, weights_path) = hf_download(DEFAULT_MODEL_ID)?;
        Self::from_files(&config_path, &tokenizer_path, &weights_path)
    }

    /// Load an installed model from a local directory containing
    /// config.json, tokenizer.json, and model.safetensors. No network.
    /// R6 (review round 3): the manifest.json written at install time is
    /// verified first — a tampered or half-written install fails here with a
    /// remediation message instead of loading silently.
    pub fn from_local(dir: &std::path::Path) -> KResult<Self> {
        verify_install(dir)?;
        Self::from_files(
            &dir.join("config.json"),
            &dir.join("tokenizer.json"),
            &dir.join("model.safetensors"),
        )
    }

    /// Install a model into the local store (`store/<slug>/`). Returns the
    /// installed directory. R6: staged + atomic — files land in a `.tmp-`
    /// sibling first (with a manifest.json recording model id, embedding
    /// dimension, and per-file sha256), then the swap renames the old install
    /// aside only for the instant the rename runs; a crash mid-install leaves
    /// the previous install intact.
    pub fn install(model_id: &str, store: &std::path::Path) -> KResult<std::path::PathBuf> {
        let (config_path, tokenizer_path, weights_path) = hf_download(model_id)?;
        let slug = model_slug(model_id);
        let dir = store.join(slug);
        let tmp = store.join(format!(".tmp-{slug}"));
        let old = store.join(format!(".old-{slug}"));
        let _ = std::fs::remove_dir_all(&tmp); // stale stage from a crashed install
        std::fs::create_dir_all(&tmp)
            .map_err(|e| KError::Store(format!("create model stage dir: {e}")))?;
        for (src, name) in [
            (&config_path, "config.json"),
            (&tokenizer_path, "tokenizer.json"),
            (&weights_path, "model.safetensors"),
        ] {
            std::fs::copy(src, tmp.join(name))
                .map_err(|e| KError::Store(format!("install {name}: {e}")))?;
        }
        write_manifest(&tmp, model_id)?;
        if dir.exists() {
            let _ = std::fs::remove_dir_all(&old);
            std::fs::rename(&dir, &old)
                .map_err(|e| KError::Store(format!("stage old install: {e}")))?;
        }
        std::fs::rename(&tmp, &dir).map_err(|e| KError::Store(format!("commit install: {e}")))?;
        // ponytail: cleanup is best-effort — a leftover .old-* is inert
        // (from_local only reads the canonical dir) and the next install
        // removes it.
        let _ = std::fs::remove_dir_all(&old);
        Ok(dir)
    }

    fn from_files(
        config_path: &std::path::Path,
        tokenizer_path: &std::path::Path,
        weights_path: &std::path::Path,
    ) -> KResult<Self> {
        let config_raw = std::fs::read_to_string(config_path)
            .map_err(|e| KError::Store(format!("read config: {e}")))?;
        let config: candle_transformers::models::bert::Config =
            serde_json::from_str(&config_raw)
                .map_err(|e| KError::Store(format!("parse config: {e}")))?;
        let mut tokenizer = tokenizers::Tokenizer::from_file(tokenizer_path)
            .map_err(|e| KError::Store(format!("load tokenizer: {e}")))?;
        // Cap input to all-MiniLM's 512-token window — enrichment text (e.g. an
        // ingested directory's ir_json) can be MBs and would blow up the tensor.
        let tp = tokenizers::TruncationParams {
            max_length: 512,
            ..Default::default()
        };
        tokenizer
            .with_truncation(Some(tp))
            .map_err(|e| KError::Store(format!("tokenizer truncation: {e}")))?;

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

        // NB: forward(input_ids, token_type_ids, attention_mask) — swapping
        // mask and type_ids feeds an all-zero attention mask, making every
        // token attend to all [PAD] positions and collapsing every text onto
        // the pad vector (measured: cosine 0.93-0.95 between unrelated texts).
        let output = self
            .model
            .lock()
            .unwrap()
            .forward(&ids, &type_ids, Some(&mask))
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
            .squeeze(0)
            .map_err(|e| KError::Store(format!("squeeze: {e}")))?
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

    fn cos(a: &[f32], b: &[f32]) -> f32 {
        let (mut d, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
        for (x, y) in a.iter().zip(b) {
            d += x * y;
            na += x * x;
            nb += y * y;
        }
        d / (na.sqrt() * nb.sqrt())
    }

    #[test]
    fn model_slug_strips_repo_prefix() {
        assert_eq!(
            model_slug("sentence-transformers/all-MiniLM-L6-v2"),
            "all-MiniLM-L6-v2"
        );
        assert_eq!(model_slug("plainname"), "plainname");
    }

    #[test]
    #[cfg(feature = "embedding-candle")]
    fn from_local_missing_dir_is_clear_error() {
        // PRR-3: from_local must never download — a missing install errors
        // immediately instead of hitting the network.
        let tmp = std::env::temp_dir().join(format!(
            "aikoql-semantic-missing-model-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let err = match CandleEmbedding::from_local(&tmp) {
            Ok(_) => panic!("from_local must fail on a missing install"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("manifest") || msg.contains("NotFound"),
            "error should name the missing manifest, got: {msg}"
        );
    }

    // R6 (review round 3): install-time integrity — manifest + per-file
    // sha256, verified before every load. No network: fake files, only the
    // verification seam is exercised.

    fn write_fake_install(dir: &std::path::Path, hidden_size: u64) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            format!(r#"{{"hidden_size":{hidden_size}}}"#),
        )
        .unwrap();
        std::fs::write(dir.join("tokenizer.json"), "{}").unwrap();
        std::fs::write(dir.join("model.safetensors"), b"fake-weights").unwrap();
        write_manifest(dir, "test-model").unwrap();
    }

    fn temp_install_dir(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("aikoql-semantic-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    #[cfg(feature = "embedding-candle")]
    fn verify_install_accepts_untampered_dir() {
        let dir = temp_install_dir("verify-ok");
        write_fake_install(&dir, 384);
        verify_install(&dir).expect("untampered install must verify");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(feature = "embedding-candle")]
    fn verify_install_rejects_tampered_file() {
        let dir = temp_install_dir("verify-tamper");
        write_fake_install(&dir, 384);
        // Tamper AFTER the manifest was written.
        std::fs::write(dir.join("tokenizer.json"), b"tampered").unwrap();
        let err = verify_install(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("checksum mismatch"), "got: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(feature = "embedding-candle")]
    fn verify_install_rejects_missing_manifest() {
        let dir = temp_install_dir("verify-nomanifest");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), r#"{"hidden_size":384}"#).unwrap();
        std::fs::write(dir.join("tokenizer.json"), "{}").unwrap();
        std::fs::write(dir.join("model.safetensors"), b"fake-weights").unwrap();
        let err = verify_install(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("manifest"), "got: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(feature = "embedding-candle")]
    fn verify_install_rejects_dimension_change() {
        let dir = temp_install_dir("verify-dim");
        write_fake_install(&dir, 384);
        // Swap in a config with a different hidden_size and rewrite the
        // manifest so its file hashes still match — the tamper here is the
        // dimension, which the hash check cannot see (belt+braces: a plain
        // byte tamper of config.json is already caught by the hash check).
        std::fs::write(dir.join("config.json"), r#"{"hidden_size":768}"#).unwrap();
        let manifest = serde_json::json!({
            "format": 1,
            "model_id": "test-model",
            "embedding_dimension": 384,
            "files": {
                "config.json": sha256_hex(&dir.join("config.json")).unwrap(),
                "tokenizer.json": sha256_hex(&dir.join("tokenizer.json")).unwrap(),
                "model.safetensors": sha256_hex(&dir.join("model.safetensors")).unwrap(),
            },
        });
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        let err = verify_install(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("dimension changed"), "got: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(feature = "embedding-candle")]
    fn candle_embeddings_distinguish_texts() {
        // Sanity gate for the model path: unrelated English texts must not
        // collapse onto the same vector (degenerate pooling shows up as
        // cosines ≥0.9 for everything, which silently wrecks semantic recall).
        let p = CandleEmbedding::new().expect("load model");
        let gibberish = p.embed("quadruple zulu wendigo galvanize", None).unwrap();
        let chunker = p
            .embed("MockDocumentChunker splits documents into chunks", None)
            .unwrap();
        let graph = p
            .embed(
                "the graph API panics when it truncates a multibyte string property",
                None,
            )
            .unwrap();
        let graph2 = p
            .embed(
                "the graph API panics when it truncates a multibyte string property",
                None,
            )
            .unwrap();
        let g_g = cos(&graph, &graph2);
        let g_c = cos(&graph, &chunker);
        let g_gib = cos(&graph, &gibberish);
        println!("same-text cosine: {g_g:.3}, unrelated: {g_c:.3}, gibberish: {g_gib:.3}");
        assert!(g_g > 0.99, "embedding must be deterministic");
        assert!(
            g_c < 0.8,
            "unrelated texts collapse (cosine {g_c:.3}) — pooling is degenerate"
        );
        assert!(g_gib < 0.8, "gibberish collapses (cosine {g_gib:.3})");
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
