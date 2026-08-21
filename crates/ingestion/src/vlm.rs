//! PR-F DoD row 14: VLM analyzer set behind the visual traits (HLD §32).
//!
//! Feature-gated (`vlm`) — never in the default build. Talks to an
//! OpenAI-compatible `/v1/chat/completions` endpoint, configured from the
//! environment:
//!
//! - `AIKOQL_VLM_ENDPOINT` — base URL (required, e.g. `https://api.openai.com/v1`)
//! - `AIKOQL_VLM_KEY` — bearer token (optional; local endpoints may need none)
//! - `AIKOQL_VLM_MODEL` — model id (default `gpt-4o-mini`)
//!
//! Usage: build the analyzers and hand them to a pipeline stage that takes
//! the `VisualClassifier` / `ImageAnalyzer` trait objects — the seams PR-F
//! established. The default pipeline stays mock-only; a VLM is never
//! invoked per image (HLD §33).
//!
//! ```ignore
//! let config = VlmConfig::from_env().expect("AIKOQL_VLM_ENDPOINT");
//! let client = VlmClient::new(config, Some(asset_dir.to_string()));
//! let classifier = VlmVisualClassifier::new(client.clone());
//! let image = VlmImageAnalyzer::new(client);
//! ```

use crate::ast::{AstNode, ImagePayload};
use crate::source::VisualAssetRef;
use crate::visual::{ImageAnalyzer, VisualClassification, VisualClassifier};

/// Model version persisted on VLM-derived candidates (DoD row 14).
pub const MODEL_VLM: &str = "vlm-v1";

/// Endpoint configuration from environment variables.
#[derive(Clone, Debug)]
pub struct VlmConfig {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub model: String,
}

impl VlmConfig {
    /// `None` when `AIKOQL_VLM_ENDPOINT` is unset — the signal that VLM is
    /// not configured and the mock pipeline should be used instead.
    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("AIKOQL_VLM_ENDPOINT").ok()?;
        Some(Self {
            endpoint,
            api_key: std::env::var("AIKOQL_VLM_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
            model: std::env::var("AIKOQL_VLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into()),
        })
    }
}

/// Minimal client for OpenAI-compatible chat/completions calls.
#[derive(Clone)]
pub struct VlmClient {
    config: VlmConfig,
    /// Asset dir for loading persisted images as data URIs.
    asset_dir: Option<String>,
}

impl VlmClient {
    pub fn new(config: VlmConfig, asset_dir: Option<String>) -> Self {
        Self { config, asset_dir }
    }

    /// One vision completion. Returns the assistant text, `None` on any
    /// transport/parse failure — VLM output is untrusted and optional.
    fn complete(&self, prompt: &str, asset: Option<&VisualAssetRef>) -> Option<String> {
        let mut content = vec![serde_json::json!({"type": "text", "text": prompt})];
        if let (Some(dir), Some(asset)) = (&self.asset_dir, asset) {
            if let Some(bytes) = crate::asset_store::load_asset(dir, &asset.content_hash) {
                content.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:{};base64,{}", asset.mime_type, b64(&bytes)),
                    }
                }));
            }
        }
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [{"role": "user", "content": content}],
            "max_tokens": 512,
            "temperature": 0.0,
        });
        let mut req = ureq::post(&format!("{}/chat/completions", self.config.endpoint))
            .header("Content-Type", "application/json");
        if let Some(key) = &self.config.api_key {
            req = req.header("Authorization", &format!("Bearer {}", key));
        }
        let resp = req.send_json(body).ok()?;
        let v: serde_json::Value = resp.into_body().read_json().ok()?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
    }
}

/// Map a VLM label to the canonical classification. Forgiving: any of the
/// expected words may appear in a longer answer.
pub fn classification_from_label(label: &str) -> VisualClassification {
    let l = label.trim().to_lowercase();
    for (kw, class) in [
        ("chart", VisualClassification::Chart),
        // no "graph" — substring of "photograph" (image, not chart)
        ("plot", VisualClassification::Chart),
        ("diagram", VisualClassification::Diagram),
        ("formula", VisualClassification::Formula),
        ("equation", VisualClassification::Formula),
        ("screenshot", VisualClassification::Screenshot),
        ("scanned_text", VisualClassification::ScannedText),
        ("scan", VisualClassification::ScannedText),
        ("image", VisualClassification::Image),
        ("photo", VisualClassification::Image),
    ] {
        if l.contains(kw) {
            return class;
        }
    }
    VisualClassification::Unknown
}

/// VLM-backed classifier (HLD §32): asks the model what the visual is.
pub struct VlmVisualClassifier {
    client: VlmClient,
}

impl VlmVisualClassifier {
    pub fn new(client: VlmClient) -> Self {
        Self { client }
    }
}

impl VisualClassifier for VlmVisualClassifier {
    fn name(&self) -> &str {
        "vlm-visual"
    }

    fn classify(&self, node: &AstNode) -> VisualClassification {
        let prompt = "Classify this visual. Answer with exactly one word from: \
                      image, chart, diagram, formula, screenshot, scanned_text, unknown.";
        match self.client.complete(prompt, node.asset.as_ref()) {
            Some(label) => classification_from_label(&label),
            None => VisualClassification::Unknown,
        }
    }
}

/// VLM-backed image analyzer (HLD §13): caption + object detection prompt.
pub struct VlmImageAnalyzer {
    client: VlmClient,
}

impl VlmImageAnalyzer {
    pub fn new(client: VlmClient) -> Self {
        Self { client }
    }
}

impl ImageAnalyzer for VlmImageAnalyzer {
    fn name(&self) -> &str {
        "vlm-image"
    }

    fn analyze(&self, node: &AstNode) -> Option<ImagePayload> {
        let asset = node.asset.clone()?;
        let prompt =
            "Describe this image in one sentence. Then list the main objects, one per line.";
        let caption = self.client.complete(prompt, Some(&asset))?;
        Some(ImagePayload {
            asset,
            ocr_text: None,
            ocr_model: None,
            caption: Some(caption.trim().to_string()),
            detected_objects: Vec::new(),
            visual_embedding: None,
        })
    }
}

/// Standard base64 (RFC 4648, padded). Used for data URIs; the ureq stack
/// has no base64 of its own.
fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_matches_known_vectors() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foob"), "Zm9vYg==");
        assert_eq!(b64(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn classification_from_label_is_forgiving() {
        assert_eq!(
            classification_from_label("This is a chart."),
            VisualClassification::Chart
        );
        assert_eq!(
            classification_from_label("DIAGRAM"),
            VisualClassification::Diagram
        );
        assert_eq!(
            classification_from_label("a screenshot of the ui"),
            VisualClassification::Screenshot
        );
        assert_eq!(
            classification_from_label("photograph"),
            VisualClassification::Image
        );
        assert_eq!(
            classification_from_label("nothing relevant"),
            VisualClassification::Unknown
        );
    }

    #[test]
    fn config_requires_endpoint_env() {
        std::env::remove_var("AIKOQL_VLM_ENDPOINT");
        assert!(VlmConfig::from_env().is_none());

        std::env::set_var("AIKOQL_VLM_ENDPOINT", "https://vlm.example.com/v1");
        std::env::remove_var("AIKOQL_VLM_KEY");
        std::env::remove_var("AIKOQL_VLM_MODEL");
        let config = VlmConfig::from_env().expect("endpoint set");
        assert_eq!(config.endpoint, "https://vlm.example.com/v1");
        assert!(config.api_key.is_none());
        assert_eq!(config.model, "gpt-4o-mini"); // default

        std::env::set_var("AIKOQL_VLM_KEY", "k");
        std::env::set_var("AIKOQL_VLM_MODEL", "custom-model");
        let config = VlmConfig::from_env().expect("endpoint set");
        assert_eq!(config.api_key.as_deref(), Some("k"));
        assert_eq!(config.model, "custom-model");

        std::env::remove_var("AIKOQL_VLM_ENDPOINT");
        std::env::remove_var("AIKOQL_VLM_KEY");
        std::env::remove_var("AIKOQL_VLM_MODEL");
    }
}
