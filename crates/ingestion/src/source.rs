//! D3.0: Typed source geometry — provenance from physical document layout.
//!
//! `SourceSpan` replaces ad-hoc string provenance for new multimodal paths.
//! `Evidence.bbox_text` (ir.rs) remains for backward compatibility until the
//! semantic-pipeline migration (PR-D) swaps candidates onto typed sources in
//! one sweep — touching ~20 construction sites per field change is churn
//! with zero behavior gain until candidates carry `source_fragments`.

use crate::ast::BoundingBox;

/// Typed source geometry: where a piece of knowledge came from.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceSpan {
    pub document_id: Option<String>,
    pub page: u32,
    /// Character offsets, where the source format has them.
    #[serde(default)]
    pub start_offset: Option<usize>,
    #[serde(default)]
    pub end_offset: Option<usize>,
    /// Physical page region.
    #[serde(default)]
    pub bbox: Option<BoundingBox>,
    /// Parent AST node id, when one was assigned.
    #[serde(default)]
    pub node_id: Option<String>,
}

/// Reference to an original visual asset (image, drawing) in the source document.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VisualAssetRef {
    pub asset_id: String,
    pub mime_type: String,
    pub content_hash: String,
    pub source: SourceSpan,
}

/// Typed evidence source. A claim can cite a paragraph, a table cell, a chart
/// point, a diagram edge, or an asset — not just "some text on some page".
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EvidenceSource {
    TextSpan {
        start_offset: usize,
        end_offset: usize,
    },
    Region {
        bbox: BoundingBox,
    },
    TableCell {
        table_id: String,
        cell_id: String,
    },
    ChartPoint {
        chart_id: String,
        series: String,
        point_index: usize,
    },
    DiagramNode {
        diagram_id: String,
        node_id: String,
    },
    DiagramEdge {
        diagram_id: String,
        edge_id: String,
    },
    Asset {
        asset_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_span_serde_roundtrip() {
        let span = SourceSpan {
            document_id: Some("doc-1".into()),
            page: 3,
            start_offset: Some(10),
            end_offset: Some(42),
            bbox: Some(BoundingBox {
                page: 3,
                x: 1.0,
                y: 2.0,
                width: 100.0,
                height: 20.0,
            }),
            node_id: Some("n7".into()),
        };
        let json = serde_json::to_string(&span).unwrap();
        let back: SourceSpan = serde_json::from_str(&json).unwrap();
        assert_eq!(back, span);
    }

    #[test]
    fn evidence_source_serde_roundtrip() {
        let src = EvidenceSource::ChartPoint {
            chart_id: "c1".into(),
            series: "Revenue".into(),
            point_index: 4,
        };
        let json = serde_json::to_string(&src).unwrap();
        let back: EvidenceSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, src);
    }

    #[test]
    fn visual_asset_ref_serde_roundtrip() {
        let asset = VisualAssetRef {
            asset_id: "a1".into(),
            mime_type: "image/png".into(),
            content_hash: "sha256:abc".into(),
            source: SourceSpan {
                document_id: None,
                page: 1,
                start_offset: None,
                end_offset: None,
                bbox: None,
                node_id: None,
            },
        };
        let json = serde_json::to_string(&asset).unwrap();
        let back: VisualAssetRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, asset);
    }
}
