//! D8: Retrieval projection — packaging knowledge fragments for vector search.
//!
//! PR-E (HLD §41/§60): chunking is a *projection* of the canonical
//! `KnowledgeFragment` stream, never a second source of truth. The
//! `KnowledgeBoundaryDetector` owns semantic segmentation; the projector only
//! packages fragments into retrieval-sized chunks with structural metadata.
//!
//! # Architecture
//! - `DocumentChunk` — retrieval unit with structural metadata
//! - `EmbeddedChunk` — chunk + embedding vector, ready for the vector engine
//! - `ChunkingStrategy` — packaging policy (how much text per chunk)
//! - `RetrievalProjector` trait — pluggable projection
//! - `HeadingProjector` — heading-aware projection with overlap
//!
//! # Atomicity invariant
//! A chunk may *group* fragments but never *split* one: a table fragment stays
//! whole in one chunk even when it exceeds `max_chunk_chars`. Retrieval must
//! never hand a consumer half a table.

use crate::ast::{ChartPayload, DiagramPayload, FormulaPayload, ImagePayload, TablePayload};
use crate::embedding::EmbeddingProvider;
use crate::fragment::{FragmentContent, KnowledgeFragment};
use crate::ir::{Evidence, KnowledgeIr};

// ---------------------------------------------------------------------------
// Document chunk
// ---------------------------------------------------------------------------

/// A semantic segment of document text with structural metadata.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DocumentChunk {
    /// Unique chunk ID within the document (e.g. "doc-001/chunk-003").
    pub chunk_id: String,
    /// The chunk's text content.
    pub text: String,
    /// Character length of the text.
    pub char_count: usize,
    /// Position metadata.
    pub position: ChunkPosition,
    /// Structural metadata.
    pub structure: ChunkStructure,
    /// Entities mentioned in this chunk (from KnowledgeIr).
    pub entity_mentions: Vec<String>,
    /// Provenance evidence of the fragments projected into this chunk.
    pub evidence: Vec<Evidence>,
}

/// Where this chunk sits in the document.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChunkPosition {
    /// 0-based chunk index within the document.
    pub chunk_index: usize,
    /// Page number (1-based) where the chunk starts.
    pub start_page: u32,
    /// Page number where the chunk ends (same as start_page for single-page chunks).
    pub end_page: u32,
    /// Character offset from the start of the document.
    pub char_offset: usize,
}

/// Structural context for a chunk.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChunkStructure {
    /// Breadcrumb of headings leading to this chunk (e.g. ["Chapter 1", "Introduction"]).
    pub heading_path: Vec<String>,
    /// The fragment modality that generated this chunk (e.g. "text", "table"),
    /// or "mixed" when fragments of different modalities share a chunk.
    pub source_type: String,
    /// The section or heading text immediately above this chunk.
    pub section_title: Option<String>,
    /// Number of this chunk within its section (1-based).
    pub section_chunk_index: usize,
}

/// A chunk with its embedding vector, ready for vector engine ingestion.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddedChunk {
    /// The source chunk.
    pub chunk: DocumentChunk,
    /// The embedding vector.
    pub embedding: Vec<f32>,
    /// Name of the embedding provider used.
    pub embedding_provider: String,
}

// ---------------------------------------------------------------------------
// Chunking strategy
// ---------------------------------------------------------------------------

/// How to package fragments into chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkingStrategy {
    /// Group fragments under a heading into chunks. Fragments are packed
    /// greedily up to `max_chunk_chars`; an oversized fragment stays atomic.
    Heading {
        /// Maximum characters per chunk before starting a new one.
        max_chunk_chars: usize,
        /// Character overlap between adjacent chunks for context continuity.
        overlap_chars: usize,
    },
    /// One chunk per fragment group under a heading, packing small adjacent
    /// fragments up to `max_chunk_chars`; never splits a fragment.
    Paragraph {
        /// Maximum characters per chunk.
        max_chunk_chars: usize,
        /// Minimum characters for a standalone chunk.
        min_chunk_chars: usize,
    },
    /// Fixed-size sliding window over the flattened fragment stream.
    /// Splits at fragment boundaries (never mid-fragment).
    FixedWindow {
        /// Window size in characters.
        window_chars: usize,
        /// Overlap between consecutive windows.
        overlap_chars: usize,
    },
}

impl Default for ChunkingStrategy {
    fn default() -> Self {
        ChunkingStrategy::Heading {
            max_chunk_chars: 4096,
            overlap_chars: 200,
        }
    }
}

// ---------------------------------------------------------------------------
// RetrievalProjector trait
// ---------------------------------------------------------------------------

/// Pluggable retrieval projection: KnowledgeFragment[] → Vec<DocumentChunk>.
///
/// Contract (HLD §59): the projector derives chunks from canonical fragments.
/// It must never split a fragment, and must never invent content the fragment
/// stream doesn't carry (heading text in a chunk is the heading_path echoed
/// from fragment context).
pub trait RetrievalProjector: Send + Sync {
    /// Human-readable name (e.g. "heading-projection").
    fn name(&self) -> &str;

    /// Current chunking strategy.
    fn strategy(&self) -> &ChunkingStrategy;

    /// Project fragments into retrieval chunks.
    /// The optional `ir` provides entity mentions for enrichment.
    fn project(
        &self,
        fragments: &[KnowledgeFragment],
        ir: Option<&KnowledgeIr>,
    ) -> Vec<DocumentChunk>;
}

// ---------------------------------------------------------------------------
// Heading projector — heading-based projection
// ---------------------------------------------------------------------------

/// Heading-aware retrieval projection.
///
/// Strategy:
/// - Groups fragments into sections by (page, heading_path).
/// - Packs each section's fragments greedily into chunks up to
///   `max_chunk_chars`; a fragment larger than the limit becomes its own
///   atomic chunk.
/// - Adjacent chunks overlap by `overlap_chars` characters (text-level tail
///   carry; fragment boundaries are still respected).
/// - Entity mentions from KnowledgeIr are attached to chunks.
pub struct HeadingProjector {
    strategy: ChunkingStrategy,
}

impl Default for HeadingProjector {
    fn default() -> Self {
        Self::new()
    }
}

impl HeadingProjector {
    pub fn new() -> Self {
        HeadingProjector {
            strategy: ChunkingStrategy::default(),
        }
    }

    pub fn with_strategy(strategy: ChunkingStrategy) -> Self {
        HeadingProjector { strategy }
    }
}

impl RetrievalProjector for HeadingProjector {
    fn name(&self) -> &str {
        "heading-projection"
    }

    fn strategy(&self) -> &ChunkingStrategy {
        &self.strategy
    }

    fn project(
        &self,
        fragments: &[KnowledgeFragment],
        ir: Option<&KnowledgeIr>,
    ) -> Vec<DocumentChunk> {
        let (max_chunk, overlap) = match &self.strategy {
            ChunkingStrategy::Heading {
                max_chunk_chars,
                overlap_chars,
            } => (*max_chunk_chars, *overlap_chars),
            ChunkingStrategy::Paragraph {
                max_chunk_chars,
                min_chunk_chars: _,
            } => (*max_chunk_chars, 0),
            ChunkingStrategy::FixedWindow {
                window_chars,
                overlap_chars,
            } => (*window_chars, *overlap_chars),
        };

        let document_id = ir
            .and_then(|i| i.document_id.as_deref())
            .unwrap_or("unknown");

        let entity_map = build_entity_map(ir);

        let mut chunks: Vec<DocumentChunk> = Vec::new();
        let mut char_offset: usize = 0;

        for section in collect_sections(fragments) {
            let mut pending: Vec<&KnowledgeFragment> = Vec::new();
            let mut pending_len: usize = 0;
            let mut section_chunk_idx = 0usize;
            // Overlap carry from the previous chunk in this section — a text
            // tail for context continuity, reset per section (matches the
            // pre-PR-E behavior of no cross-section overlap).
            let mut carry: String = String::new();

            for frag in section.fragments.iter() {
                let text = fragment_text(frag);
                if text.trim().is_empty() {
                    continue;
                }
                // Atomicity: an oversized fragment is its own chunk.
                if !pending.is_empty() && pending_len + text.len() + 1 > max_chunk {
                    let chunk_text = emit_chunk(
                        &pending,
                        &carry,
                        &section,
                        section_chunk_idx,
                        &mut chunks,
                        document_id,
                        &mut char_offset,
                        &entity_map,
                    );
                    section_chunk_idx += 1;
                    pending.clear();
                    pending_len = 0;
                    if overlap > 0 && chunk_text.len() > overlap {
                        carry = chunk_text[chunk_text.len() - overlap..].to_string();
                    }
                }
                pending.push(frag);
                pending_len += text.len() + 1;
            }

            if !pending.is_empty() {
                emit_chunk(
                    &pending,
                    &carry,
                    &section,
                    section_chunk_idx,
                    &mut chunks,
                    document_id,
                    &mut char_offset,
                    &entity_map,
                );
            }
        }

        chunks
    }
}

/// A logical section: fragments sharing (page, heading_path), in doc order.
#[derive(Clone, Debug)]
struct Section<'a> {
    page: u32,
    heading_path: Vec<String>,
    fragments: Vec<&'a KnowledgeFragment>,
}

/// Group fragments into sections. A section boundary is a page change or a
/// heading_path change; both come from the boundary detector.
fn collect_sections(fragments: &[KnowledgeFragment]) -> Vec<Section<'_>> {
    let mut sections: Vec<Section<'_>> = Vec::new();

    for frag in fragments {
        let page = frag
            .source
            .as_ref()
            .map(|s| s.page)
            .or(frag.context.page)
            .unwrap_or(1);
        let heading_path = frag.context.heading_path.clone();

        let starts_new = match sections.last() {
            Some(last) => last.page != page || last.heading_path != heading_path,
            None => true,
        };
        if starts_new {
            sections.push(Section {
                page,
                heading_path,
                fragments: Vec::new(),
            });
        }
        sections.last_mut().unwrap().fragments.push(frag);
    }

    sections
}

/// Render a fragment's canonical content into retrieval text.
///
/// Tables render as pipe-delimited rows (all cell text preserved), visuals
/// render their textual representations. A generated projection, never the
/// canonical content — the canonical structure stays in the fragment.
fn fragment_text(frag: &KnowledgeFragment) -> String {
    match &frag.content {
        FragmentContent::Text(s) | FragmentContent::Code(s) => s.clone(),
        FragmentContent::Table(table) => render_table(table),
        FragmentContent::Image(image) => render_image(image),
        FragmentContent::Chart(chart) => render_chart(chart),
        FragmentContent::Diagram(diagram) => render_diagram(diagram),
        FragmentContent::Formula(formula) => render_formula(formula),
        FragmentContent::Mixed(children) => children
            .iter()
            .map(|child| fragment_text(child))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn render_table(table: &TablePayload) -> String {
    let header = table
        .headers
        .iter()
        .map(|h| h.text.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    let mut lines = vec![header];
    for row in &table.rows {
        let cells = table
            .headers
            .iter()
            .map(|h| {
                table
                    .cells
                    .iter()
                    .find(|c| c.row_id == row.id && c.column_id == h.id)
                    .map(|c| c.text.as_str())
                    .unwrap_or("")
            })
            .collect::<Vec<_>>()
            .join(" | ");
        lines.push(cells);
    }
    lines.join("\n")
}

fn render_image(image: &ImagePayload) -> String {
    match (&image.caption, &image.ocr_text) {
        (Some(caption), Some(ocr)) => format!("{}\n{}", caption, ocr),
        (Some(caption), None) => caption.clone(),
        (None, Some(ocr)) => ocr.clone(),
        (None, None) => String::new(),
    }
}

fn render_chart(chart: &ChartPayload) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(title) = &chart.title {
        parts.push(title.clone());
    }
    for axis in [&chart.x_axis, &chart.y_axis].into_iter().flatten() {
        if let Some(label) = &axis.label {
            parts.push(label.clone());
        }
    }
    for series in &chart.series {
        parts.push(series.name.clone());
    }
    parts.join("\n")
}

fn render_diagram(diagram: &DiagramPayload) -> String {
    let mut parts: Vec<String> = diagram.nodes.iter().map(|n| n.label.clone()).collect();
    parts.extend(
        diagram
            .edges
            .iter()
            .filter_map(|e| e.label.clone())
            .collect::<Vec<_>>(),
    );
    parts.join("\n")
}

fn render_formula(formula: &FormulaPayload) -> String {
    formula
        .latex
        .clone()
        .or_else(|| formula.plain_text.clone())
        .unwrap_or_default()
}

/// Assemble + push one chunk for the given fragment set. Returns the chunk
/// text so the caller can derive the next overlap carry.
#[allow(clippy::too_many_arguments)] // projection assembly — one call site
fn emit_chunk(
    pending: &[&KnowledgeFragment],
    carry: &str,
    section: &Section<'_>,
    section_chunk_idx: usize,
    chunks: &mut Vec<DocumentChunk>,
    document_id: &str,
    char_offset: &mut usize,
    entity_map: &std::collections::HashMap<String, Vec<String>>,
) -> String {
    let text = assemble_text(
        pending,
        carry,
        &section.heading_path,
        section_chunk_idx == 0,
    );
    let chunk = make_chunk(
        &text,
        document_id,
        chunks.len(),
        section.page,
        pending,
        section,
        section_chunk_idx,
        char_offset,
        entity_map,
    );
    chunks.push(chunk);
    text
}

/// Assemble a chunk's text from whole fragments, plus the overlap carry and
/// (on the first chunk of a section) the heading path so retrieval matches
/// heading terms. Later chunks inherit context via the overlap carry.
fn assemble_text(
    pending: &[&KnowledgeFragment],
    carry: &str,
    heading_path: &[String],
    with_heading: bool,
) -> String {
    let mut text = String::new();
    if !carry.is_empty() {
        text.push_str(carry);
        text.push('\n');
    }
    if with_heading && !heading_path.is_empty() {
        text.push_str(&heading_path.join("\n"));
        text.push('\n');
    }
    for (i, frag) in pending.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        text.push_str(&fragment_text(frag));
    }
    text
}

/// Build one DocumentChunk from the given fragment set.
#[allow(clippy::too_many_arguments)] // projection assembly — one call site
fn make_chunk(
    text: &str,
    document_id: &str,
    chunk_index: usize,
    page: u32,
    frags: &[&KnowledgeFragment],
    section: &Section<'_>,
    section_chunk_index: usize,
    char_offset: &mut usize,
    entity_map: &std::collections::HashMap<String, Vec<String>>,
) -> DocumentChunk {
    let chunk_id = format!("{}/chunk-{:04}", document_id, chunk_index);

    // Modality label: the single modality when all fragments agree, else mixed.
    let mut modalities = frags
        .iter()
        .map(|f| modality_name(&f.modality))
        .collect::<Vec<_>>();
    modalities.dedup();
    let source_type = if modalities.len() == 1 {
        modalities[0].to_string()
    } else {
        "mixed".to_string()
    };

    let entities = find_entity_mentions(text, entity_map);
    let evidence = frags.iter().flat_map(|f| f.evidence.clone()).collect();

    let chunk = DocumentChunk {
        chunk_id,
        text: text.to_string(),
        char_count: text.len(),
        position: ChunkPosition {
            chunk_index,
            start_page: page,
            end_page: page,
            char_offset: *char_offset,
        },
        structure: ChunkStructure {
            heading_path: section.heading_path.clone(),
            source_type,
            section_title: section.heading_path.last().cloned(),
            section_chunk_index,
        },
        entity_mentions: entities,
        evidence,
    };
    *char_offset += text.len();
    chunk
}

fn modality_name(m: &crate::fragment::FragmentModality) -> &'static str {
    use crate::fragment::FragmentModality as M;
    match m {
        M::Text => "text",
        M::Table => "table",
        M::Image => "image",
        M::Chart => "chart",
        M::Diagram => "diagram",
        M::Formula => "formula",
        M::Code => "code",
        M::Mixed => "mixed",
    }
}

// ---------------------------------------------------------------------------
// Entity enrichment
// ---------------------------------------------------------------------------

/// Build a map from entity mention text → entity names for quick lookup.
fn build_entity_map(ir: Option<&KnowledgeIr>) -> std::collections::HashMap<String, Vec<String>> {
    let mut map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    if let Some(ir) = ir {
        for entity in &ir.entities {
            for mention in &entity.mentions {
                map.entry(mention.to_lowercase())
                    .or_default()
                    .push(entity.name.clone());
            }
        }
    }
    map
}

/// Find which entities are mentioned in a chunk of text.
fn find_entity_mentions(
    text: &str,
    entity_map: &std::collections::HashMap<String, Vec<String>>,
) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut found: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (mention, names) in entity_map {
        if lower.contains(mention.as_str()) {
            for name in names {
                found.insert(name.clone());
            }
        }
    }
    let mut result: Vec<String> = found.into_iter().collect();
    result.sort();
    result
}

// ---------------------------------------------------------------------------
// Embedding bridge
// ---------------------------------------------------------------------------

/// Embed a set of document chunks using the given provider.
/// Returns chunks with embedding vectors attached, ready for vector engine ingestion.
pub fn embed_chunks(
    chunks: &[DocumentChunk],
    provider: &dyn EmbeddingProvider,
) -> Vec<EmbeddedChunk> {
    chunks
        .iter()
        .map(|chunk| {
            let embedding = provider.embed(&chunk.text);
            EmbeddedChunk {
                chunk: chunk.clone(),
                embedding,
                embedding_provider: provider.name().to_string(),
            }
        })
        .collect()
}

/// Convenience: project + embed in one call.
pub fn project_and_embed(
    fragments: &[KnowledgeFragment],
    ir: Option<&KnowledgeIr>,
    projector: &dyn RetrievalProjector,
    provider: &dyn EmbeddingProvider,
) -> Vec<EmbeddedChunk> {
    let chunks = projector.project(fragments, ir);
    embed_chunks(&chunks, provider)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{AstNode, BlockType, DocumentAst};
    use crate::boundary::{KnowledgeBoundaryDetector, RuleBoundaryDetector};
    use crate::embedding::MockEmbeddingProvider;
    use crate::ir::EntityCandidate;

    fn make_ast(pages: Vec<Vec<AstNode>>) -> DocumentAst {
        let page_count = pages.len() as u32;
        let pages: Vec<AstNode> = pages
            .into_iter()
            .map(|children| AstNode {
                block_type: BlockType::Unknown,
                text: String::new(),
                children,
                bbox: None,
                confidence: None,
                ..Default::default()
            })
            .collect();
        DocumentAst {
            pages,
            page_count,
            source_type: "native".into(),
        }
    }

    /// Real PR-E flow: AST → boundary detector → fragments (what the
    /// projector consumes in production).
    fn fragments(ast: &DocumentAst) -> Vec<KnowledgeFragment> {
        RuleBoundaryDetector
            .detect(ast)
            .expect("boundary detection")
    }

    fn paragraph(text: &str) -> AstNode {
        AstNode {
            block_type: BlockType::Paragraph,
            text: text.to_string(),
            children: vec![],
            bbox: None,
            confidence: None,
            ..Default::default()
        }
    }

    fn heading(level: u8, text: &str) -> AstNode {
        AstNode {
            block_type: BlockType::Heading { level },
            text: text.to_string(),
            children: vec![],
            bbox: None,
            confidence: None,
            ..Default::default()
        }
    }

    fn entity(name: &str, mentions: Vec<&str>) -> EntityCandidate {
        EntityCandidate {
            name: name.into(),
            type_hint: None,
            mentions: mentions.into_iter().map(|s| s.into()).collect(),
            confidence: 0.85,
            evidence: Evidence::default(),
        }
    }

    // ── Basic projection ──

    #[test]
    fn projector_has_name() {
        let p = HeadingProjector::new();
        assert_eq!(p.name(), "heading-projection");
    }

    #[test]
    fn single_paragraph_becomes_one_chunk() {
        let ast = make_ast(vec![vec![paragraph("This is a single paragraph of text.")]]);
        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("single paragraph"));
        assert_eq!(chunks[0].position.chunk_index, 0);
        assert_eq!(chunks[0].position.start_page, 1);
    }

    #[test]
    fn heading_splits_into_sections() {
        let ast = make_ast(vec![vec![
            heading(1, "Introduction"),
            paragraph("Welcome to the report."),
            heading(1, "Methods"),
            paragraph("We used the following methods."),
        ]]);

        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);

        assert!(chunks.len() >= 2);
        // First chunk should contain Introduction + its paragraph.
        let intro = chunks
            .iter()
            .find(|c| c.text.contains("Introduction"))
            .unwrap();
        assert!(intro.text.contains("Welcome to the report"));
        assert_eq!(
            intro.structure.section_title.as_deref(),
            Some("Introduction")
        );
    }

    #[test]
    fn heading_path_is_tracked() {
        let ast = make_ast(vec![vec![
            heading(1, "Chapter 1"),
            heading(2, "Overview"),
            paragraph("An overview of the topic."),
        ]]);

        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);

        let overview = chunks.iter().find(|c| c.text.contains("Overview")).unwrap();
        assert!(overview
            .structure
            .heading_path
            .contains(&"Chapter 1".to_string()));
        assert!(overview
            .structure
            .heading_path
            .contains(&"Overview".to_string()));
    }

    // ── Large section splitting (fragment-atomic) ──

    #[test]
    fn large_section_is_split() {
        // Create a section with many paragraphs exceeding max_chunk_chars.
        let long_para = "Lorem ipsum dolor sit amet. ".repeat(100); // ~2800 chars
        let mut nodes = vec![heading(1, "Long Section")];
        for _ in 0..5 {
            nodes.push(paragraph(&long_para));
        }

        let ast = make_ast(vec![vec![AstNode {
            block_type: BlockType::Unknown,
            text: String::new(),
            children: nodes,
            bbox: None,
            confidence: None,
            ..Default::default()
        }]]);

        // Use a small max_chunk_chars to force splits.
        let projector = HeadingProjector::with_strategy(ChunkingStrategy::Heading {
            max_chunk_chars: 5000,
            overlap_chars: 100,
        });
        let chunks = projector.project(&fragments(&ast), None);

        // Should produce multiple chunks for the long section.
        assert!(
            chunks.len() >= 2,
            "expected multiple chunks, got {}",
            chunks.len()
        );
        // All chunks should share the same heading path.
        for chunk in &chunks {
            assert!(chunk
                .structure
                .heading_path
                .contains(&"Long Section".to_string()));
        }
    }

    #[test]
    fn oversized_table_fragment_stays_atomic() {
        // A table larger than max_chunk_chars must be its own chunk — never
        // split across chunks (HLD §59: no half a table in retrieval).
        let mut rows = String::from("| Column A | Column B |\n");
        for i in 0..40 {
            rows.push_str(&format!("| row {} alpha | row {} beta |\n", i, i));
        }
        let nodes = vec![
            heading(1, "Data"),
            paragraph("Short intro."),
            AstNode {
                block_type: BlockType::Table,
                text: String::new(),
                children: table_children(&rows),
                bbox: None,
                confidence: None,
                ..Default::default()
            },
        ];
        let ast = make_ast(vec![vec![AstNode {
            block_type: BlockType::Unknown,
            text: String::new(),
            children: nodes,
            bbox: None,
            confidence: None,
            ..Default::default()
        }]]);

        let projector = HeadingProjector::with_strategy(ChunkingStrategy::Heading {
            max_chunk_chars: 100,
            overlap_chars: 20,
        });
        let chunks = projector.project(&fragments(&ast), None);

        let table_chunk = chunks
            .iter()
            .find(|c| c.structure.source_type == "table")
            .expect("one atomic table chunk");
        assert_eq!(table_chunk.structure.heading_path, vec!["Data".to_string()]);
        assert!(
            table_chunk.text.contains("row 0 alpha") && table_chunk.text.contains("row 39 beta"),
            "table chunk contains the whole table"
        );
        // No other chunk carries table content.
        let others: Vec<&DocumentChunk> = chunks
            .iter()
            .filter(|c| c.structure.source_type != "table")
            .collect();
        for other in others {
            assert!(
                !other.text.contains("row 0 alpha") && !other.text.contains("row 39 beta"),
                "no chunk may contain a partial table"
            );
        }
    }

    /// Build table-row children the way `build_table_node` does (first row
    /// becomes headers, remaining rows become cells).
    fn table_children(rows_text: &str) -> Vec<AstNode> {
        let mut lines: Vec<&str> = rows_text.lines().filter(|l| !l.trim().is_empty()).collect();
        let header_cells: Vec<String> = lines
            .remove(0)
            .split('|')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let mut children = vec![AstNode {
            block_type: BlockType::TableRow,
            text: String::new(),
            children: header_cells
                .iter()
                .map(|c| AstNode {
                    block_type: BlockType::TableCell {
                        row_span: 1,
                        col_span: 1,
                    },
                    text: c.clone(),
                    children: vec![],
                    bbox: None,
                    confidence: None,
                    ..Default::default()
                })
                .collect(),
            bbox: None,
            confidence: None,
            ..Default::default()
        }];
        for line in &lines {
            let cells: Vec<String> = line
                .split('|')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            children.push(AstNode {
                block_type: BlockType::TableRow,
                text: String::new(),
                children: cells
                    .iter()
                    .map(|c| AstNode {
                        block_type: BlockType::TableCell {
                            row_span: 1,
                            col_span: 1,
                        },
                        text: c.clone(),
                        children: vec![],
                        bbox: None,
                        confidence: None,
                        ..Default::default()
                    })
                    .collect(),
                bbox: None,
                confidence: None,
                ..Default::default()
            });
        }
        children
    }

    // ── Entity enrichment ──

    #[test]
    fn entity_mentions_are_attached_to_chunks() {
        let ast = make_ast(vec![vec![paragraph(
            "Acme Corporation announced record profits. Globex Industries also grew.",
        )]]);

        let ir = KnowledgeIr {
            entities: vec![
                entity("Acme Corporation", vec!["Acme Corporation"]),
                entity("Globex Industries", vec!["Globex Industries"]),
            ],
            ..Default::default()
        };

        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), Some(&ir));

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0]
            .entity_mentions
            .contains(&"Acme Corporation".to_string()));
        assert!(chunks[0]
            .entity_mentions
            .contains(&"Globex Industries".to_string()));
    }

    #[test]
    fn entity_enrichment_handles_missing_ir() {
        let ast = make_ast(vec![vec![paragraph("Acme Corporation text.")]]);
        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].entity_mentions.is_empty());
    }

    // ── Embedding bridge ──

    #[test]
    fn embed_chunks_produces_vectors() {
        let ast = make_ast(vec![vec![paragraph("Short text chunk.")]]);
        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);
        let provider = MockEmbeddingProvider::new();

        let embedded = embed_chunks(&chunks, &provider);
        assert_eq!(embedded.len(), 1);
        assert_eq!(embedded[0].embedding.len(), provider.dimensions());
        assert_eq!(embedded[0].embedding_provider, "mock-char-ngram");
    }

    #[test]
    fn project_and_embed_convenience() {
        let ast = make_ast(vec![vec![paragraph("Quick brown fox.")]]);
        let projector = HeadingProjector::new();
        let provider = MockEmbeddingProvider::new();

        let embedded = project_and_embed(&fragments(&ast), None, &projector, &provider);
        assert!(!embedded.is_empty());
        assert!(!embedded[0].embedding.is_empty());
    }

    #[test]
    fn embedding_is_normalized() {
        let ast = make_ast(vec![vec![paragraph("Test chunk for embedding.")]]);
        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);
        let provider = MockEmbeddingProvider::new();

        let embedded = embed_chunks(&chunks, &provider);
        let norm: f32 = embedded[0]
            .embedding
            .iter()
            .map(|x| x * x)
            .sum::<f32>()
            .sqrt();
        assert!(
            (norm - 1.0).abs() < 0.001,
            "norm should be 1.0, got {}",
            norm
        );
    }

    // ── Chunk ID uniqueness ──

    #[test]
    fn chunk_ids_are_unique() {
        let ast = make_ast(vec![vec![
            heading(1, "A"),
            paragraph("Text A."),
            heading(1, "B"),
            paragraph("Text B."),
            heading(1, "C"),
            paragraph("Text C."),
        ]]);

        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);

        let ids: std::collections::HashSet<&str> =
            chunks.iter().map(|c| c.chunk_id.as_str()).collect();
        assert_eq!(ids.len(), chunks.len(), "all chunk IDs must be unique");
    }

    // ── Empty document ──

    #[test]
    fn empty_document_produces_no_chunks() {
        let ast = make_ast(vec![]);
        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);
        assert!(chunks.is_empty());
    }

    #[test]
    fn whitespace_only_nodes_produce_no_chunks() {
        let ast = make_ast(vec![vec![paragraph("   "), paragraph("\n\n")]]);
        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);
        assert!(chunks.is_empty());
    }

    // ── Paragraph strategy ──

    #[test]
    fn paragraph_strategy_creates_chunks() {
        let ast = make_ast(vec![vec![
            paragraph("First paragraph of the document."),
            paragraph("Second paragraph with different content."),
        ]]);

        let projector = HeadingProjector::with_strategy(ChunkingStrategy::Paragraph {
            max_chunk_chars: 4096,
            min_chunk_chars: 10,
        });
        let chunks = projector.project(&fragments(&ast), None);

        assert!(!chunks.is_empty());
    }

    // ── Fixed window strategy ──

    #[test]
    fn fixed_window_strategy_with_overlap() {
        let text = "ABCDEFGHIJKLMNOPQRSTUVWXYZ".repeat(10); // 260 chars
        let ast = make_ast(vec![vec![paragraph(&text)]]);

        let projector = HeadingProjector::with_strategy(ChunkingStrategy::FixedWindow {
            window_chars: 100,
            overlap_chars: 20,
        });
        let chunks = projector.project(&fragments(&ast), None);

        // With FixedWindow but running through the heading projector, it acts
        // like heading strategy with window_chars as max. Still produces chunks.
        assert!(!chunks.is_empty());
    }

    // ── Evidence on chunks ──

    #[test]
    fn chunks_carry_fragment_evidence() {
        let ast = make_ast(vec![vec![paragraph("Evidence test.")]]);
        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);

        // Chunk evidence is the projected fragments' provenance.
        assert_eq!(chunks[0].evidence.len(), 1);
        assert_eq!(chunks[0].evidence[0].extractor, "rule_boundary");
        assert_eq!(chunks[0].evidence[0].page, Some(1));
    }

    // ── Multi-page documents ──

    #[test]
    fn multi_page_document_preserves_page_numbers() {
        let ast = make_ast(vec![
            vec![paragraph("Page one content.")],
            vec![paragraph("Page two content.")],
        ]);

        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);

        let page1 = chunks.iter().find(|c| c.position.start_page == 1).unwrap();
        assert!(page1.text.contains("Page one"));

        let page2 = chunks.iter().find(|c| c.position.start_page == 2).unwrap();
        assert!(page2.text.contains("Page two"));
    }

    // ── Trait object ──

    #[test]
    fn heading_implements_retrieval_projector_trait() {
        let projector: &dyn RetrievalProjector = &HeadingProjector::new();
        assert_eq!(projector.name(), "heading-projection");

        let ast = make_ast(vec![vec![paragraph("Test.")]]);
        let chunks = projector.project(&fragments(&ast), None);
        assert_eq!(chunks.len(), 1);
    }

    // ── Projection properties ──

    #[test]
    fn table_fragment_projects_as_table_chunk() {
        let rows = "| Name | Age |\n| Alice | 30 |\n| Bob | 25 |";
        let ast = make_ast(vec![vec![AstNode {
            block_type: BlockType::Table,
            text: String::new(),
            children: table_children(rows),
            bbox: None,
            confidence: None,
            ..Default::default()
        }]]);

        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].structure.source_type, "table");
        assert!(chunks[0].text.contains("Name | Age"));
        assert!(chunks[0].text.contains("Alice | 30"));
    }

    #[test]
    fn chunk_boundaries_align_with_fragment_boundaries() {
        // Every chunk's text is composed of whole fragment texts — a chunk
        // never starts or ends mid-fragment. On a mixed doc, table content
        // appears exactly once, in exactly one chunk (whole, never split).
        let rows = "| K | V |\n| a | 1 |\n| b | 2 |";
        let nodes = vec![
            paragraph("Context paragraph before the table."),
            AstNode {
                block_type: BlockType::Table,
                text: String::new(),
                children: table_children(rows),
                bbox: None,
                confidence: None,
                ..Default::default()
            },
            paragraph("Context paragraph after the table."),
        ];
        let ast = make_ast(vec![vec![AstNode {
            block_type: BlockType::Unknown,
            text: String::new(),
            children: nodes,
            bbox: None,
            confidence: None,
            ..Default::default()
        }]]);

        let projector = HeadingProjector::new();
        let chunks = projector.project(&fragments(&ast), None);

        let table_carriers: Vec<&DocumentChunk> =
            chunks.iter().filter(|c| c.text.contains("K | V")).collect();
        assert_eq!(
            table_carriers.len(),
            1,
            "table content lives in exactly one chunk"
        );
        let chunk = table_carriers[0];
        assert!(
            chunk.text.contains("a | 1") && chunk.text.contains("b | 2"),
            "the one chunk carries the whole table"
        );
        // Paragraphs sharing the section ride along; the chunk reports mixed.
        assert_eq!(chunk.structure.source_type, "mixed");
    }

    // ── Default strategy ──

    #[test]
    fn default_strategy_is_heading() {
        let projector = HeadingProjector::new();
        match projector.strategy() {
            ChunkingStrategy::Heading {
                max_chunk_chars,
                overlap_chars,
            } => {
                assert_eq!(*max_chunk_chars, 4096);
                assert_eq!(*overlap_chars, 200);
            }
            _ => panic!("expected Heading strategy"),
        }
    }
}
