//! PR-F DoD row 14 / PR-O: VLM analyzer set behind the visual traits
//! (HLD §32/§33), wired into the compile pipeline.
//!
//! Feature-gated (`vlm`) — never in the default build. Talks to an
//! OpenAI-compatible `/v1/chat/completions` endpoint, configured from the
//! environment:
//!
//! - `AIKOQL_VLM_ENDPOINT` — base URL (required, e.g. `https://api.openai.com/v1`)
//! - `AIKOQL_VLM_KEY` — bearer token (optional; local endpoints may need none)
//! - `AIKOQL_VLM_MODEL` — model id (default `gpt-4o-mini`)
//!
//! Wiring (PR-O): `analyzers_from_env` builds the staged set the compile
//! pipeline selects via `visual::pipeline_analyzers` — VLM classifier +
//! VLM image analyzer + a staged diagram analyzer (cheap arrow-text parse
//! first, VLM only when that yields nothing, §33 "VLM if needed"); charts
//! stay specialist-parsed (no VLM per image). Unset endpoint → mock set.

use crate::ast::{AstNode, DiagramPayload, ImagePayload};
use crate::source::VisualAssetRef;
use crate::visual::{
    Analyzers, DiagramAnalyzer, ImageAnalyzer, MockChartAnalyzer, MockDiagramAnalyzer,
    VisualClassification, VisualClassifier,
};

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
            model: Some(MODEL_VLM.into()),
        })
    }
}

/// Staged diagram analyzer (HLD §33 "diagram → VLM if needed"): the cheap
/// arrow-text specialist parses the caption first — the model is only asked
/// when it yields nothing. VLM output is untrusted (HLD §58): any transport
/// or parse failure returns `None` and the caller keeps the image payload.
pub struct VlmDiagramAnalyzer {
    client: VlmClient,
}

impl VlmDiagramAnalyzer {
    pub fn new(client: VlmClient) -> Self {
        Self { client }
    }
}

impl DiagramAnalyzer for VlmDiagramAnalyzer {
    fn name(&self) -> &str {
        "vlm-diagram"
    }

    fn analyze(&self, node: &AstNode) -> Option<DiagramPayload> {
        if let Some(payload) = MockDiagramAnalyzer.analyze(node) {
            return Some(payload);
        }
        let asset = node.asset.clone()?;
        let prompt = "Describe the diagram in this image. Return ONLY JSON: \
                      {\"nodes\":[{\"id\":\"a\",\"label\":\"Name\"}], \
                      \"edges\":[{\"source\":\"a\",\"target\":\"b\"}]}.";
        let text = self.client.complete(prompt, Some(&asset))?;
        parse_diagram_json(&text, asset)
    }
}

/// Forgiving JSON parse of untrusted VLM output: the first `{`..last `}`
/// span must parse as an object with a `nodes` array; every node needs an
/// id + label, every edge a source + target — anything else is dropped
/// rather than trusted. Nodes are deduped by id (first wins). `None` on any
/// failure — the caller keeps the image payload.
fn parse_diagram_json(text: &str, asset: VisualAssetRef) -> Option<DiagramPayload> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let v: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let mut nodes: Vec<crate::ast::DiagramNode> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for n in v["nodes"].as_array()? {
        let id = n["id"].as_str()?.to_string();
        let label = n["label"].as_str()?.to_string();
        if seen.insert(id.clone()) {
            nodes.push(crate::ast::DiagramNode {
                id,
                label,
                node_type: None,
                bbox: None,
                confidence: 0.8, // VLM-derived — below the specialist's 1.0
            });
        }
    }
    if nodes.is_empty() {
        return None;
    }
    let edges = v["edges"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|e| {
            Some(crate::ast::DiagramEdge {
                source: e["source"].as_str()?.to_string(),
                target: e["target"].as_str()?.to_string(),
                label: e["label"].as_str().map(String::from),
                confidence: 0.8,
                bbox: None,
            })
        })
        .collect();
    Some(DiagramPayload {
        nodes,
        edges,
        asset: Some(asset),
        model: Some(MODEL_VLM.into()),
    })
}

/// Staged analyzer set for the compile pipeline (HLD §33): VLM classifier +
/// image analyzer + staged diagram analyzer; charts stay specialist-parsed.
/// `None` when the endpoint env is unset — the pipeline keeps the mocks.
pub fn analyzers_from_env(asset_dir: &str) -> Option<Analyzers> {
    let config = VlmConfig::from_env()?;
    let client = VlmClient::new(config, Some(asset_dir.to_string()));
    Some(Analyzers {
        classifier: Box::new(VlmVisualClassifier::new(client.clone())),
        chart: Box::new(MockChartAnalyzer),
        diagram: Box::new(VlmDiagramAnalyzer::new(client.clone())),
        image: Box::new(VlmImageAnalyzer::new(client)),
    })
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

    /// Serializes env-mutating tests — process env is shared across the
    /// parallel test threads, and two tests setting `AIKOQL_VLM_*` at once
    /// race each other.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn config_requires_endpoint_env() {
        let _guard = ENV_LOCK.lock().unwrap();
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

    fn asset() -> VisualAssetRef {
        VisualAssetRef {
            asset_id: "a".into(),
            mime_type: "image/png".into(),
            content_hash: "h".into(),
            source: crate::source::SourceSpan {
                document_id: None,
                page: 1,
                start_offset: None,
                end_offset: None,
                bbox: None,
                node_id: None,
            },
        }
    }

    #[test]
    fn parse_diagram_json_forgives_prose_and_bad_shape() {
        // Prose around the JSON is fine; bad shapes degrade to None.
        let payload = parse_diagram_json(
            "Here you go: {\"nodes\":[{\"id\":\"a\",\"label\":\"Client\"}],\
             \"edges\":[{\"source\":\"a\",\"target\":\"b\"}]} end.",
            asset(),
        )
        .expect("json embedded in prose parses");
        assert_eq!(payload.nodes.len(), 1);
        assert_eq!(payload.nodes[0].label, "Client");
        assert!(payload.edges.is_empty() || payload.edges[0].source == "a");
        assert_eq!(payload.model.as_deref(), Some(MODEL_VLM));
        assert_eq!(
            payload.asset.as_ref().map(|a| a.asset_id.as_str()),
            Some("a")
        );
        assert!((payload.nodes[0].confidence - 0.8).abs() < f32::EPSILON);

        // Garbage, missing nodes, non-object, empty nodes → None (untrusted).
        assert!(parse_diagram_json("no json here", asset()).is_none());
        assert!(parse_diagram_json("{\"nodes\":\"not an array\"}", asset()).is_none());
        assert!(parse_diagram_json("[]", asset()).is_none());
        assert!(parse_diagram_json("{\"nodes\":[]}", asset()).is_none());
    }

    #[test]
    fn parse_diagram_json_dedupes_node_ids() {
        let text = "{\"nodes\":[{\"id\":\"a\",\"label\":\"One\"},\
                    {\"id\":\"a\",\"label\":\"Two\"},{\"id\":\"b\",\"label\":\"Three\"}]}";
        let payload = parse_diagram_json(text, asset()).expect("parses");
        assert_eq!(payload.nodes.len(), 2, "duplicate id dropped, first wins");
        assert_eq!(payload.nodes[0].label, "One");
    }

    #[test]
    fn staged_diagram_analyzer_parses_arrows_without_calling_vlm() {
        // §33: the cheap specialist must answer first — this test points the
        // client at an unreachable endpoint; any VLM call would return None
        // and fail the assert, proving the arrow path never dialed out.
        let client = VlmClient::new(
            VlmConfig {
                endpoint: "http://127.0.0.1:1/v1".into(),
                api_key: None,
                model: "x".into(),
            },
            None,
        );
        let analyzer = VlmDiagramAnalyzer::new(client);
        let node = AstNode {
            block_type: crate::ast::BlockType::Diagram,
            text: Some("Client -> Gateway --> Ledger".into()),
            children: vec![],
            bbox: None,
            confidence: None,
            payload: None,
            ..Default::default()
        };
        let payload = analyzer.analyze(&node).expect("arrow text parses locally");
        assert_eq!(payload.nodes.len(), 3);
        assert_eq!(payload.model, None, "specialist output stays mock-stamped");
    }

    #[test]
    fn analyzers_from_env_needs_endpoint() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("AIKOQL_VLM_ENDPOINT");
        assert!(analyzers_from_env("/tmp").is_none());

        std::env::set_var("AIKOQL_VLM_ENDPOINT", "https://vlm.example.com/v1");
        let a = analyzers_from_env("/tmp").expect("endpoint set");
        assert_eq!(a.classifier.name(), "vlm-visual");
        assert_eq!(a.diagram.name(), "vlm-diagram");
        assert_eq!(a.image.name(), "vlm-image");
        assert_eq!(
            a.chart.name(),
            "mock-chart",
            "charts stay specialist-parsed"
        );
        std::env::remove_var("AIKOQL_VLM_ENDPOINT");
    }
}
