//! PR-F: Visual analysis — mock-first analyzers (HLD §17–§20, §32, §57).
//!
//! Every visual element goes through classification (§32): a `VisualClassifier`
//! decides *what* an asset is (photograph / chart / diagram / formula /
//! screenshot / scanned text / unknown); per-modality analyzers then attach
//! typed payloads to the canonical AST. Classification is cheap and local
//! (§33 staged processing) — no VLM, no heavyweight AI. The trait seams are
//! the plug-in points for specialist parsers later.
//!
//! Deviation from the HLD sketch: `classify` takes the `AstNode` (text /
//! caption / existing block type), not a `VisualAsset` — the AST node is the
//! canonical input at this pipeline stage and carries the asset reference.
//!
//! The pass mutates the AST in place: visual nodes gain payloads, and
//! `Image`/`Figure` nodes whose caption classifies as chart/diagram are
//! re-typed so the boundary detector emits the right fragment modality.

use crate::ast::{
    AstNode, AstPayload, BlockType, ChartPayload, ChartType, DiagramPayload, FormulaPayload,
    ImagePayload,
};

/// What kind of visual a node carries (HLD §32).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VisualClassification {
    Image,
    Chart,
    Diagram,
    Formula,
    Screenshot,
    ScannedText,
    #[default]
    Unknown,
}

/// Cheap classifier: what is this visual? The mock uses caption/alt/text
/// keywords; a real implementation would also inspect pixels (VLM or
/// specialist detector) — §33 staged processing keeps that optional.
pub trait VisualClassifier: Send + Sync {
    fn name(&self) -> &str;
    fn classify(&self, node: &AstNode) -> VisualClassification;
}

/// Keyword classifier. Rules (checked in order):
/// - block type Formula → Formula
/// - "chart|graph|plot|bar|pie|line chart|histogram" → Chart
/// - "diagram|flow|architecture|sequence|mermaid" → Diagram
/// - "screenshot" → Screenshot; "scan" → ScannedText
/// - Image/Figure block or an asset present → Image
/// - otherwise Unknown
pub struct MockVisualClassifier;

impl VisualClassifier for MockVisualClassifier {
    fn name(&self) -> &str {
        "mock-visual"
    }

    fn classify(&self, node: &AstNode) -> VisualClassification {
        if node.block_type == BlockType::Formula {
            return VisualClassification::Formula;
        }
        let text = node_text(node).to_lowercase();
        if text.is_empty() {
            return if node.asset.is_some()
                || matches!(node.block_type, BlockType::Image | BlockType::Figure)
            {
                VisualClassification::Image
            } else {
                VisualClassification::Unknown
            };
        }
        for kw in ["chart", "graph", "plot", "histogram", " pie", " bar chart"] {
            if text.contains(kw) {
                return VisualClassification::Chart;
            }
        }
        for kw in ["diagram", "flow", "architecture", "sequence", "mermaid"] {
            if text.contains(kw) {
                return VisualClassification::Diagram;
            }
        }
        if text.contains("screenshot") {
            return VisualClassification::Screenshot;
        }
        if text.contains("scan") {
            return VisualClassification::ScannedText;
        }
        if node.asset.is_some() || matches!(node.block_type, BlockType::Image | BlockType::Figure) {
            VisualClassification::Image
        } else {
            VisualClassification::Unknown
        }
    }
}

/// Model identifiers for mock analyzers (DoD §58 row 11: model versions
/// persisted — every visual-derived candidate carries one of these).
pub const MODEL_VISUAL: &str = "mock-visual-v1";
pub const MODEL_CHART: &str = "mock-chart-v1";
pub const MODEL_DIAGRAM: &str = "mock-diagram-v1";
pub const MODEL_IMAGE: &str = "mock-image-v1";
pub const MODEL_FORMULA: &str = "mock-formula-v1";

/// The four visual analyzer slots (HLD §32/§33: pluggable classifier +
/// staged specialists). Owned boxes so feature-gated builders (vlm) can
/// construct a set that owns its clients.
pub struct Analyzers {
    pub classifier: Box<dyn VisualClassifier>,
    pub chart: Box<dyn ChartAnalyzer>,
    pub diagram: Box<dyn DiagramAnalyzer>,
    pub image: Box<dyn ImageAnalyzer>,
}

impl Default for Analyzers {
    /// The mock set — deterministic, no model calls, the default pipeline.
    fn default() -> Self {
        Self {
            classifier: Box::new(MockVisualClassifier),
            chart: Box::new(MockChartAnalyzer),
            diagram: Box::new(MockDiagramAnalyzer),
            image: Box::new(MockImageAnalyzer),
        }
    }
}

/// Pipeline analyzer selection (HLD §33 staged): the VLM-backed set when
/// the `vlm` feature is on, the endpoint env is configured, and an asset
/// dir exists (the VLM needs asset bytes); the mock set otherwise — the
/// default build never requires a VLM (DoD row 14).
pub fn pipeline_analyzers(asset_dir: Option<&str>) -> Analyzers {
    #[cfg(feature = "vlm")]
    if let Some(dir) = asset_dir {
        if let Some(a) = crate::vlm::analyzers_from_env(dir) {
            return a;
        }
    }
    // Base build: mocks need no assets — the param exists for the vlm build.
    #[cfg(not(feature = "vlm"))]
    let _ = asset_dir;
    Analyzers::default()
}

/// Chart analyzer: structured interpretation of a chart node (HLD §10).
/// Mock: chart type and title from caption keywords; axes/series come from
/// the specialist chart-data pass (`fill_chart_data`) over an adjacent
/// table (HLD §33 staged processing — no VLM per image).
pub trait ChartAnalyzer: Send + Sync {
    fn name(&self) -> &str;
    fn analyze(&self, node: &AstNode) -> Option<ChartPayload>;
}

pub struct MockChartAnalyzer;

impl ChartAnalyzer for MockChartAnalyzer {
    fn name(&self) -> &str {
        "mock-chart"
    }

    fn analyze(&self, node: &AstNode) -> Option<ChartPayload> {
        let caption = caption_text(node)?;
        Some(ChartPayload {
            chart_type: chart_type_from_text(&caption),
            title: Some(caption.trim().to_string()),
            asset: node.asset.clone(),
            x_axis: None,
            y_axis: None,
            series: Vec::new(),
            extracted_data: None,
        })
    }
}

fn chart_type_from_text(text: &str) -> ChartType {
    let t = text.to_lowercase();
    if t.contains("pie") {
        ChartType::Pie
    } else if t.contains("histogram") {
        ChartType::Histogram
    } else if t.contains("scatter") {
        ChartType::Scatter
    } else if t.contains("area") {
        ChartType::Area
    } else if t.contains("line") {
        ChartType::Line
    } else if t.contains("bar") {
        ChartType::Bar
    } else {
        ChartType::Unknown
    }
}

/// HLD §33 staged processing: chart → specialist parser (cheapest stage —
/// no VLM per image). Fills x/y axes, series, and `extracted_data` from a
/// sibling table node: first column = x categories, remaining columns =
/// series (header = series name, "(unit)" suffix = axis unit).
fn fill_chart_data(chart: &mut ChartPayload, table_node: &AstNode) {
    if chart.extracted_data.is_some() {
        return;
    }
    let Some(table) = crate::ast::table_payload_from_node(table_node) else {
        return;
    };
    if table.headers.len() < 2 || table.rows.is_empty() {
        return;
    }
    let header_text = |i: usize| {
        table
            .headers
            .get(i)
            .map(|h| h.text.trim().to_string())
            .unwrap_or_default()
    };
    let (x_label, x_unit) = split_unit(header_text(0));
    chart.x_axis = Some(crate::ast::Axis {
        label: Some(x_label),
        unit: x_unit,
        min: None,
        max: None,
    });
    let (y_label, y_unit) = split_unit(header_text(1));
    chart.y_axis = Some(crate::ast::Axis {
        label: Some(y_label),
        unit: y_unit,
        min: None,
        max: None,
    });

    let mut series: Vec<crate::ast::ChartSeries> = Vec::new();
    for col in 1..table.headers.len() {
        let (name, _) = split_unit(header_text(col));
        let name = if name.is_empty() {
            format!("col{}", col)
        } else {
            name
        };
        let mut values = Vec::new();
        for row in &table.rows {
            let x = table
                .cells
                .iter()
                .find(|c| c.row_id == row.id && c.column_id == table.headers[0].id)
                .map(|c| c.text.trim().to_string())
                .unwrap_or_default();
            let Some(cell) = table
                .cells
                .iter()
                .find(|c| c.row_id == row.id && c.column_id == table.headers[col].id)
            else {
                continue;
            };
            let y = match &cell.value {
                Some(crate::ast::ScalarValue::Float(f)) => *f,
                Some(crate::ast::ScalarValue::Integer(i)) => *i as f64,
                _ => cell.text.trim().parse::<f64>().unwrap_or(f64::NAN),
            };
            if x.is_empty() || y.is_nan() {
                continue;
            }
            values.push(crate::ast::ChartPoint {
                x,
                y,
                confidence: cell.confidence,
                bbox: cell.bbox.clone(),
            });
        }
        if !values.is_empty() {
            series.push(crate::ast::ChartSeries { name, values });
        }
    }
    chart.series = series;
    chart.extracted_data = Some(table);
}

/// "Revenue (USD)" → ("Revenue", Some("USD")).
fn split_unit(header: String) -> (String, Option<String>) {
    if let Some(rest) = header.strip_suffix(')') {
        if let Some(pos) = rest.rfind('(') {
            let unit = rest[pos + 1..].trim().to_string();
            let label = rest[..pos].trim().to_string();
            if !unit.is_empty() && !label.is_empty() {
                return (label, Some(unit));
            }
        }
    }
    (header, None)
}

/// Diagram analyzer: nodes/edges from the diagram spec (HLD §11).
/// Mock: parses ASCII arrow chains (`A -> B --> C → D`, one chain per line)
/// — mermaid fence content primarily. Edge labels (`A -- label --> B`) are
/// preserved when present.
pub trait DiagramAnalyzer: Send + Sync {
    fn name(&self) -> &str;
    fn analyze(&self, node: &AstNode) -> Option<DiagramPayload>;
}

pub struct MockDiagramAnalyzer;

impl DiagramAnalyzer for MockDiagramAnalyzer {
    fn name(&self) -> &str {
        "mock-diagram"
    }

    fn analyze(&self, node: &AstNode) -> Option<DiagramPayload> {
        // Arrow text lives in the caption child for figure-marker diagrams
        // ("Figure 2: Architecture diagram" + arrow paragraph), and in
        // node.text for fenced (mermaid) diagrams. Gather both line-wise
        // (node_text would join them with spaces, corrupting the first
        // label) — non-arrow lines are skipped below, so the figure marker
        // itself is harmless.
        let mut text = node.text.clone().unwrap_or_default();
        if let Some(caption) = node
            .children
            .iter()
            .find(|c| c.block_type == BlockType::Caption)
            .and_then(|c| c.text.clone())
        {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&caption);
        }
        let mut nodes: Vec<crate::ast::DiagramNode> = Vec::new();
        let mut edges: Vec<crate::ast::DiagramEdge> = Vec::new();
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            // A --> B or A -> B or A → B or A -- label --> B
            let parts = split_arrow_chain(line);
            if parts.len() < 2 {
                continue;
            }
            for label in &parts {
                let id = node_id_from_label(label);
                if seen_ids.insert(id.clone()) {
                    nodes.push(crate::ast::DiagramNode {
                        id: id.clone(),
                        label: label.clone(),
                        node_type: None,
                        bbox: None,
                        confidence: 1.0,
                    });
                }
            }
            for (src, dst) in parts.iter().zip(parts.iter().skip(1)) {
                edges.push(crate::ast::DiagramEdge {
                    source: node_id_from_label(src),
                    target: node_id_from_label(dst),
                    label: None,
                    confidence: 1.0,
                    bbox: None,
                });
            }
        }

        if nodes.is_empty() {
            return None;
        }
        Some(DiagramPayload {
            nodes,
            edges,
            asset: node.asset.clone(),
            model: None,
        })
    }
}

/// Split "Client -> Gateway --> Payment Service → Ledger" into labels.
/// `--` inside a label is treated as a (dropped) edge annotation.
fn split_arrow_chain(line: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut rest = line;
    loop {
        // Find the next arrow of any supported shape.
        let mut arrow_start = None;
        let mut arrow_len = 0;
        for (pat, len) in [("-->", 3), ("->", 2), ("→", 3), ("—>", 4), ("–>", 4)] {
            if let Some(pos) = rest.find(pat) {
                if arrow_start.map(|s| pos < s).unwrap_or(true) {
                    arrow_start = Some(pos);
                    arrow_len = len;
                }
            }
        }
        match arrow_start {
            None => {
                let label = rest.trim();
                if !label.is_empty() {
                    parts.push(strip_edge_annotation(label).to_string());
                }
                break;
            }
            Some(pos) => {
                let label = rest[..pos].trim();
                if !label.is_empty() {
                    parts.push(strip_edge_annotation(label).to_string());
                }
                rest = &rest[pos + arrow_len..];
            }
        }
    }
    parts
}

/// "A -- label" → "A" (annotation sits after the label, before the arrow).
fn strip_edge_annotation(label: &str) -> &str {
    label.split(" --").next().unwrap_or(label).trim()
}

/// Deterministic node id from a label: lowercase, non-alphanumerics to '-'.
fn node_id_from_label(label: &str) -> String {
    let id: String = label
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // Collapse runs of '-' and trim edges.
    let collapsed: String = id.chars().fold(String::new(), |mut acc, c| {
        if c == '-' && acc.ends_with('-') {
            acc
        } else {
            acc.push(c);
            acc
        }
    });
    collapsed.trim_matches('-').to_string()
}

/// Image analyzer: multiple representations of an image (HLD §13) — the
/// asset reference plus caption/OCR. Mock: no OCR, no object detection,
/// no visual embedding (specialist seams, §33).
pub trait ImageAnalyzer: Send + Sync {
    fn name(&self) -> &str;
    fn analyze(&self, node: &AstNode) -> Option<ImagePayload>;
}

pub struct MockImageAnalyzer;

impl ImageAnalyzer for MockImageAnalyzer {
    fn name(&self) -> &str {
        "mock-image"
    }

    fn analyze(&self, node: &AstNode) -> Option<ImagePayload> {
        let asset = node.asset.clone()?;
        Some(ImagePayload {
            asset,
            ocr_text: None,
            ocr_model: None,
            caption: caption_text(node),
            detected_objects: Vec::new(),
            visual_embedding: None,
            model: None,
        })
    }
}

/// Caption text for a visual node: a `Caption` child, the node's own text
/// (alt text), or a following sibling paragraph that reads like a figure
/// caption ("Figure 1: …", "Chart 2 — …").
fn caption_text(node: &AstNode) -> Option<String> {
    for child in &node.children {
        if child.block_type == BlockType::Caption {
            if let Some(t) = child.text.as_deref() {
                if !t.trim().is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    if let Some(t) = node.text.as_deref() {
        if !t.trim().is_empty() && !t.starts_with("![") {
            return Some(t.to_string());
        }
    }
    None
}

/// All text available for classification: node text plus caption children.
fn node_text(node: &AstNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = node.text.as_deref() {
        parts.push(t.to_string());
    }
    if let Some(c) = caption_text(node) {
        parts.push(c);
    }
    parts.join(" ")
}

/// One classification pass over the whole AST. For every visual node:
/// classifies, attaches the payload (chart/diagram/image), and re-types
/// `Image`/`Figure` nodes whose caption says chart/diagram. Nodes without
/// payloads stay as-is — boundary detection falls back to text fragments,
/// so a degraded analyzer never loses content.
pub fn classify_visuals(ast: &mut crate::ast::DocumentAst) {
    classify_visuals_inner(ast, None, None, &Analyzers::default());
}

/// Classification + OCR fill for Screenshot/ScannedText images (HLD §33:
/// "OCR only if needed"). `asset_dir` locates persisted `{hash}.bin`
/// assets; `ocr` is the provider (tesseract CLI in production, mocks in
/// tests). Both degrade silently: no provider → no OCR, no asset file →
/// no OCR. Uses the mock analyzer set.
pub fn classify_visuals_with_assets(
    ast: &mut crate::ast::DocumentAst,
    asset_dir: Option<&str>,
    ocr: &dyn crate::ocr::OcrProvider,
) {
    classify_visuals_inner(ast, asset_dir, Some(ocr), &Analyzers::default());
}

/// Full seam (PR-O, HLD §32): the classification pass runs the supplied
/// analyzer set (mock or VLM-backed — see `pipeline_analyzers`).
pub fn classify_visuals_with_analyzers(
    ast: &mut crate::ast::DocumentAst,
    asset_dir: Option<&str>,
    ocr: Option<&dyn crate::ocr::OcrProvider>,
    analyzers: &Analyzers,
) {
    classify_visuals_inner(ast, asset_dir, ocr, analyzers);
}

fn classify_visuals_inner(
    ast: &mut crate::ast::DocumentAst,
    asset_dir: Option<&str>,
    ocr: Option<&dyn crate::ocr::OcrProvider>,
    analyzers: &Analyzers,
) {
    for page in &mut ast.pages {
        classify_visuals_in_children(&mut page.children, analyzers);
        if let (Some(dir), Some(provider)) = (asset_dir, ocr) {
            if provider.available() {
                fill_ocr_in_children(&mut page.children, dir, provider);
            }
        }
    }
}

/// OCR pass (post-classification): Screenshot/ScannedText images get
/// `ocr_text` + `ocr_model` filled from their persisted asset.
fn fill_ocr_in_children(nodes: &mut [AstNode], asset_dir: &str, ocr: &dyn crate::ocr::OcrProvider) {
    for node in nodes {
        if !node.children.is_empty() {
            fill_ocr_in_children(&mut node.children, asset_dir, ocr);
        }
        if !matches!(node.block_type, BlockType::Image | BlockType::Figure) {
            continue;
        }
        // The OCR gate re-classifies with the cheap text classifier even
        // when a VLM classifier is wired — §33 forbids a model call per
        // image here. ponytail: a VLM-classified scanned image without text
        // cues keeps its caption but skips the OCR fill until the gate
        // carries the main-pass classification.
        if !matches!(
            MockVisualClassifier.classify(node),
            VisualClassification::Screenshot | VisualClassification::ScannedText
        ) {
            continue;
        }
        let Some(AstPayload::Image(mut payload)) = node.payload.take() else {
            continue;
        };
        if payload.ocr_text.is_none() {
            if let Some((text, model)) = ocr_asset(asset_dir, &payload.asset, ocr) {
                payload.ocr_text = Some(text);
                payload.ocr_model = Some(model);
            }
        }
        node.payload = Some(AstPayload::Image(payload));
    }
}

/// OCR one persisted asset: load bytes, write a temp file with the right
/// extension (tesseract sniffs the format), recognize, clean up. Returns
/// (text, provider name) or None on any failure.
fn ocr_asset(
    asset_dir: &str,
    asset: &crate::source::VisualAssetRef,
    ocr: &dyn crate::ocr::OcrProvider,
) -> Option<(String, String)> {
    let ext = match asset.mime_type.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/webp" => "webp",
        _ => return None, // tesseract can't handle unknown formats
    };
    let bytes = crate::asset_store::load_asset(asset_dir, &asset.content_hash)?;
    let dir = std::env::temp_dir().join("aikoql-ocr");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.{}", asset.content_hash, ext));
    std::fs::write(&path, &bytes).ok()?;
    let result = ocr.recognize(&path.to_string_lossy(), "eng", &dir.to_string_lossy());
    std::fs::remove_file(&path).ok();
    let text = result.ok()?.text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some((text, ocr.name().to_string()))
}

fn classify_visuals_in_children(nodes: &mut Vec<AstNode>, analyzers: &Analyzers) {
    let mut claimed = vec![false; nodes.len()];
    for i in 0..nodes.len() {
        // Caption context: a following sibling (markdown convention — the
        // caption sits under the figure) or a preceding sibling (PDF text
        // runs precede the drawn images). Walk back past caption-less visual
        // siblings so a figure group shares its leading caption; a caption
        // paragraph already claimed as a following sibling of an earlier
        // visual (markdown shape) belongs to that figure, not this one.
        // ponytail: bounded 3-sibling walk; a real reading-order model if
        // captions drift further from their figures.
        let is_visual = matches!(
            nodes[i].block_type,
            BlockType::Image | BlockType::Figure | BlockType::Chart | BlockType::Diagram
        );
        let next = nodes
            .get(i + 1)
            .and_then(|n| n.text.as_deref())
            .filter(|t| is_caption_paragraph(t))
            .map(|t| t.to_string());
        if is_visual && next.is_some() {
            claimed[i + 1] = true;
        }
        let prev = (0..3.min(i)).find_map(|back| {
            let idx = i - back - 1;
            if claimed[idx] {
                return None; // caption already belongs to an earlier figure
            }
            let n = nodes.get(idx)?;
            if matches!(
                n.block_type,
                BlockType::Image | BlockType::Figure | BlockType::Chart | BlockType::Diagram
            ) && n.text.as_deref().is_none()
            {
                return None; // caption-less visual sibling — keep walking
            }
            n.text
                .as_deref()
                .filter(|t| is_caption_paragraph(t))
                .map(|t| t.to_string())
        });
        if !nodes[i].children.is_empty() {
            classify_visuals_in_children(&mut nodes[i].children, analyzers);
        }
        analyze_node(&mut nodes[i], next.or(prev).as_deref(), analyzers);
    }

    // HLD §33 staged processing: charts get structured data from an
    // adjacent table (cheap specialist parse — no VLM per image).
    for i in 0..nodes.len() {
        if nodes[i].block_type != BlockType::Chart {
            continue;
        }
        let Some(AstPayload::Chart(mut chart)) = nodes[i].payload.take() else {
            continue;
        };
        if chart.extracted_data.is_none() {
            // Data tables sit right after the caption (i+2) or before the
            // figure (i-1) — order varies in the wild.
            for j in [i.checked_add(1), i.checked_add(2), i.checked_sub(1)]
                .into_iter()
                .flatten()
            {
                if let Some(sibling) = nodes.get(j) {
                    fill_chart_data(&mut chart, sibling);
                    if chart.extracted_data.is_some() {
                        break;
                    }
                }
            }
        }
        nodes[i].payload = Some(AstPayload::Chart(chart));
    }

    // PR-N (HLD §24): adopt the page's vector-graphics SVG asset for
    // asset-less chart nodes — PDF chart drawings extracted from
    // content-stream path operators. The SVG node is a page-level
    // placeholder, not a semantic image, so it is consumed by the chart.
    // ponytail: one SVG per page serves the first asset-less chart; two
    // charts per page share nothing until region-level extraction exists
    // (needs per-op geometry + figure bboxes — a real layout model).
    let mut adopted: Vec<usize> = Vec::new();
    for i in 0..nodes.len() {
        let chart_needs_asset = nodes[i].block_type == BlockType::Chart
            && matches!(&nodes[i].payload, Some(AstPayload::Chart(c)) if c.asset.is_none());
        if !chart_needs_asset {
            continue;
        }
        let Some(j) = nodes.iter().enumerate().find_map(|(j, n)| {
            let is_svg = n
                .asset
                .as_ref()
                .map(|a| a.mime_type == "image/svg+xml")
                .unwrap_or(false);
            is_svg.then_some(j)
        }) else {
            continue;
        };
        let svg_asset = nodes[j].asset.clone();
        if let Some(AstPayload::Chart(chart)) = nodes[i].payload.as_mut() {
            chart.asset = svg_asset;
            adopted.push(j);
        }
    }
    if !adopted.is_empty() {
        let mut i = 0;
        nodes.retain(|_| {
            let keep = !adopted.contains(&i);
            i += 1;
            keep
        });
    }
}

/// "Figure 1: …" / "Chart 2 — …" / "Diagram: …" style caption paragraphs.
fn is_caption_paragraph(text: &str) -> bool {
    let t = text.trim();
    let lower = t.to_lowercase();
    for prefix in ["figure", "fig.", "fig ", "chart", "diagram", "table"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = rest.trim_start();
            if rest.is_empty() {
                return false; // bare "Figure" — too weak a signal
            }
            let mut chars = rest.chars();
            if chars.next().unwrap_or(' ').is_ascii_digit()
                || matches!(chars.next(), Some(':' | '.' | '—' | '-'))
            {
                return true;
            }
        }
    }
    false
}

fn analyze_node(node: &mut AstNode, next_caption: Option<&str>, analyzers: &Analyzers) {
    match node.block_type {
        BlockType::Formula => {
            if node.payload.is_none() {
                if let Some(text) = node.text.clone() {
                    let looks_latex = text.contains('\\') || text.contains('$');
                    node.payload = Some(AstPayload::Formula(FormulaPayload {
                        latex: if looks_latex {
                            Some(text.clone())
                        } else {
                            None
                        },
                        mathml: None,
                        plain_text: Some(text),
                    }));
                }
            }
        }
        BlockType::Diagram => {
            if node.payload.is_none() {
                node.payload = analyzers.diagram.analyze(node).map(AstPayload::Diagram);
            }
        }
        BlockType::Chart => {
            if node.payload.is_none() {
                node.payload = analyzers.chart.analyze(node).map(AstPayload::Chart);
            }
        }
        BlockType::Image | BlockType::Figure => {
            if node.payload.is_some() {
                return;
            }
            // Classify by caption: chart/diagram captions re-type the node;
            // everything else stays an image with an ImagePayload.
            let mut probe = node.clone();
            if probe.children.is_empty() {
                if let Some(cap) = next_caption {
                    probe.children.push(AstNode {
                        block_type: BlockType::Caption,
                        text: Some(cap.to_string()),
                        children: vec![],
                        bbox: None,
                        confidence: None,
                        ..Default::default()
                    });
                }
            }
            match analyzers.classifier.classify(&probe) {
                VisualClassification::Chart => {
                    node.block_type = BlockType::Chart;
                    node.payload = analyzers.chart.analyze(&probe).map(AstPayload::Chart);
                }
                VisualClassification::Diagram => {
                    node.block_type = BlockType::Diagram;
                    // Arrow text in the caption (figure-marker diagrams) can
                    // populate the diagram spec directly; an image whose
                    // caption has no arrows keeps the ImagePayload.
                    node.payload = analyzers
                        .diagram
                        .analyze(&probe)
                        .map(AstPayload::Diagram)
                        .or_else(|| analyzers.image.analyze(&probe).map(AstPayload::Image));
                }
                VisualClassification::Formula => {
                    node.block_type = BlockType::Formula;
                }
                _ => {
                    node.payload = analyzers.image.analyze(&probe).map(AstPayload::Image);
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::DocumentAst;
    use crate::source::{SourceSpan, VisualAssetRef};

    fn visual_node(block_type: BlockType, text: Option<&str>, children: Vec<AstNode>) -> AstNode {
        AstNode {
            block_type,
            text: text.map(|s| s.to_string()),
            children,
            bbox: None,
            confidence: None,
            ..Default::default()
        }
    }

    fn asset() -> VisualAssetRef {
        VisualAssetRef {
            asset_id: "h".into(),
            mime_type: "image/png".into(),
            content_hash: "h".into(),
            source: SourceSpan {
                document_id: None,
                page: 1,
                start_offset: None,
                end_offset: None,
                bbox: None,
                node_id: None,
            },
        }
    }

    fn ast_with(nodes: Vec<AstNode>) -> DocumentAst {
        DocumentAst {
            page_count: 1,
            pages: vec![AstNode {
                block_type: BlockType::Unknown,
                text: None,
                children: nodes,
                bbox: None,
                confidence: None,
                ..Default::default()
            }],
            source_type: "markdown".into(),
            document_id: None,
        }
    }

    #[test]
    fn classifier_keywords_route_modalities() {
        let c = MockVisualClassifier;
        assert_eq!(
            c.classify(&visual_node(
                BlockType::Image,
                Some("Revenue chart 2025"),
                vec![]
            )),
            VisualClassification::Chart
        );
        assert_eq!(
            c.classify(&visual_node(
                BlockType::Image,
                Some("Architecture diagram"),
                vec![]
            )),
            VisualClassification::Diagram
        );
        assert_eq!(
            c.classify(&visual_node(
                BlockType::Image,
                Some("screenshot of cli"),
                vec![]
            )),
            VisualClassification::Screenshot
        );
        assert_eq!(
            c.classify(&visual_node(
                BlockType::Image,
                Some("scan of invoice"),
                vec![]
            )),
            VisualClassification::ScannedText
        );
        assert_eq!(
            c.classify(&visual_node(BlockType::Formula, Some("x = 1"), vec![])),
            VisualClassification::Formula
        );
        assert_eq!(
            c.classify(&visual_node(BlockType::Image, None, vec![])),
            VisualClassification::Image
        );
    }

    #[test]
    fn chart_analyzer_detects_type_and_title() {
        let node = visual_node(
            BlockType::Chart,
            None,
            vec![visual_node(
                BlockType::Caption,
                Some("Bar chart: revenue"),
                vec![],
            )],
        );
        let payload = MockChartAnalyzer.analyze(&node).expect("payload");
        assert_eq!(payload.chart_type, ChartType::Bar);
        assert_eq!(payload.title.as_deref(), Some("Bar chart: revenue"));
    }

    #[test]
    fn diagram_analyzer_parses_arrow_chains() {
        let node = visual_node(
            BlockType::Diagram,
            Some("Client -> Gateway --> Payment Service → Ledger\nLedger -> Archive"),
            vec![],
        );
        let payload = MockDiagramAnalyzer.analyze(&node).expect("payload");
        assert_eq!(payload.nodes.len(), 5);
        assert_eq!(payload.edges.len(), 4);
        assert_eq!(payload.edges[0].source, "client");
        assert_eq!(payload.edges[0].target, "gateway");
        assert_eq!(payload.edges[2].target, "ledger");
        assert_eq!(payload.edges[2].source, "payment-service");
        assert_eq!(payload.edges[3].source, "ledger");
        assert_eq!(payload.edges[3].target, "archive");
        // node ids dedupe across chains sharing a label
        assert_eq!(
            payload.nodes.iter().filter(|n| n.label == "Ledger").count(),
            1
        );
    }

    #[test]
    fn diagram_analyzer_rejects_non_arrow_text() {
        let node = visual_node(BlockType::Diagram, Some("just some words"), vec![]);
        assert!(MockDiagramAnalyzer.analyze(&node).is_none());
    }

    #[test]
    fn diagram_analyzer_reads_arrows_from_caption_child() {
        // Figure-marker diagrams: arrows live in the caption child
        // (detect_figures consumes the paragraph after "Figure N:").
        let node = visual_node(
            BlockType::Figure,
            Some("Figure 2: Architecture diagram"),
            vec![visual_node(
                BlockType::Caption,
                Some("Client -> Gateway -> Database"),
                vec![],
            )],
        );
        let payload = MockDiagramAnalyzer.analyze(&node).expect("payload");
        assert_eq!(payload.nodes.len(), 3);
        assert_eq!(payload.edges.len(), 2);
        assert_eq!(payload.edges[0].source, "client");
        assert_eq!(payload.edges[0].target, "gateway");
        assert_eq!(payload.edges[1].target, "database");
    }

    #[test]
    fn figure_diagram_retype_attaches_diagram_payload_when_arrows_exist() {
        // Figure marker + arrows in node.text + caption child: the re-type
        // path must attach a DiagramPayload (not ImagePayload) so the
        // fragment leg emits a Diagram fragment. Regression for the golden
        // architecture-diagram fixture, which first produced a Text fragment.
        let mut ast = ast_with(vec![visual_node(
            BlockType::Figure,
            Some("Figure 2: Architecture diagram\nClient -> Gateway -> Database"),
            vec![visual_node(
                BlockType::Caption,
                Some("Gateway -> Cache"),
                vec![],
            )],
        )]);
        classify_visuals(&mut ast);

        let node = &ast.pages[0].children[0];
        assert_eq!(node.block_type, BlockType::Diagram);
        match &node.payload {
            Some(AstPayload::Diagram(d)) => {
                assert_eq!(d.nodes.len(), 4, "all four arrow labels become nodes");
                assert_eq!(d.edges.len(), 3);
            }
            other => panic!("expected diagram payload, got {:?}", other),
        }
    }

    #[test]
    fn image_analyzer_keeps_asset_and_caption() {
        let mut node = visual_node(
            BlockType::Image,
            Some("logo"),
            vec![visual_node(
                BlockType::Caption,
                Some("Figure 1: logo"),
                vec![],
            )],
        );
        node.asset = Some(asset());
        let payload = MockImageAnalyzer.analyze(&node).expect("payload");
        assert_eq!(payload.asset.content_hash, "h");
        assert_eq!(payload.caption.as_deref(), Some("Figure 1: logo"));
    }

    #[test]
    fn preceding_caption_paragraph_becomes_image_caption() {
        // PDF text runs precede the drawn images: [caption text, Im1, Im2] —
        // the walk-back must caption both asset images (PR-K fix; images.pdf
        // fixture shape).
        let mut img1 = visual_node(BlockType::Image, None, vec![]);
        img1.asset = Some(asset());
        let mut img2 = visual_node(BlockType::Image, None, vec![]);
        img2.asset = Some(asset());
        let mut ast = ast_with(vec![
            visual_node(
                BlockType::Paragraph,
                Some("Figure 7: Group caption"),
                vec![],
            ),
            img1,
            img2,
        ]);
        classify_visuals(&mut ast);

        for node in &ast.pages[0].children[1..] {
            match &node.payload {
                Some(AstPayload::Image(i)) => {
                    assert_eq!(i.caption.as_deref(), Some("Figure 7: Group caption"))
                }
                other => panic!("expected image payload, got {:?}", other),
            }
        }
    }

    #[test]
    fn caption_claimed_by_earlier_figure_is_not_reused() {
        // Markdown shape: [image, caption, image]. The caption belongs to
        // the first image; the walk-back must not re-type the second image
        // (regression: the logo after "Chart 1: …" became a Chart).
        let mut fees = visual_node(BlockType::Image, Some("fees"), vec![]);
        fees.asset = Some(asset());
        let mut logo = visual_node(BlockType::Image, Some("Logo"), vec![]);
        logo.asset = Some(asset());
        let mut ast = ast_with(vec![
            fees,
            visual_node(
                BlockType::Paragraph,
                Some("Chart 1: Fee structure by plan"),
                vec![],
            ),
            logo,
        ]);
        classify_visuals(&mut ast);

        let nodes = &ast.pages[0].children;
        assert!(matches!(nodes[0].payload, Some(AstPayload::Chart(_))));
        match &nodes[2].payload {
            Some(AstPayload::Image(i)) => {
                assert_eq!(i.caption.as_deref(), Some("Logo"), "logo stays an image")
            }
            other => panic!("expected image payload, got {:?}", other),
        }
    }

    #[test]
    fn classify_visuals_retypes_chart_caption_and_fills_payloads() {
        let mut ast = ast_with(vec![
            visual_node(BlockType::Image, Some("fees"), vec![]),
            visual_node(BlockType::Paragraph, Some("Chart 1: Fee structure"), vec![]),
            visual_node(BlockType::Diagram, Some("A -> B"), vec![]),
            visual_node(BlockType::Formula, Some("F = B * R"), vec![]),
        ]);
        classify_visuals(&mut ast);

        let nodes = &ast.pages[0].children;
        assert_eq!(nodes[0].block_type, BlockType::Chart);
        match &nodes[0].payload {
            Some(AstPayload::Chart(c)) => {
                assert_eq!(c.title.as_deref(), Some("Chart 1: Fee structure"));
                assert_eq!(c.chart_type, ChartType::Unknown);
            }
            other => panic!("expected chart payload, got {:?}", other),
        }
        assert!(matches!(nodes[2].payload, Some(AstPayload::Diagram(_))));
        assert!(matches!(nodes[3].payload, Some(AstPayload::Formula(_))));
    }

    fn table_node(rows: &[&[&str]]) -> AstNode {
        AstNode {
            block_type: BlockType::Table,
            text: None,
            children: rows
                .iter()
                .map(|row| AstNode {
                    block_type: BlockType::TableRow,
                    text: None,
                    children: row
                        .iter()
                        .map(|cell| AstNode {
                            block_type: BlockType::TableCell {
                                row_span: 1,
                                col_span: 1,
                            },
                            text: Some(cell.to_string()),
                            children: vec![],
                            bbox: None,
                            confidence: None,
                            ..Default::default()
                        })
                        .collect(),
                    bbox: None,
                    confidence: None,
                    ..Default::default()
                })
                .collect(),
            bbox: None,
            confidence: None,
            ..Default::default()
        }
    }

    #[test]
    fn chart_specialist_fills_axes_series_from_adjacent_table() {
        let mut ast = ast_with(vec![
            visual_node(BlockType::Image, Some("fees"), vec![]),
            visual_node(BlockType::Paragraph, Some("Chart 1: Fee structure"), vec![]),
            table_node(&[
                &["Quarter", "Revenue (USD)", "Cost (USD)"],
                &["Q1", "100", "60"],
                &["Q2", "120", "70"],
            ]),
        ]);
        classify_visuals(&mut ast);

        let nodes = &ast.pages[0].children;
        assert_eq!(nodes[0].block_type, BlockType::Chart);
        match &nodes[0].payload {
            Some(AstPayload::Chart(c)) => {
                assert_eq!(
                    c.x_axis.as_ref().and_then(|a| a.label.clone()).as_deref(),
                    Some("Quarter")
                );
                assert_eq!(
                    c.y_axis.as_ref().and_then(|a| a.label.clone()).as_deref(),
                    Some("Revenue")
                );
                assert_eq!(
                    c.y_axis.as_ref().and_then(|a| a.unit.clone()).as_deref(),
                    Some("USD")
                );
                assert_eq!(c.series.len(), 2);
                assert_eq!(c.series[0].name, "Revenue");
                assert_eq!(c.series[0].values.len(), 2);
                assert_eq!(c.series[0].values[0].x, "Q1");
                assert_eq!(c.series[0].values[0].y, 100.0);
                assert_eq!(c.series[1].name, "Cost");
                assert_eq!(c.series[1].values[1].x, "Q2");
                assert_eq!(c.series[1].values[1].y, 70.0);
                let table = c.extracted_data.as_ref().expect("extracted table");
                assert_eq!(table.headers.len(), 3);
                assert_eq!(table.rows.len(), 2);
            }
            other => panic!("expected chart payload, got {:?}", other),
        }
    }

    #[test]
    fn chart_specialist_leaves_chart_without_adjacent_table_unfilled() {
        let mut ast = ast_with(vec![
            visual_node(BlockType::Image, Some("fees"), vec![]),
            visual_node(BlockType::Paragraph, Some("Chart 1: Fee structure"), vec![]),
            visual_node(BlockType::Paragraph, Some("no data table here"), vec![]),
        ]);
        classify_visuals(&mut ast);
        match &ast.pages[0].children[0].payload {
            Some(AstPayload::Chart(c)) => {
                assert!(c.extracted_data.is_none());
                assert!(c.series.is_empty());
                assert!(c.x_axis.is_none());
            }
            other => panic!("expected chart payload, got {:?}", other),
        }
    }

    struct MockOcr {
        available: bool,
        text: &'static str,
    }

    impl crate::ocr::OcrProvider for MockOcr {
        fn name(&self) -> &str {
            "mock-ocr"
        }
        fn available(&self) -> bool {
            self.available
        }
        fn recognize(
            &self,
            _image_path: &str,
            _language: &str,
            _work_dir: &str,
        ) -> Result<crate::ocr::OcrPageResult, String> {
            Ok(crate::ocr::OcrPageResult {
                text: self.text.into(),
                confidence: 90.0,
                word_confidences: vec![],
                word_count: 2,
                words: vec![],
                block_bboxes: vec![],
            })
        }
    }

    #[test]
    fn ocr_fill_adds_ocr_text_to_screenshot_images() {
        let dir = std::env::temp_dir().join(format!("aikoql-test-assets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hash = crate::asset_store::store_asset(&dir.to_string_lossy(), b"fake-png").unwrap();
        let mut n = visual_node(BlockType::Image, Some("screenshot of cli"), vec![]);
        n.asset = Some(VisualAssetRef {
            asset_id: hash.clone(),
            mime_type: "image/png".into(),
            content_hash: hash,
            source: SourceSpan {
                document_id: None,
                page: 1,
                start_offset: None,
                end_offset: None,
                bbox: None,
                node_id: None,
            },
        });
        let mut ast = ast_with(vec![n]);
        let dir_str = dir.to_string_lossy().to_string();
        classify_visuals_with_assets(
            &mut ast,
            Some(&dir_str),
            &MockOcr {
                available: true,
                text: "INVOICE TOTAL 42",
            },
        );
        match &ast.pages[0].children[0].payload {
            Some(AstPayload::Image(i)) => {
                assert_eq!(i.ocr_text.as_deref(), Some("INVOICE TOTAL 42"));
                assert_eq!(i.ocr_model.as_deref(), Some("mock-ocr"));
            }
            other => panic!("expected image payload, got {:?}", other),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ocr_fill_skips_when_unavailable_or_not_text_classified() {
        // Provider unavailable → pass skipped entirely.
        let mut ast = ast_with(vec![visual_node(
            BlockType::Image,
            Some("screenshot of cli"),
            vec![],
        )]);
        ast.pages[0].children[0].asset = Some(asset());
        classify_visuals_with_assets(
            &mut ast,
            Some("no-such-dir"),
            &MockOcr {
                available: false,
                text: "X",
            },
        );
        match &ast.pages[0].children[0].payload {
            Some(AstPayload::Image(i)) => assert!(i.ocr_text.is_none()),
            other => panic!("expected image payload, got {:?}", other),
        }

        // Available provider, but a plain logo → not Screenshot/ScannedText.
        let mut ast = ast_with(vec![visual_node(BlockType::Image, Some("logo"), vec![])]);
        ast.pages[0].children[0].asset = Some(asset());
        classify_visuals_with_assets(
            &mut ast,
            Some("no-such-dir"),
            &MockOcr {
                available: true,
                text: "X",
            },
        );
        match &ast.pages[0].children[0].payload {
            Some(AstPayload::Image(i)) => assert!(i.ocr_text.is_none()),
            other => panic!("expected image payload, got {:?}", other),
        }

        // Screenshot with a missing asset file → load fails soft.
        let mut ast = ast_with(vec![visual_node(
            BlockType::Image,
            Some("scan of invoice"),
            vec![],
        )]);
        ast.pages[0].children[0].asset = Some(asset());
        classify_visuals_with_assets(
            &mut ast,
            Some("no-such-dir"),
            &MockOcr {
                available: true,
                text: "X",
            },
        );
        match &ast.pages[0].children[0].payload {
            Some(AstPayload::Image(i)) => assert!(i.ocr_text.is_none()),
            other => panic!("expected image payload, got {:?}", other),
        }
    }

    #[test]
    fn split_unit_parses_label_and_unit() {
        assert_eq!(
            split_unit("Revenue (USD)".into()),
            ("Revenue".into(), Some("USD".into()))
        );
        assert_eq!(split_unit("Revenue".into()), ("Revenue".into(), None));
        assert_eq!(split_unit("bad (paren".into()), ("bad (paren".into(), None));
        assert_eq!(split_unit("()".into()), ("()".into(), None));
    }

    #[test]
    fn classify_visuals_leaves_plain_images_as_image_payload() {
        let mut ast = ast_with(vec![{
            let mut n = visual_node(BlockType::Image, Some("logo"), vec![]);
            n.asset = Some(asset());
            n
        }]);
        classify_visuals(&mut ast);
        let node = &ast.pages[0].children[0];
        assert_eq!(node.block_type, BlockType::Image);
        assert!(matches!(node.payload, Some(AstPayload::Image(_))));
    }

    #[test]
    fn formula_payload_prefers_latex_when_present() {
        let mut ast = ast_with(vec![visual_node(
            BlockType::Formula,
            Some("\\frac{a}{b}"),
            vec![],
        )]);
        classify_visuals(&mut ast);
        match &ast.pages[0].children[0].payload {
            Some(AstPayload::Formula(f)) => {
                assert_eq!(f.latex.as_deref(), Some("\\frac{a}{b}"));
                assert_eq!(f.plain_text.as_deref(), Some("\\frac{a}{b}"));
            }
            other => panic!("expected formula payload, got {:?}", other),
        }
    }

    fn svg_image_node() -> AstNode {
        let mut n = visual_node(BlockType::Image, None, vec![]);
        n.asset = Some(VisualAssetRef {
            mime_type: "image/svg+xml".into(),
            ..asset()
        });
        n
    }

    #[test]
    fn chart_adopts_svg_vector_asset() {
        // PR-N: a figure-marked chart (no raster asset) adopts the page's
        // extracted vector-graphics SVG as its visual; the placeholder SVG
        // node is consumed.
        let mut ast = ast_with(vec![
            visual_node(
                BlockType::Image,
                Some("Figure 1: Revenue bar chart by quarter"),
                vec![],
            ),
            svg_image_node(),
        ]);
        classify_visuals(&mut ast);
        let nodes = &ast.pages[0].children;
        assert_eq!(nodes.len(), 1, "svg node consumed by the chart");
        assert_eq!(nodes[0].block_type, BlockType::Chart);
        match &nodes[0].payload {
            Some(AstPayload::Chart(c)) => {
                assert_eq!(
                    c.asset.as_ref().map(|a| a.asset_id.as_str()),
                    Some("h"),
                    "chart carries the svg asset"
                );
            }
            other => panic!("expected chart payload, got {other:?}"),
        }
    }

    #[test]
    fn svg_asset_without_chart_stays_an_image() {
        let mut ast = ast_with(vec![
            visual_node(BlockType::Paragraph, Some("plain prose"), vec![]),
            svg_image_node(),
        ]);
        classify_visuals(&mut ast);
        let nodes = &ast.pages[0].children;
        assert_eq!(nodes.len(), 2, "nothing adopts the svg node");
        assert_eq!(nodes[1].block_type, BlockType::Image);
    }
}
