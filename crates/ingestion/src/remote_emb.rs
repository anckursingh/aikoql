//! PR-P (HLD §60): remote embedding provider behind the embedding seams.
//!
//! Feature-gated (`remote_emb`) — never in the default build. Talks to an
//! OpenAI-compatible `/embeddings` endpoint, configured from the
//! environment:
//!
//! - `AIKOQL_EMBEDDING_ENDPOINT` — base URL (required, e.g. `https://api.openai.com/v1`)
//! - `AIKOQL_EMBEDDING_KEY` — bearer token (optional; local endpoints may need none)
//! - `AIKOQL_EMBEDDING_MODEL` — model id (default `text-embedding-3-small`)
//! - `AIKOQL_EMBEDDING_DIMS` — pinned vector dimensionality (optional; when
//!   unset the first successful response sets it — any model works)
//!
//! This provider exists to make the §60 real-model decision *measurable*:
//! `tests/real_model_bench.rs` runs the §60 retrieval instrument with a
//! live endpoint and emits a gate verdict against the pinned mock baseline
//! (0.867). The mock stays the default provider until that verdict is GO.
//!
//! Failures degrade to a zero vector — a failed embed must never silently
//! become a mock number in a benchmark (§58: untrusted optional output);
//! the caller's retrieval yields nothing, and `call_count` still records
//! the billed attempt.

use crate::embedding::EmbeddingProvider;
use crate::multimodal_embedding::MultimodalEmbeddingProvider;

/// Endpoint configuration from environment variables.
#[derive(Clone, Debug)]
pub struct RemoteEmbeddingConfig {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub model: String,
    /// Explicit dimensionality; `None` = 1536 until the first response
    /// says otherwise.
    pub dimensions: Option<usize>,
}

impl RemoteEmbeddingConfig {
    /// `None` when `AIKOQL_EMBEDDING_ENDPOINT` is unset — the signal that
    /// no remote model is configured and the mock provider stays.
    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("AIKOQL_EMBEDDING_ENDPOINT").ok()?;
        Some(Self {
            endpoint,
            api_key: std::env::var("AIKOQL_EMBEDDING_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
            model: std::env::var("AIKOQL_EMBEDDING_MODEL")
                .unwrap_or_else(|_| "text-embedding-3-small".into()),
            dimensions: std::env::var("AIKOQL_EMBEDDING_DIMS")
                .ok()
                .and_then(|d| d.parse().ok()),
        })
    }
}

/// Remote text-embedding provider over an OpenAI-compatible endpoint.
pub struct RemoteEmbeddingProvider {
    config: RemoteEmbeddingConfig,
    dims: std::sync::Mutex<usize>,
    calls: std::sync::atomic::AtomicUsize,
}

impl RemoteEmbeddingProvider {
    pub fn new(config: RemoteEmbeddingConfig) -> Self {
        let dims = config.dimensions.unwrap_or(1536);
        Self {
            config,
            dims: std::sync::Mutex::new(dims),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Embedding API calls made so far — the §60 ingestion-cost metric.
    pub fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn zeros(&self) -> Vec<f32> {
        vec![0.0; *self.dims.lock().unwrap()]
    }
}

impl EmbeddingProvider for RemoteEmbeddingProvider {
    fn name(&self) -> &str {
        "remote-emb"
    }

    fn dimensions(&self) -> usize {
        *self.dims.lock().unwrap()
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let body = serde_json::json!({ "model": self.config.model, "input": text });
        let mut req = ureq::post(&format!("{}/embeddings", self.config.endpoint))
            .header("Content-Type", "application/json");
        if let Some(key) = &self.config.api_key {
            req = req.header("Authorization", &format!("Bearer {}", key));
        }
        // Any transport/parse failure degrades to zeros (see module doc).
        let parsed = req
            .send_json(body)
            .ok()
            .and_then(|resp| resp.into_body().read_json::<serde_json::Value>().ok());
        let Some(v) = parsed else {
            return self.zeros();
        };
        let Some(out) = v["data"][0]["embedding"].as_array().map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_f64().map(|f| f as f32))
                .collect::<Vec<f32>>()
        }) else {
            return self.zeros();
        };
        if out.is_empty() {
            return self.zeros();
        }
        // Adopt the model's real dimensionality unless the env pinned one.
        if self.config.dimensions.is_none() {
            *self.dims.lock().unwrap() = out.len();
        }
        out
    }
}

impl MultimodalEmbeddingProvider for RemoteEmbeddingProvider {
    fn name(&self) -> &str {
        "remote-emb"
    }

    fn dimensions(&self) -> usize {
        *self.dims.lock().unwrap()
    }

    fn embed_text(&self, text: &str) -> Vec<f32> {
        self.embed(text)
    }

    // ponytail: text-only endpoint — the image channel is zeros and the
    // default `TextImage` dispatch stays text-dominant, so visual records
    // embed their captions. Replace with a true image embedding when a
    // multimodal endpoint exists.
    fn embed_image(&self, _image: &[u8]) -> Vec<f32> {
        self.zeros()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    /// Serializes env-mutating tests — process env is shared across the
    /// parallel test threads (same pattern as `vlm.rs`).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn config_from_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("AIKOQL_EMBEDDING_ENDPOINT");
        assert!(RemoteEmbeddingConfig::from_env().is_none());

        std::env::set_var("AIKOQL_EMBEDDING_ENDPOINT", "https://emb.example.com/v1");
        std::env::remove_var("AIKOQL_EMBEDDING_KEY");
        std::env::remove_var("AIKOQL_EMBEDDING_MODEL");
        std::env::remove_var("AIKOQL_EMBEDDING_DIMS");
        let config = RemoteEmbeddingConfig::from_env().expect("endpoint set");
        assert_eq!(config.endpoint, "https://emb.example.com/v1");
        assert!(config.api_key.is_none());
        assert_eq!(config.model, "text-embedding-3-small"); // default
        assert_eq!(config.dimensions, None);

        std::env::set_var("AIKOQL_EMBEDDING_KEY", "k");
        std::env::set_var("AIKOQL_EMBEDDING_MODEL", "custom-emb");
        std::env::set_var("AIKOQL_EMBEDDING_DIMS", "256");
        let config = RemoteEmbeddingConfig::from_env().expect("endpoint set");
        assert_eq!(config.api_key.as_deref(), Some("k"));
        assert_eq!(config.model, "custom-emb");
        assert_eq!(config.dimensions, Some(256));

        std::env::remove_var("AIKOQL_EMBEDDING_ENDPOINT");
        std::env::remove_var("AIKOQL_EMBEDDING_KEY");
        std::env::remove_var("AIKOQL_EMBEDDING_MODEL");
        std::env::remove_var("AIKOQL_EMBEDDING_DIMS");
    }

    /// One-request stub server: serves `response` once, returns its address
    /// and the join handle. Std-only — proves the live-endpoint path end to
    /// end without a real model.
    fn serve_once(response: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            // Read the COMPLETE request (headers + Content-Length body)
            // before responding — responding while the client is still
            // writing the body races its send (early close → the client
            // sees a transport error under parallel load).
            let mut buf = [0u8; 4096];
            let mut total = 0usize;
            let header_end = loop {
                match stream.read(&mut buf[total..]) {
                    Ok(0) => break total,
                    Ok(n) => {
                        total += n;
                        if let Some(i) = buf[..total].windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    }
                    Err(_) => break total,
                }
            };
            let headers = std::str::from_utf8(&buf[..header_end]).unwrap_or("");
            let content_length: usize = headers
                .lines()
                .find_map(|l| {
                    let l = l.trim().to_ascii_lowercase();
                    l.strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse().ok())
                })
                .unwrap_or(0);
            let mut body_read = total.saturating_sub(header_end);
            let mut scratch = [0u8; 4096];
            while body_read < content_length {
                match stream.read(&mut scratch) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => body_read += n,
                }
            }
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn embed_parses_response_and_adopts_dims() {
        let body = r#"{"data":[{"embedding":[0.1,0.2,0.3,0.4]}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let (endpoint, server) = serve_once(Box::leak(response.into_boxed_str()));
        let provider = RemoteEmbeddingProvider::new(RemoteEmbeddingConfig {
            endpoint: endpoint.clone(),
            api_key: None,
            model: "x".into(),
            dimensions: None,
        });
        assert_eq!(
            EmbeddingProvider::dimensions(&provider),
            1536,
            "default before first call"
        );
        let v = provider.embed("hi");
        assert_eq!(v, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(
            EmbeddingProvider::dimensions(&provider),
            4,
            "adopts the response dims"
        );
        assert_eq!(provider.call_count(), 1);
        server.join().unwrap();
    }

    #[test]
    fn embed_degrades_to_zeros_on_error() {
        let response =
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (endpoint, server) = serve_once(response);
        let provider = RemoteEmbeddingProvider::new(RemoteEmbeddingConfig {
            endpoint: endpoint.clone(),
            api_key: None,
            model: "x".into(),
            dimensions: Some(8),
        });
        assert_eq!(
            provider.embed("hi"),
            vec![0.0; 8],
            "500 → zeros, dims pinned"
        );
        assert_eq!(provider.call_count(), 1, "the billed attempt is counted");
        server.join().unwrap();
    }

    #[test]
    fn embed_degrades_to_zeros_when_unreachable() {
        let provider = RemoteEmbeddingProvider::new(RemoteEmbeddingConfig {
            endpoint: "http://127.0.0.1:1/v1".into(),
            api_key: None,
            model: "x".into(),
            dimensions: None,
        });
        assert_eq!(provider.embed("hi"), vec![0.0; 1536]);
        assert_eq!(provider.call_count(), 1);
    }

    #[test]
    fn mm_text_channel_is_the_remote_embed() {
        // Text routes to the endpoint; the image channel is zeros (text-only
        // endpoint) and the default TextImage dispatch stays text-dominant.
        let body = r#"{"data":[{"embedding":[1.0,2.0]}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let (endpoint, server) = serve_once(Box::leak(response.into_boxed_str()));
        let provider = RemoteEmbeddingProvider::new(RemoteEmbeddingConfig {
            endpoint: endpoint.clone(),
            api_key: None,
            model: "x".into(),
            dimensions: None,
        });
        let mm: &dyn MultimodalEmbeddingProvider = &provider;
        assert_eq!(mm.embed_text("hi"), vec![1.0, 2.0]);
        assert_eq!(mm.embed_image(&[0xAB]), vec![0.0; 2]);
        server.join().unwrap();
    }
}
