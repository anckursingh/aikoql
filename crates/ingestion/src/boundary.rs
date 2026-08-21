//! D4: Knowledge boundary detection — semantic segmentation of the AST.
//!
//! Split from retrieval chunking (HLD §22/§37): the boundary detector owns
//! *semantic* segmentation ("which coherent knowledge units exist?"), the
//! chunker owns *retrieval* packaging ("how do chunks serve a backend?").
//!
//! Production detector: `RuleBoundaryDetector` — hard structural boundaries
//! (heading/table/figure/code/list blocks, page transitions) plus heading-path
//! context. Semantic-similarity scoring (embedding/transformer/hybrid) is
//! deliberately absent until the rule baseline exists to benchmark against
//! (HLD §60).

use crate::ast::{table_payload_from_node, AstNode, BlockType, DocumentAst};
use crate::fragment::{FragmentContent, FragmentContext, FragmentModality, KnowledgeFragment};
use crate::ir::Evidence;
use crate::source::{EvidenceSource, SourceSpan};

/// Splits a DocumentAst into coherent knowledge units.
///
/// Implementations: RuleBoundaryDetector (now), EmbeddingBoundaryDetector /
/// TransformerBoundaryDetector / HybridBoundaryDetector (after baseline
/// metrics — HLD §16).
pub trait KnowledgeBoundaryDetector: Send + Sync {
    fn name(&self) -> &str;

    fn detect(&self, ast: &DocumentAst) -> Result<Vec<KnowledgeFragment>, BoundaryError>;
}

#[derive(Debug)]
pub enum BoundaryError {
    Construction(String),
}

impl std::fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoundaryError::Construction(msg) => {
                write!(f, "fragment construction failed: {}", msg)
            }
        }
    }
}

impl std::error::Error for BoundaryError {}

/// Structural boundary detector: one fragment per top-level block, with
/// headings tracked as context instead of emitted fragments (their text
/// reaches consumers through `FragmentContext.heading_path`).
pub struct RuleBoundaryDetector;

impl KnowledgeBoundaryDetector for RuleBoundaryDetector {
    fn name(&self) -> &str {
        "rule-boundary"
    }

    fn detect(&self, ast: &DocumentAst) -> Result<Vec<KnowledgeFragment>, BoundaryError> {
        let mut fragments: Vec<KnowledgeFragment> = Vec::new();
        let mut heading_path: Vec<String> = Vec::new();

        for (page_idx, page_node) in ast.pages.iter().enumerate() {
            let page = page_idx as u32 + 1;
            for (block_idx, block) in page_node.children.iter().enumerate() {
                emit_block(block, page, block_idx, &mut heading_path, &mut fragments);
            }
            heading_path.clear(); // headings do not cross page boundaries
        }

        // PR-C: document-hash prefix — fragment ids are globally unique
        // across documents (`frag-{hash8}-p{page}-b{block}`), and the
        // context carries the document identity.
        if let Some(doc_id) = ast.document_id.as_deref() {
            let hash8: String = doc_id.chars().take(8).collect();
            if !hash8.is_empty() {
                for frag in &mut fragments {
                    let short = frag.fragment_id.strip_prefix("frag-").unwrap_or("");
                    frag.fragment_id = format!("frag-{}-{}", hash8, short);
                    frag.context.document_id = Some(doc_id.to_string());
                }
            }
        }

        // Neighbor links for context (previous/next fragment ids).
        let ids: Vec<String> = fragments.iter().map(|f| f.fragment_id.clone()).collect();
        for (i, frag) in fragments.iter_mut().enumerate() {
            let mut neighbors = Vec::with_capacity(2);
            if i > 0 {
                neighbors.push(ids[i - 1].clone());
            }
            if i + 1 < ids.len() {
                neighbors.push(ids[i + 1].clone());
            }
            frag.context.neighboring_fragments = neighbors;
        }

        Ok(fragments)
    }
}

fn emit_block(
    node: &AstNode,
    page: u32,
    block_idx: usize,
    heading_path: &mut Vec<String>,
    out: &mut Vec<KnowledgeFragment>,
) {
    match &node.block_type {
        BlockType::Heading { .. } | BlockType::Title => {
            // Headings are structural context, not content fragments.
            heading_path.push(node.text.clone().unwrap_or_default());
        }
        BlockType::Table => {
            // The canonical AST already carries the TablePayload (attached by
            // build_table_node); fall back to conversion for hand-built ASTs.
            let payload = node
                .payload
                .clone()
                .and_then(|p| match p {
                    crate::ast::AstPayload::Table(t) => Some(t),
                    _ => None,
                })
                .or_else(|| table_payload_from_node(node));
            match payload {
                Some(table) => {
                    let confidence = node.confidence.unwrap_or(1.0);
                    out.push(KnowledgeFragment {
                        fragment_id: fragment_id(page, block_idx),
                        modality: FragmentModality::Table,
                        content: FragmentContent::Table(table),
                        context: FragmentContext {
                            heading_path: heading_path.clone(),
                            page: Some(page),
                            ..Default::default()
                        },
                        source: Some(SourceSpan {
                            document_id: None,
                            page,
                            start_offset: None,
                            end_offset: None,
                            bbox: node.bbox.clone(),
                            node_id: node.node_id.clone(),
                        }),
                        evidence: vec![evidence(node, page, confidence)],
                        confidence,
                    });
                }
                None => out.push(text_fragment(node, page, block_idx, heading_path)),
            }
        }
        BlockType::Code => {
            let confidence = node.confidence.unwrap_or(1.0);
            out.push(KnowledgeFragment {
                fragment_id: fragment_id(page, block_idx),
                modality: FragmentModality::Code,
                content: FragmentContent::Code(node.text.clone().unwrap_or_default()),
                context: FragmentContext {
                    heading_path: heading_path.clone(),
                    page: Some(page),
                    ..Default::default()
                },
                source: Some(SourceSpan {
                    document_id: None,
                    page,
                    start_offset: None,
                    end_offset: None,
                    bbox: node.bbox.clone(),
                    node_id: node.node_id.clone(),
                }),
                evidence: vec![evidence(node, page, confidence)],
                confidence,
            });
        }
        BlockType::List { .. } => {
            // Join items for the fragment; the list structure itself stays
            // canonical in the AST.
            let text = node
                .children
                .iter()
                .filter_map(|item| {
                    if item.text.as_deref().unwrap_or_default().trim().is_empty() {
                        None
                    } else {
                        Some(item.text.as_deref().unwrap_or_default().trim().to_string())
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                out.push(text_fragment_with(
                    page,
                    block_idx,
                    heading_path,
                    node,
                    FragmentContent::Text(text),
                    FragmentModality::Text,
                ));
            }
        }
        // PR-F: visual modalities emit typed fragments when classification
        // attached a payload; without one (degraded analyzer) they fall back
        // to Text so the figure marker + caption are never lost.
        BlockType::Chart | BlockType::Diagram | BlockType::Formula | BlockType::Image => {
            let content = node.payload.clone().and_then(|p| match p {
                crate::ast::AstPayload::Chart(c) => Some(FragmentContent::Chart(c)),
                crate::ast::AstPayload::Diagram(d) => Some(FragmentContent::Diagram(d)),
                crate::ast::AstPayload::Formula(f) => Some(FragmentContent::Formula(f)),
                crate::ast::AstPayload::Image(i) => Some(FragmentContent::Image(i)),
                _ => None,
            });
            match content {
                Some(FragmentContent::Chart(c)) => out.push(text_fragment_with(
                    page,
                    block_idx,
                    heading_path,
                    node,
                    FragmentContent::Chart(c),
                    FragmentModality::Chart,
                )),
                Some(FragmentContent::Diagram(d)) => out.push(text_fragment_with(
                    page,
                    block_idx,
                    heading_path,
                    node,
                    FragmentContent::Diagram(d),
                    FragmentModality::Diagram,
                )),
                Some(FragmentContent::Formula(f)) => out.push(text_fragment_with(
                    page,
                    block_idx,
                    heading_path,
                    node,
                    FragmentContent::Formula(f),
                    FragmentModality::Formula,
                )),
                Some(FragmentContent::Image(i)) => out.push(text_fragment_with(
                    page,
                    block_idx,
                    heading_path,
                    node,
                    FragmentContent::Image(i),
                    FragmentModality::Image,
                )),
                _ => out.push(text_fragment(node, page, block_idx, heading_path)),
            }
        }
        _ => {
            // Empty container (Unknown/Section wrappers): recurse so wrapped
            // content is never silently dropped.
            if node.text.as_deref().unwrap_or_default().trim().is_empty()
                && !node.children.is_empty()
            {
                for (child_idx, child) in node.children.iter().enumerate() {
                    emit_block(child, page, child_idx, heading_path, out);
                }
            } else {
                out.push(text_fragment(node, page, block_idx, heading_path));
            }
        }
    }
}

fn text_fragment(
    node: &AstNode,
    page: u32,
    block_idx: usize,
    heading_path: &[String],
) -> KnowledgeFragment {
    text_fragment_with(
        page,
        block_idx,
        heading_path,
        node,
        FragmentContent::Text(node.text.clone().unwrap_or_default()),
        FragmentModality::Text,
    )
}

fn text_fragment_with(
    page: u32,
    block_idx: usize,
    heading_path: &[String],
    node: &AstNode,
    content: FragmentContent,
    modality: FragmentModality,
) -> KnowledgeFragment {
    let confidence = node.confidence.unwrap_or(1.0);
    KnowledgeFragment {
        fragment_id: fragment_id(page, block_idx),
        modality,
        content,
        context: FragmentContext {
            heading_path: heading_path.to_vec(),
            page: Some(page),
            ..Default::default()
        },
        source: Some(SourceSpan {
            document_id: None,
            page,
            start_offset: None,
            end_offset: None,
            bbox: node.bbox.clone(),
            node_id: node.node_id.clone(),
        }),
        evidence: vec![evidence(node, page, confidence)],
        confidence,
    }
}

/// Deterministic fragment id from position. ponytail: no document-hash
/// prefix until DocumentAst carries a document_id (PR-B) — position alone
/// is stable for a given document layout.
fn fragment_id(page: u32, block_idx: usize) -> String {
    format!("frag-p{}-b{}", page, block_idx)
}

fn evidence(node: &AstNode, page: u32, confidence: f32) -> Evidence {
    Evidence {
        document_id: None,
        page: Some(page),
        source: node
            .bbox
            .as_ref()
            .map(|b| EvidenceSource::Region { bbox: b.clone() }),
        extractor: "rule_boundary".into(),
        model: None,
        confidence,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual::DiagramAnalyzer;
    use crate::{document_model_to_ast, DocumentModel, PageModel};

    fn doc(pages: Vec<&str>) -> DocumentModel {
        let pages: Vec<PageModel> = pages
            .iter()
            .map(|t| PageModel {
                page_number: 1,
                text: t.to_string(),
                char_count: t.len(),
                source: "native".into(),
                ocr_confidence: None,
                images: vec![],
            })
            .collect();
        DocumentModel {
            page_count: pages.len() as u32,
            total_chars: pages.iter().map(|p| p.char_count).sum(),
            pages,
            ocr_stats: None,
        }
    }

    #[test]
    fn detect_structural_boundaries() {
        let dm = doc(vec![
            "1. Payment Terms\n\nPayment is due within 30 days.\n\n| Item | Qty |\n| Widget | 10 |\n| Gadget | 5 |\n\n    let x = 1;\n    let y = 2;",
        ]);
        let ast = document_model_to_ast(&dm);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();

        let kinds: Vec<FragmentModality> = fragments.iter().map(|f| f.modality.clone()).collect();
        assert!(
            kinds.contains(&FragmentModality::Text),
            "paragraph fragment"
        );
        assert!(kinds.contains(&FragmentModality::Table), "table fragment");
        assert!(kinds.contains(&FragmentModality::Code), "code fragment");
    }

    #[test]
    fn headings_become_context_not_fragments() {
        let dm = doc(vec![
            "1. Billing\n\nPayment is due within 30 days of invoice date.",
        ]);
        let ast = document_model_to_ast(&dm);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].modality, FragmentModality::Text);
        assert!(fragments[0]
            .context
            .heading_path
            .iter()
            .any(|h| h.contains("Billing")));
    }

    #[test]
    fn table_fragment_preserves_structure() {
        let dm = doc(vec!["| Name | Age |\n| Alice | 30 |\n| Bob | 25 |"]);
        let ast = document_model_to_ast(&dm);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();

        let table = fragments
            .iter()
            .find(|f| f.modality == FragmentModality::Table)
            .expect("table fragment");
        match &table.content {
            FragmentContent::Table(payload) => {
                assert_eq!(payload.headers.len(), 2);
                assert_eq!(payload.headers[0].text, "Name");
                assert_eq!(payload.rows.len(), 2);
                assert_eq!(payload.cells.len(), 4);
                assert_eq!(payload.cells[1].text, "30");
                assert_eq!(payload.cells[1].column_id, "h1");
            }
            other => panic!("expected table content, got {:?}", other),
        }
    }

    #[test]
    fn fragments_have_provenance_and_deterministic_ids() {
        let dm = doc(vec!["Paragraph one.\n\nParagraph two."]);
        let ast = document_model_to_ast(&dm);
        let first = RuleBoundaryDetector.detect(&ast).unwrap();
        let second = RuleBoundaryDetector.detect(&ast).unwrap();

        assert_eq!(first.len(), 2);
        for f in &first {
            assert!(!f.evidence.is_empty(), "evidence on {}", f.fragment_id);
            assert_eq!(f.evidence[0].page, Some(1));
            assert!(f.source.is_some(), "typed source on {}", f.fragment_id);
        }
        let ids: Vec<&str> = first.iter().map(|f| f.fragment_id.as_str()).collect();
        let ids2: Vec<&str> = second.iter().map(|f| f.fragment_id.as_str()).collect();
        assert_eq!(ids, ids2, "fragment ids must be deterministic");
        assert_eq!(
            first[0].context.neighboring_fragments,
            vec![first[1].fragment_id.clone()]
        );
    }

    #[test]
    fn fragment_ids_carry_document_hash_prefix() {
        let dm = doc(vec!["Paragraph one.\n\nParagraph two."]);
        let mut ast = document_model_to_ast(&dm);
        ast.document_id = Some("0123456789abcdef".into());
        let frags = RuleBoundaryDetector.detect(&ast).unwrap();

        assert_eq!(frags.len(), 2);
        assert_eq!(frags[0].fragment_id, "frag-01234567-p1-b0");
        assert_eq!(frags[1].fragment_id, "frag-01234567-p1-b1");
        for f in &frags {
            assert_eq!(f.context.document_id.as_deref(), Some("0123456789abcdef"));
        }
        // Neighbor links use the prefixed ids.
        assert_eq!(
            frags[0].context.neighboring_fragments,
            vec!["frag-01234567-p1-b1".to_string()]
        );
        // Deterministic across runs.
        let again = RuleBoundaryDetector.detect(&ast).unwrap();
        assert_eq!(frags[0].fragment_id, again[0].fragment_id);
    }

    #[test]
    fn empty_document_yields_no_fragments() {
        let dm = doc(vec![]);
        let ast = document_model_to_ast(&dm);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();
        assert!(fragments.is_empty());
    }

    #[test]
    fn visual_node_with_payload_emits_typed_fragment() {
        // PR-F: a Diagram node carrying a payload emits a Diagram fragment
        // (HLD §11: visual structure reaches the semantic leg typed).
        let mut node = AstNode {
            block_type: BlockType::Diagram,
            text: Some("Client -> Gateway".into()),
            children: vec![],
            bbox: None,
            confidence: None,
            ..Default::default()
        };
        node.payload = crate::visual::MockDiagramAnalyzer
            .analyze(&node)
            .map(crate::ast::AstPayload::Diagram);
        let page_node = AstNode {
            block_type: BlockType::Unknown,
            text: None,
            children: vec![node],
            bbox: None,
            confidence: None,
            ..Default::default()
        };
        let ast = DocumentAst {
            page_count: 1,
            pages: vec![page_node],
            source_type: "test".into(),
            document_id: None,
        };

        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].modality, FragmentModality::Diagram);
        assert!(matches!(fragments[0].content, FragmentContent::Diagram(_)));
    }

    #[test]
    fn visual_node_without_payload_falls_back_to_text() {
        // Degraded analyzer (no payload) must never lose content: the
        // figure marker + caption stay as a text fragment.
        let page_node = AstNode {
            block_type: BlockType::Unknown,
            text: None,
            children: vec![AstNode {
                block_type: BlockType::Figure,
                text: Some("Figure 1: fees".into()),
                children: vec![],
                bbox: None,
                confidence: None,
                ..Default::default()
            }],
            bbox: None,
            confidence: None,
            ..Default::default()
        };
        let ast = DocumentAst {
            page_count: 1,
            pages: vec![page_node],
            source_type: "test".into(),
            document_id: None,
        };

        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].modality, FragmentModality::Text);
        match &fragments[0].content {
            FragmentContent::Text(t) => assert!(t.contains("Figure 1: fees")),
            other => panic!("expected text fallback, got {:?}", other),
        }
    }

    #[test]
    fn fragment_serde_roundtrip() {
        let dm = doc(vec!["| A | B |\n| 1 | 2 |"]);
        let ast = document_model_to_ast(&dm);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();
        let json = serde_json::to_string(&fragments).unwrap();
        let back: Vec<KnowledgeFragment> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), fragments.len());
        assert_eq!(back[0].fragment_id, fragments[0].fragment_id);
    }
}
