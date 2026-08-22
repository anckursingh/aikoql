//! PR-K (HLD §24): visual retrieval index.
//!
//! The visual index is an access path — query → visual similarity → visual
//! object → KnowledgeFragment → Knowledge Object → evidence — never the
//! source of truth. Records derive from the fragment stream, so the index
//! can always be rebuilt from the canonical segmentation.

use crate::ast::BoundingBox;
use crate::embedding::EmbeddingProvider;
use crate::fragment::{FragmentContent, KnowledgeFragment};
use crate::multimodal_embedding::{MultimodalEmbeddingInput, MultimodalEmbeddingProvider};

/// HLD §24: one retrievable visual object. `bbox` is optional here where
/// the HLD sketch assumes extractors always produce geometry — fragments
/// without a source region still index (an honest `None` over a fake box).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VisualIndexRecord {
    pub asset_id: String,
    pub document_id: String,
    pub page: u32,
    #[serde(default)]
    pub bbox: Option<BoundingBox>,
    pub embedding: Vec<f32>,
    pub semantic_caption: Option<String>,
    pub fragment_ids: Vec<String>,
}

/// Build the visual index over the final fragment stream. Mixed composites
/// recurse (PR-I hybrid merges nest visuals inside them). Images, charts,
/// and (PR-O) diagrams with an asset reference are visual objects;
/// formulas and text-sourced diagrams (mermaid fences) carry no asset —
/// their knowledge lives in the IR.
pub fn build_visual_index(
    fragments: &[KnowledgeFragment],
    provider: &dyn EmbeddingProvider,
) -> Vec<VisualIndexRecord> {
    build(fragments, &|asset, caption| {
        // Caption/title when present; the asset id otherwise — an embedding
        // of the empty string would retrieve nothing.
        let text = caption.unwrap_or(&asset.asset_id);
        provider.embed(text)
    })
}

/// Build the visual index with a multimodal provider (HLD §23 + §24):
/// when `load_asset` returns the visual object's bytes, the record embeds a
/// fused `TextImage` input; otherwise the text channel. The base build never
/// needs a multimodal model — `build_visual_index` above stays text-only.
pub fn build_visual_index_with_mm(
    fragments: &[KnowledgeFragment],
    mm: &dyn MultimodalEmbeddingProvider,
    load_asset: &mut dyn FnMut(&crate::source::VisualAssetRef) -> Option<Vec<u8>>,
) -> Vec<VisualIndexRecord> {
    // The embed closure is `Fn`, so the `FnMut` loader needs interior
    // mutability — one RefCell instead of threading mutability through build.
    let loader = std::cell::RefCell::new(load_asset);
    build(fragments, &|asset, caption| {
        let text = caption.unwrap_or(&asset.asset_id);
        match loader.borrow_mut()(asset) {
            Some(bytes) => mm.embed_multimodal(&MultimodalEmbeddingInput::TextImage {
                text,
                image: &bytes,
            }),
            None => mm.embed_text(text),
        }
    })
}

fn build(
    fragments: &[KnowledgeFragment],
    embed_visual: &dyn Fn(&crate::source::VisualAssetRef, Option<&str>) -> Vec<f32>,
) -> Vec<VisualIndexRecord> {
    let mut out = Vec::new();
    for frag in fragments {
        collect(frag, embed_visual, &mut out);
    }
    out
}

fn collect(
    frag: &KnowledgeFragment,
    embed_visual: &dyn Fn(&crate::source::VisualAssetRef, Option<&str>) -> Vec<f32>,
    out: &mut Vec<VisualIndexRecord>,
) {
    let (asset, caption) = match &frag.content {
        FragmentContent::Image(image) => (
            Some(image.asset.clone()),
            image.caption.clone().or_else(|| {
                image
                    .ocr_text
                    .as_ref()
                    .map(|t| t.chars().take(200).collect())
            }),
        ),
        FragmentContent::Chart(chart) => (chart.asset.clone(), chart.title.clone()),
        // PR-O: asset-backed diagrams are visual objects too. The caption is
        // honest None — DiagramPayload carries structure, not a title; text
        // ranking for diagrams stays in the IR.
        FragmentContent::Diagram(diagram) => (diagram.asset.clone(), None),
        FragmentContent::Mixed(children) => {
            for child in children {
                collect(child, embed_visual, out);
            }
            return;
        }
        _ => (None, None),
    };
    let Some(asset) = asset else { return };
    // Embed before moving asset_id into the record (partial-move order).
    let embedding = embed_visual(&asset, caption.as_deref());
    out.push(VisualIndexRecord {
        asset_id: asset.asset_id,
        document_id: frag.context.document_id.clone().unwrap_or_default(),
        page: frag.context.page.unwrap_or(0),
        bbox: frag.source.as_ref().and_then(|s| s.bbox.clone()),
        embedding,
        semantic_caption: caption,
        fragment_ids: vec![frag.fragment_id.clone()],
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{AstNode, BlockType, ChartPayload, ChartType, DocumentAst, ImagePayload};
    use crate::boundary::{KnowledgeBoundaryDetector, RuleBoundaryDetector};
    use crate::embedding::MockEmbeddingProvider;
    use crate::fragment::{FragmentContext, FragmentModality};
    use crate::source::{SourceSpan, VisualAssetRef};

    fn asset() -> VisualAssetRef {
        VisualAssetRef {
            asset_id: "asset-1".into(),
            mime_type: "image/png".into(),
            content_hash: "deadbeef".into(),
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

    fn image_node() -> AstNode {
        AstNode {
            block_type: BlockType::Image,
            text: Some("Figure 1: payment flow".into()),
            children: vec![],
            bbox: None,
            confidence: None,
            payload: Some(crate::ast::AstPayload::Image(ImagePayload {
                asset: asset(),
                ocr_text: None,
                ocr_model: None,
                caption: Some("Figure 1: payment flow".into()),
                detected_objects: vec![],
                visual_embedding: None,
                model: None,
            })),
            ..Default::default()
        }
    }

    fn text_node(text: &str) -> AstNode {
        AstNode {
            block_type: BlockType::Unknown,
            text: Some(text.to_string()),
            children: vec![],
            bbox: None,
            confidence: None,
            ..Default::default()
        }
    }

    fn ast(nodes: Vec<AstNode>) -> DocumentAst {
        let page_node = AstNode {
            block_type: BlockType::Unknown,
            text: None,
            children: nodes,
            bbox: None,
            confidence: None,
            ..Default::default()
        };
        DocumentAst {
            page_count: 1,
            pages: vec![page_node],
            source_type: "test".into(),
            document_id: None,
        }
    }

    fn fragments(ast: &DocumentAst) -> Vec<KnowledgeFragment> {
        RuleBoundaryDetector.detect(ast).unwrap()
    }

    #[test]
    fn image_fragment_indexes_with_asset_and_caption() {
        let fs = fragments(&ast(vec![image_node(), text_node("Some body text.")]));
        let provider = MockEmbeddingProvider::new();
        let index = build_visual_index(&fs, &provider);
        assert_eq!(index.len(), 1, "one visual object");
        let rec = &index[0];
        assert_eq!(rec.asset_id, "asset-1");
        assert_eq!(rec.page, 1);
        assert_eq!(rec.fragment_ids, vec!["frag-p1-b0".to_string()]);
        assert_eq!(
            rec.semantic_caption.as_deref(),
            Some("Figure 1: payment flow")
        );
        assert!(rec.bbox.is_none(), "no geometry on this fragment");
        assert!(!rec.embedding.is_empty());
        assert_eq!(rec.document_id, "", "no document id on the test AST");
    }

    #[test]
    fn text_only_document_produces_no_records() {
        let fs = fragments(&ast(vec![text_node("Just prose, no visuals.")]));
        let provider = MockEmbeddingProvider::new();
        assert!(build_visual_index(&fs, &provider).is_empty());
    }

    #[test]
    fn mixed_fragment_walks_to_nested_visual() {
        // PR-I hybrid merges nest visuals inside Mixed composites; the index
        // must still find them (record cites the child's fragment id).
        let child = KnowledgeFragment {
            fragment_id: "frag-p1-b0".into(),
            modality: FragmentModality::Image,
            context: FragmentContext {
                page: Some(1),
                ..Default::default()
            },
            content: FragmentContent::Image(ImagePayload {
                asset: asset(),
                ocr_text: None,
                ocr_model: None,
                caption: Some("Figure 2: nested".into()),
                detected_objects: vec![],
                visual_embedding: None,
                model: None,
            }),
            source: None,
            evidence: Vec::new(),
            confidence: 1.0,
        };
        let composite = KnowledgeFragment {
            fragment_id: "frag-p1-b0-mixed".into(),
            modality: FragmentModality::Mixed,
            context: FragmentContext {
                page: Some(1),
                ..Default::default()
            },
            content: FragmentContent::Mixed(vec![Box::new(child)]),
            source: None,
            evidence: Vec::new(),
            confidence: 1.0,
        };
        let provider = MockEmbeddingProvider::new();
        let index = build_visual_index(&[composite], &provider);
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].fragment_ids, vec!["frag-p1-b0".to_string()]);
        assert_eq!(
            index[0].semantic_caption.as_deref(),
            Some("Figure 2: nested")
        );
    }

    #[test]
    fn chart_records_require_an_asset() {
        let make = |title: Option<String>, asset: Option<VisualAssetRef>| KnowledgeFragment {
            fragment_id: "frag-p1-b0".into(),
            modality: FragmentModality::Chart,
            context: FragmentContext {
                page: Some(1),
                ..Default::default()
            },
            content: FragmentContent::Chart(ChartPayload {
                chart_type: ChartType::Bar,
                title,
                asset,
                x_axis: None,
                y_axis: None,
                series: Vec::new(),
                extracted_data: None,
            }),
            source: None,
            evidence: Vec::new(),
            confidence: 1.0,
        };
        let provider = MockEmbeddingProvider::new();
        // Title without an asset: not a visual object (nothing to retrieve
        // visually); the IR carries the chart knowledge.
        assert!(
            build_visual_index(&[make(Some("Quarterly revenue".into()), None)], &provider)
                .is_empty()
        );
        // Asset-backed chart: indexed, title as the semantic caption.
        let index = build_visual_index(
            &[make(Some("Quarterly revenue".into()), Some(asset()))],
            &provider,
        );
        assert_eq!(index.len(), 1);
        assert_eq!(
            index[0].semantic_caption.as_deref(),
            Some("Quarterly revenue")
        );
    }

    #[test]
    fn diagram_records_require_an_asset() {
        // PR-O: asset-backed diagrams index (visual diagram ranking);
        // text-sourced diagrams (mermaid fences) stay IR-only.
        let make = |asset: Option<VisualAssetRef>| KnowledgeFragment {
            fragment_id: "frag-p1-b0".into(),
            modality: FragmentModality::Diagram,
            context: FragmentContext {
                page: Some(1),
                ..Default::default()
            },
            content: FragmentContent::Diagram(crate::ast::DiagramPayload {
                nodes: vec![crate::ast::DiagramNode {
                    id: "a".into(),
                    label: "Client".into(),
                    node_type: None,
                    bbox: None,
                    confidence: 1.0,
                }],
                edges: Vec::new(),
                asset,
                model: None,
            }),
            source: None,
            evidence: Vec::new(),
            confidence: 1.0,
        };
        let provider = MockEmbeddingProvider::new();
        assert!(
            build_visual_index(&[make(None)], &provider).is_empty(),
            "text-sourced diagram is not a visual object"
        );
        let index = build_visual_index(&[make(Some(asset()))], &provider);
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].asset_id, "asset-1");
        assert_eq!(index[0].semantic_caption, None, "honest: no diagram title");
    }

    #[test]
    fn visual_index_is_deterministic() {
        let fs = fragments(&ast(vec![image_node(), text_node("Some body text.")]));
        let provider = MockEmbeddingProvider::new();
        let first = build_visual_index(&fs, &provider);
        let second = build_visual_index(&fs, &provider);
        assert_eq!(first, second);
    }

    #[test]
    fn mm_builder_fuses_image_bytes_when_loadable() {
        // HLD §23 consumption proof: when asset bytes load, the record
        // embeds the fused TextImage input — the image channel contributes
        // (mock fusion ≠ text-only), unlike the base text-only builder.
        let fs = fragments(&ast(vec![image_node(), text_node("Some body text.")]));
        let mm = crate::multimodal_embedding::MockMultimodalEmbeddingProvider::new();
        let bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let mut loader = |a: &VisualAssetRef| {
            assert_eq!(a.asset_id, "asset-1");
            Some(bytes.clone())
        };
        let index = build_visual_index_with_mm(&fs, &mm, &mut loader);
        assert_eq!(index.len(), 1);
        let expected = mm.embed_multimodal(&MultimodalEmbeddingInput::TextImage {
            text: "Figure 1: payment flow",
            image: &bytes,
        });
        assert_eq!(index[0].embedding, expected);
        assert_ne!(
            index[0].embedding,
            mm.embed_text("Figure 1: payment flow"),
            "image channel must contribute when bytes load"
        );
        assert_eq!(
            index[0].semantic_caption.as_deref(),
            Some("Figure 1: payment flow")
        );
    }

    #[test]
    fn mm_builder_falls_back_to_text_without_bytes() {
        // HLD §23: the architecture works without a multimodal model —
        // no asset bytes → text channel → records identical to the base
        // text-only builder.
        let fs = fragments(&ast(vec![image_node(), text_node("Some body text.")]));
        let mm = crate::multimodal_embedding::MockMultimodalEmbeddingProvider::new();
        let mut loader = |_a: &VisualAssetRef| None;
        let mm_index = build_visual_index_with_mm(&fs, &mm, &mut loader);
        let text_index = build_visual_index(&fs, &MockEmbeddingProvider::new());
        assert_eq!(mm_index, text_index);
    }
}
