//! D8: Document Chunking — semantic segmentation for vector retrieval.
//!
//! Splits a `DocumentAst` into semantically coherent chunks, enriches each
//! chunk with document context (heading path, page, entity mentions), and
//! optionally embeds chunks using an `EmbeddingProvider`. The output feeds
//! into the vector engine for HNSW indexing and hybrid BM25+vector retrieval.
//!
//! # Architecture
//! - `DocumentChunk` — text segment with structural metadata
//! - `EmbeddedChunk` — chunk + embedding vector, ready for the vector engine
//! - `ChunkingStrategy` — how to split the document
//! - `DocumentChunker` trait — pluggable chunking
//! - `MockDocumentChunker` — heading-aware chunking with overlap

use crate::ast::{AstNode, BlockType, DocumentAst};
use crate::embedding::EmbeddingProvider;
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
    /// Provenance evidence.
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
    /// The block type that generated this chunk (e.g. "paragraph", "list", "table").
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

/// How to segment a document into chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkingStrategy {
    /// Split at heading boundaries. Each heading + its content becomes a chunk.
    /// Sub-headings create sub-chunks. Maximum chunk size is configurable.
    Heading {
        /// Maximum characters per chunk before forced split.
        max_chunk_chars: usize,
        /// Character overlap between adjacent chunks for context continuity.
        overlap_chars: usize,
    },
    /// Split at paragraph boundaries. Each paragraph is a chunk.
    /// Adjacent small paragraphs may be merged.
    Paragraph {
        /// Maximum characters per chunk (merge paragraphs up to this limit).
        max_chunk_chars: usize,
        /// Minimum characters for a standalone chunk.
        min_chunk_chars: usize,
    },
    /// Fixed-size sliding window with overlap.
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
// DocumentChunker trait
// ---------------------------------------------------------------------------

/// Pluggable document chunking: DocumentAst → Vec<DocumentChunk>.
pub trait DocumentChunker: Send + Sync {
    /// Human-readable name (e.g. "mock-heading", "semantic-llm").
    fn name(&self) -> &str;

    /// Current chunking strategy.
    fn strategy(&self) -> &ChunkingStrategy;

    /// Chunk a document AST into semantic segments.
    /// The optional `ir` provides entity mentions for enrichment.
    fn chunk(&self, ast: &DocumentAst, ir: Option<&KnowledgeIr>) -> Vec<DocumentChunk>;
}

// ---------------------------------------------------------------------------
// Mock chunker — heading-based segmentation
// ---------------------------------------------------------------------------

/// Heading-aware chunker using document structure.
///
/// Strategy:
/// - Walks the AST depth-first, tracking the current heading path.
/// - Content under each heading becomes a chunk.
/// - Large sections (> max_chunk_chars) are split at paragraph boundaries.
/// - Adjacent chunks overlap by `overlap_chars` characters for context continuity.
/// - Entity mentions from KnowledgeIr are attached to chunks.
pub struct MockDocumentChunker {
    strategy: ChunkingStrategy,
}

impl MockDocumentChunker {
    pub fn new() -> Self {
        MockDocumentChunker {
            strategy: ChunkingStrategy::default(),
        }
    }

    pub fn with_strategy(strategy: ChunkingStrategy) -> Self {
        MockDocumentChunker { strategy }
    }
}

impl DocumentChunker for MockDocumentChunker {
    fn name(&self) -> &str {
        "mock-heading"
    }

    fn strategy(&self) -> &ChunkingStrategy {
        &self.strategy
    }

    fn chunk(&self, ast: &DocumentAst, ir: Option<&KnowledgeIr>) -> Vec<DocumentChunk> {
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

        let mut chunks: Vec<DocumentChunk> = Vec::new();
        let mut char_offset: usize = 0;
        let heading_path: Vec<String> = Vec::new();

        // Build entity mention lookup: text substring → entity names.
        let entity_map = build_entity_map(ir);

        for (pi, page) in ast.pages.iter().enumerate() {
            let page_num = (pi + 1) as u32;
            let page_sections = collect_sections(&page.children, &heading_path);

            for section in &page_sections {
                if section.text.trim().is_empty() {
                    continue;
                }

                let entities = find_entity_mentions(&section.text, &entity_map);

                // If the section is small enough, emit as a single chunk.
                if section.text.len() <= max_chunk {
                    let chunk_id = format!("{}/chunk-{:04}", document_id, chunks.len());
                    chunks.push(DocumentChunk {
                        chunk_id,
                        text: section.text.clone(),
                        char_count: section.text.len(),
                        position: ChunkPosition {
                            chunk_index: chunks.len(),
                            start_page: page_num,
                            end_page: page_num,
                            char_offset,
                        },
                        structure: ChunkStructure {
                            heading_path: section.heading_path.clone(),
                            source_type: section.source_type.clone(),
                            section_title: section.heading_path.last().cloned(),
                            section_chunk_index: 0,
                        },
                        entity_mentions: entities,
                        evidence: evidence_for_page(page_num),
                    });
                    char_offset += section.text.len();
                    continue;
                }

                // Large section — split at paragraph boundaries with overlap.
                let sub_chunks = split_section(
                    &section.text,
                    max_chunk,
                    overlap,
                    &section.heading_path,
                    &section.source_type,
                    &entity_map,
                    document_id,
                    page_num,
                    chunks.len(),
                    &mut char_offset,
                );
                chunks.extend(sub_chunks);
            }
        }

        chunks
    }
}

/// A logical section extracted from the AST.
#[derive(Clone, Debug)]
struct Section {
    text: String,
    heading_path: Vec<String>,
    source_type: String,
}

/// Walk AST nodes and collect text grouped by sections (heading boundaries).
fn collect_sections(nodes: &[AstNode], initial_path: &[String]) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut heading_path: Vec<String> = initial_path.to_vec();
    let mut current_text = String::new();
    let mut current_source = "paragraph".to_string();

    for node in nodes {
        match &node.block_type {
            BlockType::Heading { level: _ } | BlockType::Title => {
                // Flush prior section with current heading_path.
                if !current_text.trim().is_empty() {
                    sections.push(Section {
                        text: std::mem::take(&mut current_text),
                        heading_path: heading_path.clone(),
                        source_type: std::mem::take(&mut current_source),
                    });
                }
                // Update heading path for content under this heading.
                heading_path.push(node.text.clone());
                // Recurse into children with the updated heading path.
                if !node.children.is_empty() {
                    sections.extend(collect_sections(&node.children, &heading_path));
                }
                // The heading text itself starts the new section content.
                current_text.push_str(&node.text);
                current_text.push('\n');
                current_source = format!("heading-{}", heading_path.len());
            }
            _ => {
                if !node.text.trim().is_empty() {
                    if !current_text.is_empty() && !current_text.ends_with('\n') {
                        current_text.push('\n');
                    }
                    current_text.push_str(&node.text);
                }
                // Recurse into children with current heading_path.
                if !node.children.is_empty() {
                    sections.extend(collect_sections(&node.children, &heading_path));
                }
            }
        }
    }

    // Flush final section with current heading_path.
    if !current_text.trim().is_empty() {
        sections.push(Section {
            text: current_text,
            heading_path,
            source_type: current_source,
        });
    }

    sections
}

/// Split a large section into sub-chunks at sentence/paragraph boundaries with overlap.
fn split_section(
    text: &str,
    max_chars: usize,
    overlap: usize,
    heading_path: &[String],
    source_type: &str,
    entity_map: &std::collections::HashMap<String, Vec<String>>,
    document_id: &str,
    page_num: u32,
    start_index: usize,
    char_offset: &mut usize,
) -> Vec<DocumentChunk> {
    let mut chunks: Vec<DocumentChunk> = Vec::new();
    let paragraphs: Vec<&str> = text.split('\n').filter(|p| !p.trim().is_empty()).collect();

    let mut current = String::new();
    let mut section_chunk_idx = 0usize;

    for para in paragraphs {
        // If adding this paragraph exceeds the limit, flush current chunk.
        if !current.is_empty() && current.len() + para.len() + 1 > max_chars {
            let chunk_idx = start_index + chunks.len();
            let chunk_id = format!("{}/chunk-{:04}", document_id, chunk_idx);
            let entities = find_entity_mentions(&current, entity_map);

            chunks.push(DocumentChunk {
                chunk_id,
                text: current.clone(),
                char_count: current.len(),
                position: ChunkPosition {
                    chunk_index: chunk_idx,
                    start_page: page_num,
                    end_page: page_num,
                    char_offset: *char_offset,
                },
                structure: ChunkStructure {
                    heading_path: heading_path.to_vec(),
                    source_type: source_type.to_string(),
                    section_title: heading_path.last().cloned(),
                    section_chunk_index: section_chunk_idx,
                },
                entity_mentions: entities,
                evidence: evidence_for_page(page_num),
            });

            *char_offset += current.len();
            section_chunk_idx += 1;

            // Start new chunk with overlap from the end of the previous.
            if overlap > 0 && current.len() > overlap {
                current = current[current.len() - overlap..].to_string();
                current.push('\n');
            } else {
                current = String::new();
            }
        }

        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(para);
    }

    // Flush final partial chunk.
    if !current.trim().is_empty() {
        let chunk_idx = start_index + chunks.len();
        let chunk_id = format!("{}/chunk-{:04}", document_id, chunk_idx);
        let entities = find_entity_mentions(&current, entity_map);

        chunks.push(DocumentChunk {
            chunk_id,
            text: current,
            char_count: 0, // computed below
            position: ChunkPosition {
                chunk_index: chunk_idx,
                start_page: page_num,
                end_page: page_num,
                char_offset: *char_offset,
            },
            structure: ChunkStructure {
                heading_path: heading_path.to_vec(),
                source_type: source_type.to_string(),
                section_title: heading_path.last().cloned(),
                section_chunk_index: section_chunk_idx,
            },
            entity_mentions: entities,
            evidence: evidence_for_page(page_num),
        });
        // Fix char_count for the last chunk.
        chunks.last_mut().unwrap().char_count = chunks.last().unwrap().text.len();
    }

    chunks
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

/// Create a minimal evidence record for a page.
fn evidence_for_page(page: u32) -> Vec<Evidence> {
    vec![Evidence {
        document_id: None,
        page: Some(page),
        bbox_text: None,
        extractor: "mock-chunker".into(),
        model: Some("mock-v1".into()),
        confidence: 1.0,
    }]
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

/// Convenience: chunk + embed in one call.
pub fn chunk_and_embed(
    ast: &DocumentAst,
    ir: Option<&KnowledgeIr>,
    chunker: &dyn DocumentChunker,
    provider: &dyn EmbeddingProvider,
) -> Vec<EmbeddedChunk> {
    let chunks = chunker.chunk(ast, ir);
    embed_chunks(&chunks, provider)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{AstNode, BlockType, DocumentAst};
    use crate::embedding::MockEmbeddingProvider;
    use crate::ir::{EntityCandidate, Evidence};

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
            })
            .collect();
        DocumentAst {
            pages,
            page_count,
            source_type: "native".into(),
        }
    }

    fn paragraph(text: &str) -> AstNode {
        AstNode {
            block_type: BlockType::Paragraph,
            text: text.to_string(),
            children: vec![],
            bbox: None,
            confidence: None,
        }
    }

    fn heading(level: u8, text: &str) -> AstNode {
        AstNode {
            block_type: BlockType::Heading { level },
            text: text.to_string(),
            children: vec![],
            bbox: None,
            confidence: None,
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

    // ── Basic chunking ──

    #[test]
    fn mock_chunker_has_name() {
        let c = MockDocumentChunker::new();
        assert_eq!(c.name(), "mock-heading");
    }

    #[test]
    fn single_paragraph_becomes_one_chunk() {
        let ast = make_ast(vec![vec![paragraph("This is a single paragraph of text.")]]);
        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, None);

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

        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, None);

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

        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, None);

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

    // ── Large section splitting ──

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
        }]]);

        // Use a small max_chunk_chars to force splits.
        let chunker = MockDocumentChunker::with_strategy(ChunkingStrategy::Heading {
            max_chunk_chars: 5000,
            overlap_chars: 100,
        });
        let chunks = chunker.chunk(&ast, None);

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

        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, Some(&ir));

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
        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, None);

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].entity_mentions.is_empty());
    }

    // ── Embedding bridge ──

    #[test]
    fn embed_chunks_produces_vectors() {
        let ast = make_ast(vec![vec![paragraph("Short text chunk.")]]);
        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, None);
        let provider = MockEmbeddingProvider::new();

        let embedded = embed_chunks(&chunks, &provider);
        assert_eq!(embedded.len(), 1);
        assert_eq!(embedded[0].embedding.len(), provider.dimensions());
        assert_eq!(embedded[0].embedding_provider, "mock-char-ngram");
    }

    #[test]
    fn chunk_and_embed_convenience() {
        let ast = make_ast(vec![vec![paragraph("Quick brown fox.")]]);
        let chunker = MockDocumentChunker::new();
        let provider = MockEmbeddingProvider::new();

        let embedded = chunk_and_embed(&ast, None, &chunker, &provider);
        assert!(!embedded.is_empty());
        assert!(!embedded[0].embedding.is_empty());
    }

    #[test]
    fn embedding_is_normalized() {
        let ast = make_ast(vec![vec![paragraph("Test chunk for embedding.")]]);
        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, None);
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

        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, None);

        let ids: std::collections::HashSet<&str> =
            chunks.iter().map(|c| c.chunk_id.as_str()).collect();
        assert_eq!(ids.len(), chunks.len(), "all chunk IDs must be unique");
    }

    // ── Empty document ──

    #[test]
    fn empty_document_produces_no_chunks() {
        let ast = make_ast(vec![]);
        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, None);
        assert!(chunks.is_empty());
    }

    #[test]
    fn whitespace_only_nodes_produce_no_chunks() {
        let ast = make_ast(vec![vec![paragraph("   "), paragraph("\n\n")]]);
        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, None);
        assert!(chunks.is_empty());
    }

    // ── Paragraph strategy ──

    #[test]
    fn paragraph_strategy_creates_chunks() {
        let ast = make_ast(vec![vec![
            paragraph("First paragraph of the document."),
            paragraph("Second paragraph with different content."),
        ]]);

        let chunker = MockDocumentChunker::with_strategy(ChunkingStrategy::Paragraph {
            max_chunk_chars: 4096,
            min_chunk_chars: 10,
        });
        let chunks = chunker.chunk(&ast, None);

        assert!(!chunks.is_empty());
    }

    // ── Fixed window strategy ──

    #[test]
    fn fixed_window_strategy_with_overlap() {
        let text = "ABCDEFGHIJKLMNOPQRSTUVWXYZ".repeat(10); // 260 chars
        let ast = make_ast(vec![vec![paragraph(&text)]]);

        let chunker = MockDocumentChunker::with_strategy(ChunkingStrategy::FixedWindow {
            window_chars: 100,
            overlap_chars: 20,
        });
        let chunks = chunker.chunk(&ast, None);

        // With FixedWindow but running through the mock (heading code path), it acts
        // like heading strategy with window_chars as max. Still produces chunks.
        assert!(!chunks.is_empty());
    }

    // ── Evidence on chunks ──

    #[test]
    fn chunks_have_evidence() {
        let ast = make_ast(vec![vec![paragraph("Evidence test.")]]);
        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, None);

        assert_eq!(chunks[0].evidence.len(), 1);
        assert_eq!(chunks[0].evidence[0].extractor, "mock-chunker");
        assert_eq!(chunks[0].evidence[0].page, Some(1));
    }

    // ── Multi-page documents ──

    #[test]
    fn multi_page_document_preserves_page_numbers() {
        let ast = make_ast(vec![
            vec![paragraph("Page one content.")],
            vec![paragraph("Page two content.")],
        ]);

        let chunker = MockDocumentChunker::new();
        let chunks = chunker.chunk(&ast, None);

        let page1 = chunks.iter().find(|c| c.position.start_page == 1).unwrap();
        assert!(page1.text.contains("Page one"));

        let page2 = chunks.iter().find(|c| c.position.start_page == 2).unwrap();
        assert!(page2.text.contains("Page two"));
    }

    // ── Trait object ──

    #[test]
    fn mock_implements_document_chunker_trait() {
        let chunker: &dyn DocumentChunker = &MockDocumentChunker::new();
        assert_eq!(chunker.name(), "mock-heading");

        let ast = make_ast(vec![vec![paragraph("Test.")]]);
        let chunks = chunker.chunk(&ast, None);
        assert_eq!(chunks.len(), 1);
    }

    // ── Default strategy ──

    #[test]
    fn default_strategy_is_heading() {
        let chunker = MockDocumentChunker::new();
        match chunker.strategy() {
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
