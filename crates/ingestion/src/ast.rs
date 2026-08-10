//! D3: Document AST — provider-independent structural representation.
//!
//! Sits between physical analysis (D1 native text, D2 OCR) and semantic
//! analysis (D4 Knowledge IR). The AST is the stable contract: every
//! extraction backend produces it, every semantic analyzer consumes it.
//!
//! # Architecture
//! - `BlockType` — classification enum (Heading, Paragraph, Table, ...)
//! - `AstNode` — recursive tree node with optional bounding box + confidence
//! - `DocumentAst` — container: pages, metadata, source provenance
//! - `document_model_to_ast()` — adapter from D1/D2 DocumentModel
//! - `classify_blocks()` — heuristic layout analysis on plain text

use crate::ocr::BlockBbox;
use crate::DocumentModel;

/// Classification of a structural block in a document.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum BlockType {
    Title,
    Heading { level: u8 },
    Paragraph,
    List { ordered: bool },
    ListItem,
    Table,
    TableRow,
    TableCell { row_span: u32, col_span: u32 },
    Image,
    Caption,
    Header,
    Footer,
    Footnote,
    Code,
    Unknown,
}

/// Bounding box on a specific page. Coordinates in document units.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BoundingBox {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A node in the document AST. Recursive: a Section contains Headings,
/// Paragraphs, Tables; a Table contains TableRows; etc.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AstNode {
    pub block_type: BlockType,
    pub text: String,
    #[serde(default)]
    pub children: Vec<AstNode>,
    pub bbox: Option<BoundingBox>,
    pub confidence: Option<f32>,
}

/// Provider-independent document structure produced by physical analysis.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DocumentAst {
    /// Top-level blocks, one per page.
    pub pages: Vec<AstNode>,
    pub page_count: u32,
    /// Provenance: "native", "ocr", or "mixed".
    pub source_type: String,
}

// ---------------------------------------------------------------------------
// DocumentModel → DocumentAst adapter
// ---------------------------------------------------------------------------

/// Convert D1/D2 `DocumentModel` into the provider-independent `DocumentAst`.
///
/// Each page becomes a top-level block. Within each page, plain text is split
/// into structural blocks via heuristic classification.
pub fn document_model_to_ast(doc: &DocumentModel) -> DocumentAst {
    let mut pages = Vec::with_capacity(doc.pages.len());

    for page in &doc.pages {
        let blocks = classify_blocks(&page.text, page.page_number);
        pages.push(AstNode {
            block_type: BlockType::Unknown, // page container
            text: String::new(),
            children: blocks,
            bbox: None,
            confidence: page.ocr_confidence,
        });
    }

    let source_type = determine_source_type(doc);

    DocumentAst {
        page_count: doc.page_count,
        pages,
        source_type,
    }
}

fn determine_source_type(doc: &DocumentModel) -> String {
    match &doc.ocr_stats {
        Some(stats) if stats.pages_ocr_attempted > 0 => {
            if stats.pages_ocr_failed > 0 {
                "mixed".into()
            } else {
                let native = doc.pages.iter().filter(|p| p.source == "native").count();
                if native > 0 {
                    "mixed".into()
                } else {
                    "ocr".into()
                }
            }
        }
        _ => "native".into(),
    }
}

// ---------------------------------------------------------------------------
// Heuristic block classification
// ---------------------------------------------------------------------------

/// Split page text into structural blocks using layout heuristics.
///
/// Strategy:
/// 1. Split on blank lines (paragraph boundaries).
/// 2. Classify each block: heading, paragraph, list, table, code.
fn classify_blocks(text: &str, _page: u32) -> Vec<AstNode> {
    if text.trim().is_empty() {
        return vec![];
    }

    // Split into blocks by blank-line boundaries.
    let raw_blocks: Vec<&str> = text.split("\n\n").collect();
    let mut nodes = Vec::with_capacity(raw_blocks.len());

    // Track consecutive list-item-like blocks to group into a List.
    let mut list_buffer: Vec<AstNode> = Vec::new();

    for raw in raw_blocks {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        // ── Code block: check raw lines to preserve first-line indentation ──
        let raw_lines: Vec<&str> = raw.lines().collect();
        let is_code = raw_lines.len() >= 2
            && raw_lines.iter().all(|l| {
                let t = l.trim();
                t.is_empty() || l.starts_with("    ") || l.starts_with('\t')
            })
            && raw_lines.iter().any(|l| !l.trim().is_empty());
        if is_code {
            flush_list(&mut list_buffer, &mut nodes);
            nodes.push(AstNode {
                block_type: BlockType::Code,
                text: trimmed.to_string(),
                children: vec![],
                bbox: None,
                confidence: None,
            });
            continue;
        }

        let lines: Vec<&str> = trimmed.lines().collect();

        // ── List item detection (before heading/table: "1. First\n2. Second" is a
        //     list, not a heading; "- a\n- b" is not a 3-col table) ──
        if is_list_item(&lines) {
            list_buffer.push(build_list_items(&lines, trimmed));
            continue;
        } else {
            flush_list(&mut list_buffer, &mut nodes);
        }

        // ── Figure marker: "Figure N:" / "Fig. N:" before heading check ──
        if parse_figure_marker(trimmed).is_some() {
            // Let detect_figures post-pass handle this.
            nodes.push(AstNode {
                block_type: BlockType::Paragraph,
                text: trimmed.to_string(),
                children: vec![],
                bbox: None,
                confidence: None,
            });
            continue;
        }

        // ── Heading heuristic ──
        if let Some(heading) = try_heading(&lines, trimmed) {
            nodes.push(heading);
            continue;
        }

        // ── Table heuristic: lines with consistent field separation ──
        if is_table_block(&lines) {
            nodes.push(build_table_node(&lines, trimmed));
            continue;
        }

        // ── Default: paragraph ──
        nodes.push(AstNode {
            block_type: BlockType::Paragraph,
            text: trimmed.to_string(),
            children: vec![],
            bbox: None,
            confidence: None,
        });
    }

    flush_list(&mut list_buffer, &mut nodes);

    // Post-processing passes.
    let nodes = merge_list_continuations(nodes);
    let nodes = merge_adjacent_lists(nodes);

    detect_figures(nodes)
}

/// Post-pass: merge adjacent List blocks into a single List.
fn merge_adjacent_lists(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let mut out: Vec<AstNode> = Vec::with_capacity(nodes.len());
    for node in nodes {
        match &node.block_type {
            BlockType::List { .. } => {
                if let Some(last) = out.last_mut() {
                    if matches!(last.block_type, BlockType::List { .. }) {
                        last.children.append(&mut node.children.clone());
                        continue;
                    }
                }
                out.push(node);
            }
            _ => out.push(node),
        }
    }
    out
}

fn flush_list(buffer: &mut Vec<AstNode>, out: &mut Vec<AstNode>) {
    if buffer.is_empty() {
        return;
    }
    let items = std::mem::take(buffer);
    let ordered = items.first().is_some_and(|item| {
        // Ordered if the first item starts with a digit followed by "." or ")"
        let t = item.text.trim();
        t.chars().next().is_some_and(|c| c.is_ascii_digit())
            && t.chars().nth(1).is_some_and(|c| c == '.' || c == ')')
    });
    out.push(AstNode {
        block_type: BlockType::List { ordered },
        text: String::new(),
        children: items,
        bbox: None,
        confidence: None,
    });
}

// ── Heading detection ──

fn try_heading(lines: &[&str], text: &str) -> Option<AstNode> {
    let first = lines[0].trim();
    if first.is_empty() {
        return None;
    }

    // Numeric prefix: "1.", "1.1", "1.1.1", "Section 1", "Chapter 2"
    let numeric_prefix: String = first
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if numeric_prefix.len() >= 2 && first.len() < 150 {
        let dots = numeric_prefix.chars().filter(|c| *c == '.').count() as u8;
        let level = dots.clamp(1, 3); // "1." → 1 dot → level 1; "1.1.1" → 3 dots → level 3
        return Some(AstNode {
            block_type: BlockType::Heading { level },
            text: text.to_string(),
            children: vec![],
            bbox: None,
            confidence: None,
        });
    }

    // Section/Chapter keyword prefix
    for (prefix, default_level) in &[
        ("Chapter ", 1u8),
        ("Section ", 2u8),
        ("Part ", 1u8),
        ("Appendix ", 1u8),
    ] {
        if first.starts_with(prefix) && first.len() < 150 {
            return Some(AstNode {
                block_type: BlockType::Heading {
                    level: *default_level,
                },
                text: text.to_string(),
                children: vec![],
                bbox: None,
                confidence: None,
            });
        }
    }

    // Single short line, all-caps or title-case → Heading level 1
    if lines.len() == 1
        && first.len() <= 120
        && !first.ends_with('.')
        && !first.ends_with(',')
        && !first.ends_with(';')
    {
        let alpha_chars: Vec<char> = first.chars().filter(|c| c.is_alphabetic()).collect();
        if alpha_chars.is_empty() {
            return None;
        }
        let upper_ratio = alpha_chars.iter().filter(|c| c.is_uppercase()).count() as f32
            / alpha_chars.len() as f32;

        let level = if upper_ratio > 0.8 {
            1u8
        } else if lines.len() == 1 && first.len() <= 80 {
            2u8
        } else {
            return None;
        };

        return Some(AstNode {
            block_type: BlockType::Heading { level },
            text: text.to_string(),
            children: vec![],
            bbox: None,
            confidence: None,
        });
    }

    None
}

// ── List detection ──

fn is_list_item(lines: &[&str]) -> bool {
    // Multi-line blocks where all lines are list-like → list.
    // Single-line numeric blocks are ambiguous (could be headings), let
    // try_heading decide; single-line bullets stay as list items.
    if lines.len() == 1 {
        let t = lines[0].trim();
        if t.is_empty() {
            return false;
        }
        let first = t.chars().next().unwrap_or('\0');
        // Numeric single-line → heading candidate, not list
        if first.is_ascii_digit() {
            return false;
        }
        // Alphabetic single-line with lettered prefix → heading candidate
        if first.is_ascii_alphabetic() && t.len() > 2 {
            let rest: String = t.chars().skip(1).collect();
            if rest.starts_with(". ") || rest.starts_with(") ") {
                return false;
            }
        }
    }

    lines.iter().all(|l| {
        let t = l.trim();
        if t.is_empty() {
            return false;
        }
        // Bullet markers
        if t.starts_with('•')
            || t.starts_with(" - ")
            || t.starts_with("- ")
            || t.starts_with("* ")
            || t.starts_with("· ")
            || t.starts_with("‣ ")
            || t.starts_with("○ ")
            || t.starts_with("▪ ")
        {
            return true;
        }
        // Numbered: "1.", "1)", "(1)"
        let first = t.chars().next().unwrap_or('\0');
        if first.is_ascii_digit() {
            let after_digits: String = t.chars().skip_while(|c| c.is_ascii_digit()).collect();
            return after_digits.starts_with(". ")
                || after_digits.starts_with(") ")
                || after_digits.starts_with(" - ");
        }
        // Lettered: "a.", "A)"
        if first.is_ascii_alphabetic() && t.len() > 2 {
            let rest: String = t.chars().skip(1).collect();
            return rest.starts_with(". ") || rest.starts_with(") ");
        }
        false
    })
}

fn build_list_items(lines: &[&str], _text: &str) -> AstNode {
    // ponytail: single block only, multi-paragraph list items deferred
    AstNode {
        block_type: BlockType::ListItem,
        text: lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" "),
        children: vec![],
        bbox: None,
        confidence: None,
    }
}

// ── Table detection ──

fn is_table_block(lines: &[&str]) -> bool {
    // A table must have at least 2 rows.
    if lines.len() < 2 {
        return false;
    }

    // Quick reject: if every line looks like a list item, it's not a table.
    if lines.iter().all(|l| {
        let t = l.trim();
        t.starts_with('-')
            || t.starts_with('•')
            || t.starts_with('*')
            || t.starts_with(" - ")
            || (t.chars().next().is_some_and(|c| c.is_ascii_digit())
                && t.chars()
                    .find(|c| !c.is_ascii_digit())
                    .is_some_and(|c| c == '.' || c == ')'))
    }) {
        return false;
    }

    // Count how the lines split. Consistent field counts → table.
    let splits: Vec<usize> = lines
        .iter()
        .map(|l| {
            // Split by 2+ spaces, tabs, or pipe characters
            let by_pipe: Vec<&str> = l.split('|').collect();
            if by_pipe.len() >= 3 {
                return by_pipe.len();
            }
            let by_spaces: Vec<&str> = l.split_whitespace().collect();
            by_spaces.len()
        })
        .collect();

    // Check consistency: at least 3 columns, most lines have same field count
    let mode = mode_usize(&splits);
    let consistent = splits.iter().filter(|&&s| s == mode).count();

    mode >= 3 && consistent as f32 / splits.len() as f32 >= 0.7
}

fn build_table_node(lines: &[&str], _text: &str) -> AstNode {
    let mut rows = Vec::new();
    for line in lines {
        let cells: Vec<&str> = if line.contains('|') {
            line.split('|')
                .map(|c| c.trim())
                .filter(|c| !c.is_empty())
                .collect()
        } else {
            line.split_whitespace().collect()
        };

        let cell_nodes: Vec<AstNode> = cells
            .iter()
            .map(|c| AstNode {
                block_type: BlockType::TableCell {
                    row_span: 1,
                    col_span: 1,
                },
                text: c.to_string(),
                children: vec![],
                bbox: None,
                confidence: None,
            })
            .collect();

        rows.push(AstNode {
            block_type: BlockType::TableRow,
            text: String::new(),
            children: cell_nodes,
            bbox: None,
            confidence: None,
        });
    }

    AstNode {
        block_type: BlockType::Table,
        text: String::new(),
        children: rows,
        bbox: None,
        confidence: None,
    }
}

/// Mode (most frequent value) of a usize slice.
fn mode_usize(vals: &[usize]) -> usize {
    let mut best = 0usize;
    let mut best_count = 0u32;
    for v in vals {
        let count = vals.iter().filter(|&&x| x == *v).count() as u32;
        if count > best_count {
            best_count = count;
            best = *v;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Enriched classification with OCR bbox data
// ---------------------------------------------------------------------------

/// Like `classify_blocks()` but uses OCR bounding-box data for:
/// - Font-size-based heading detection (word height as proxy)
/// - Position-based header/footer detection
/// - Title detection (first large block on page)
///
/// `bboxes` should be sorted by top position (ascending). Blocks are matched
/// in order — the i-th text block uses the i-th bbox if available.
pub fn classify_blocks_enriched(text: &str, page: u32, bboxes: &[BlockBbox]) -> Vec<AstNode> {
    if text.trim().is_empty() {
        return vec![];
    }

    let raw_blocks: Vec<&str> = text.split("\n\n").collect();
    let mut nodes = Vec::with_capacity(raw_blocks.len());
    let mut list_buffer: Vec<AstNode> = Vec::new();

    // Compute average word height across all blocks on this page (for relative sizing).
    let page_avg_height: f32 = if bboxes.is_empty() {
        0.0
    } else {
        bboxes.iter().map(|b| b.avg_word_height).sum::<f32>() / bboxes.len() as f32
    };

    for (idx, raw) in raw_blocks.iter().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Code block check (unchanged — indentation-based).
        let raw_lines: Vec<&str> = raw.lines().collect();
        let is_code = raw_lines.len() >= 2
            && raw_lines.iter().all(|l| {
                let t = l.trim();
                t.is_empty() || l.starts_with("    ") || l.starts_with('\t')
            })
            && raw_lines.iter().any(|l| !l.trim().is_empty());
        if is_code {
            flush_list(&mut list_buffer, &mut nodes);
            nodes.push(AstNode {
                block_type: BlockType::Code,
                text: trimmed.to_string(),
                children: vec![],
                bbox: None,
                confidence: None,
            });
            continue;
        }

        let lines: Vec<&str> = trimmed.lines().collect();
        let hint = bboxes.get(idx);

        // ── List item detection (first, as in basic path) ──
        if is_list_item(&lines) {
            let mut item = build_list_items(&lines, trimmed);
            if let Some(h) = hint {
                item.bbox = Some(BoundingBox {
                    page,
                    x: h.left as f32,
                    y: h.top as f32,
                    width: h.width as f32,
                    height: h.height as f32,
                });
                item.confidence = Some(h.avg_confidence);
            }
            list_buffer.push(item);
            continue;
        } else {
            flush_list(&mut list_buffer, &mut nodes);
        }

        // ── Position-based header/footer detection ──
        if let Some(h) = hint {
            // Header: first block, very near top, small font (not a title).
            if idx == 0
                && h.top <= 60
                && h.height < 40
                && (page_avg_height == 0.0 || h.avg_word_height <= page_avg_height)
            {
                nodes.push(AstNode {
                    block_type: BlockType::Header,
                    text: trimmed.to_string(),
                    children: vec![],
                    bbox: Some(BoundingBox {
                        page,
                        x: h.left as f32,
                        y: h.top as f32,
                        width: h.width as f32,
                        height: h.height as f32,
                    }),
                    confidence: Some(h.avg_confidence),
                });
                continue;
            }
            // Footer: near bottom of page (>2500px at 300dpi ~A4).
            if h.top > 2500 && h.height < 50 {
                nodes.push(AstNode {
                    block_type: BlockType::Footer,
                    text: trimmed.to_string(),
                    children: vec![],
                    bbox: Some(BoundingBox {
                        page,
                        x: h.left as f32,
                        y: h.top as f32,
                        width: h.width as f32,
                        height: h.height as f32,
                    }),
                    confidence: Some(h.avg_confidence),
                });
                continue;
            }
        }

        // ── Title detection: first block, larger-than-average font ──
        if idx == 0
            && hint
                .is_some_and(|h| page_avg_height > 0.0 && h.avg_word_height > page_avg_height * 1.3)
        {
            let h = hint.unwrap();
            nodes.push(AstNode {
                block_type: BlockType::Title,
                text: trimmed.to_string(),
                children: vec![],
                bbox: Some(BoundingBox {
                    page,
                    x: h.left as f32,
                    y: h.top as f32,
                    width: h.width as f32,
                    height: h.height as f32,
                }),
                confidence: Some(h.avg_confidence),
            });
            continue;
        }

        // ── Font-size-based heading: word height > 1.2x page average ──
        if hint.is_some_and(|h| page_avg_height > 0.0 && h.avg_word_height > page_avg_height * 1.2)
            && lines.len() <= 3
        {
            let h = hint.unwrap();
            let first = lines[0].trim();
            let level = heading_level_from_text(first);
            nodes.push(AstNode {
                block_type: BlockType::Heading { level },
                text: trimmed.to_string(),
                children: vec![],
                bbox: Some(BoundingBox {
                    page,
                    x: h.left as f32,
                    y: h.top as f32,
                    width: h.width as f32,
                    height: h.height as f32,
                }),
                confidence: Some(h.avg_confidence),
            });
            continue;
        }

        // ── Figure marker guard: don't classify "Figure N:" as heading ──
        if parse_figure_marker(trimmed).is_some() {
            let mut para = AstNode {
                block_type: BlockType::Paragraph,
                text: trimmed.to_string(),
                children: vec![],
                bbox: None,
                confidence: None,
            };
            if let Some(hint) = hint {
                para.bbox = Some(BoundingBox {
                    page,
                    x: hint.left as f32,
                    y: hint.top as f32,
                    width: hint.width as f32,
                    height: hint.height as f32,
                });
                para.confidence = Some(hint.avg_confidence);
            }
            nodes.push(para);
            continue;
        }

        // ── Fall through to text-based heading heuristic ──
        if let Some(heading) = try_heading(&lines, trimmed) {
            let mut h = heading;
            if let Some(hint) = hint {
                h.bbox = Some(BoundingBox {
                    page,
                    x: hint.left as f32,
                    y: hint.top as f32,
                    width: hint.width as f32,
                    height: hint.height as f32,
                });
                h.confidence = Some(hint.avg_confidence);
            }
            nodes.push(h);
            continue;
        }

        // ── Table detection ──
        if is_table_block(&lines) {
            let mut table = build_table_node(&lines, trimmed);
            if let Some(hint) = hint {
                table.bbox = Some(BoundingBox {
                    page,
                    x: hint.left as f32,
                    y: hint.top as f32,
                    width: hint.width as f32,
                    height: hint.height as f32,
                });
                table.confidence = Some(hint.avg_confidence);
            }
            nodes.push(table);
            continue;
        }

        // ── Default: paragraph ──
        let mut para = AstNode {
            block_type: BlockType::Paragraph,
            text: trimmed.to_string(),
            children: vec![],
            bbox: None,
            confidence: None,
        };
        if let Some(hint) = hint {
            para.bbox = Some(BoundingBox {
                page,
                x: hint.left as f32,
                y: hint.top as f32,
                width: hint.width as f32,
                height: hint.height as f32,
            });
            para.confidence = Some(hint.avg_confidence);
        }
        nodes.push(para);
    }

    flush_list(&mut list_buffer, &mut nodes);

    // Post-processing passes.
    let nodes = merge_list_continuations(nodes);
    let nodes = merge_adjacent_lists(nodes);

    detect_figures(nodes)
}

fn heading_level_from_text(first: &str) -> u8 {
    let dots = first
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .filter(|c| *c == '.')
        .count();
    if dots >= 2 {
        3
    } else if dots == 1 {
        2
    } else if first
        .chars()
        .filter(|c| c.is_alphabetic())
        .all(|c| c.is_uppercase())
    {
        1
    } else {
        2
    }
}

// ---------------------------------------------------------------------------
// Multi-paragraph list item merging
// ---------------------------------------------------------------------------

fn merge_list_continuations(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let mut out: Vec<AstNode> = Vec::with_capacity(nodes.len());
    let mut pending: Vec<AstNode> = Vec::new();

    for node in nodes {
        match &node.block_type {
            BlockType::ListItem => {
                if let Some(last) = out.last_mut() {
                    if matches!(last.block_type, BlockType::ListItem) {
                        last.children.append(&mut pending);
                    }
                }
                pending.clear();
                out.push(node);
            }
            BlockType::Paragraph => {
                let preceded_by_list_item = out.last().is_some_and(|n| {
                    matches!(n.block_type, BlockType::ListItem)
                        || (matches!(n.block_type, BlockType::List { .. })
                            && !n.children.is_empty())
                });
                if is_continuation_paragraph(&node) && preceded_by_list_item {
                    pending.push(node);
                } else {
                    flush_continuations(&mut out, &mut pending);
                    out.push(node);
                }
            }
            _ => {
                flush_continuations(&mut out, &mut pending);
                out.push(node);
            }
        }
    }
    flush_continuations(&mut out, &mut pending);
    out
}

fn is_continuation_paragraph(node: &AstNode) -> bool {
    // A paragraph is a continuation if it doesn't look like a new structural
    // element: no list markers, no heading patterns, no figure markers.
    let t = node.text.trim();
    if t.is_empty() {
        return false;
    }
    // Not a list item.
    if t.starts_with('-')
        || t.starts_with('•')
        || t.starts_with('*')
        || (t.chars().next().is_some_and(|c| c.is_ascii_digit())
            && t.chars()
                .find(|c| !c.is_ascii_digit())
                .is_some_and(|c| c == '.' || c == ')'))
    {
        return false;
    }
    // Not a figure marker.
    if parse_figure_marker(t).is_some() {
        return false;
    }
    // Not a heading (all-caps short line).
    let alpha: Vec<char> = t.chars().filter(|c| c.is_alphabetic()).collect();
    if !alpha.is_empty()
        && alpha.iter().all(|c| c.is_uppercase())
        && t.len() <= 80
        && !t.ends_with('.')
        && !t.ends_with(',')
        && !t.ends_with(';')
    {
        return false;
    }
    true
}

fn flush_continuations(out: &mut Vec<AstNode>, pending: &mut Vec<AstNode>) {
    if pending.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        match &last.block_type {
            BlockType::ListItem => {
                last.children.append(pending);
                return;
            }
            BlockType::List { .. } => {
                if let Some(last_item) = last.children.last_mut() {
                    if matches!(last_item.block_type, BlockType::ListItem) {
                        last_item.children.append(pending);
                        return;
                    }
                }
            }
            _ => {}
        }
    }
    out.append(pending);
}

// ---------------------------------------------------------------------------
// Figure detection
// ---------------------------------------------------------------------------

/// Post-pass: detect "Figure N:" / "Fig. N:" patterns, restructure as
/// Figure node with Caption child.
fn detect_figures(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let mut out: Vec<AstNode> = Vec::with_capacity(nodes.len());
    let mut i = 0;
    while i < nodes.len() {
        let node = &nodes[i];

        if node.block_type == BlockType::Paragraph {
            if let Some(_fig_num) = parse_figure_marker(&node.text) {
                let caption =
                    if i + 1 < nodes.len() && nodes[i + 1].block_type == BlockType::Paragraph {
                        i += 1;
                        let cap = &nodes[i];
                        AstNode {
                            block_type: BlockType::Caption,
                            text: cap.text.clone(),
                            children: vec![],
                            bbox: cap.bbox.clone(),
                            confidence: cap.confidence,
                        }
                    } else {
                        AstNode {
                            block_type: BlockType::Caption,
                            text: String::new(),
                            children: vec![],
                            bbox: None,
                            confidence: None,
                        }
                    };

                out.push(AstNode {
                    block_type: BlockType::Image,
                    text: node.text.clone(),
                    children: vec![caption],
                    bbox: node.bbox.clone(),
                    confidence: node.confidence,
                });
                i += 1;
                continue;
            }
        }

        out.push(node.clone());
        i += 1;
    }
    out
}

fn parse_figure_marker(text: &str) -> Option<String> {
    let t = text.trim();
    for prefix in &["Figure ", "Fig. ", "Fig "] {
        if let Some(rest) = t.strip_prefix(prefix) {
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !num.is_empty() {
                return Some(format!("{}{}", prefix, num));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Enriched adapter: DocumentModel → DocumentAst with bbox hints
// ---------------------------------------------------------------------------

/// Convert `DocumentModel` to `DocumentAst` using optional OCR bbox data per page.
pub fn document_model_to_ast_enriched(
    doc: &DocumentModel,
    block_bboxes_by_page: &[Vec<BlockBbox>],
) -> DocumentAst {
    let mut pages = Vec::with_capacity(doc.pages.len());

    for (i, page) in doc.pages.iter().enumerate() {
        let page_bboxes = block_bboxes_by_page
            .get(i)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let blocks = classify_blocks_enriched(&page.text, page.page_number, page_bboxes);

        pages.push(AstNode {
            block_type: BlockType::Unknown,
            text: String::new(),
            children: blocks,
            bbox: None,
            confidence: page.ocr_confidence,
        });
    }

    let source_type = determine_source_type(doc);

    DocumentAst {
        page_count: doc.page_count,
        pages,
        source_type,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentModel, PageModel};

    fn page(text: &str) -> PageModel {
        PageModel {
            page_number: 1,
            char_count: text.len(),
            text: text.to_string(),
            source: "native".into(),
            ocr_confidence: None,
        }
    }

    fn doc(pages: Vec<PageModel>) -> DocumentModel {
        let page_count = pages.len() as u32;
        let total_chars: usize = pages.iter().map(|p| p.char_count).sum();
        DocumentModel {
            page_count,
            pages,
            total_chars,
            ocr_stats: None,
        }
    }

    // ── Adapter ──

    #[test]
    fn adapter_produces_ast_from_document_model() {
        let dm = doc(vec![page("Line one.\n\nLine two.")]);
        let ast = document_model_to_ast(&dm);
        assert_eq!(ast.page_count, 1);
        assert_eq!(ast.pages.len(), 1);
        assert_eq!(ast.source_type, "native");
    }

    #[test]
    fn adapter_sets_source_type_ocr_when_all_pages_ocrd() {
        let mut dm = doc(vec![PageModel {
            page_number: 1,
            char_count: 200,
            text: "OCR text".into(),
            source: "ocr".into(),
            ocr_confidence: Some(88.0),
        }]);
        dm.ocr_stats = Some(crate::OcrStats {
            pages_ocr_attempted: 1,
            pages_ocr_succeeded: 1,
            pages_ocr_failed: 0,
            average_confidence: 88.0,
        });
        let ast = document_model_to_ast(&dm);
        assert_eq!(ast.source_type, "ocr");
    }

    #[test]
    fn adapter_sets_source_type_mixed_when_both_present() {
        let mut dm = doc(vec![
            PageModel {
                page_number: 1,
                char_count: 200,
                text: "Native text".into(),
                source: "native".into(),
                ocr_confidence: None,
            },
            PageModel {
                page_number: 2,
                char_count: 150,
                text: "OCR text".into(),
                source: "ocr".into(),
                ocr_confidence: Some(85.0),
            },
        ]);
        dm.ocr_stats = Some(crate::OcrStats {
            pages_ocr_attempted: 1,
            pages_ocr_succeeded: 1,
            pages_ocr_failed: 0,
            average_confidence: 85.0,
        });
        let ast = document_model_to_ast(&dm);
        assert_eq!(ast.source_type, "mixed");
    }

    #[test]
    fn adapter_empty_document() {
        let dm = doc(vec![]);
        let ast = document_model_to_ast(&dm);
        assert_eq!(ast.page_count, 0);
        assert!(ast.pages.is_empty());
        assert_eq!(ast.source_type, "native");
    }

    // ── Heading classification ──

    #[test]
    fn classify_numbered_heading() {
        let blocks = classify_blocks("1. Introduction", 1);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_type, BlockType::Heading { level: 1 });
        assert!(blocks[0].text.contains("Introduction"));
    }

    #[test]
    fn classify_all_caps_heading() {
        let blocks = classify_blocks("PAYMENT TERMS", 1);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0].block_type, BlockType::Heading { .. }));
    }

    #[test]
    fn classify_section_keyword_heading() {
        let blocks = classify_blocks("Section 2: Scope of Work", 1);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0].block_type, BlockType::Heading { .. }));
    }

    #[test]
    fn paragraph_not_misclassified_as_heading() {
        let blocks = classify_blocks(
            "This is a normal paragraph that contains multiple words and ends with a period.",
            1,
        );
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_type, BlockType::Paragraph);
    }

    // ── List classification ──

    #[test]
    fn classify_bullet_list() {
        let blocks = classify_blocks("- Item one\n- Item two\n- Item three", 1);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0].block_type, BlockType::List { .. }));
        assert_eq!(blocks[0].children.len(), 1); // ponytail: single block list item
    }

    #[test]
    fn classify_numbered_list() {
        let blocks = classify_blocks("1. First\n2. Second\n3. Third", 1);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            blocks[0].block_type,
            BlockType::List { ordered: true }
        ));
    }

    // ── Table classification ──

    #[test]
    fn classify_pipe_table() {
        let text = "| Name | Age | City |\n| Alice | 30 | NYC |\n| Bob | 25 | LA |";
        let blocks = classify_blocks(text, 1);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_type, BlockType::Table);
        assert_eq!(blocks[0].children.len(), 3); // 3 rows
        for row in &blocks[0].children {
            assert_eq!(row.block_type, BlockType::TableRow);
            assert!(row.children.len() >= 3);
        }
    }

    #[test]
    fn classify_multiple_blocks() {
        let text =
            "1. Overview\n\nThis is a paragraph about the system.\n\n- Feature A\n- Feature B";
        let blocks = classify_blocks(text, 1);
        assert_eq!(blocks.len(), 3); // heading, paragraph, list
        assert!(matches!(blocks[0].block_type, BlockType::Heading { .. }));
        assert_eq!(blocks[1].block_type, BlockType::Paragraph);
        assert!(matches!(blocks[2].block_type, BlockType::List { .. }));
    }

    // ── End-to-end: heading + paragraphs + table ──

    #[test]
    fn full_document_ast_with_heading_paragraphs_and_table() {
        let text = "1. Payment Terms\n\nPayment is due within 30 days of invoice date.\n\nLate payments incur a 1.5% monthly finance charge.\n\n| Item | Qty | Rate | Amount |\n| Widget | 10 | 5.00 | 50.00 |\n| Gadget | 5 | 12.00 | 60.00 |";
        let dm = doc(vec![page(text)]);
        let ast = document_model_to_ast(&dm);

        assert_eq!(ast.page_count, 1);
        let blocks = &ast.pages[0].children;
        assert_eq!(blocks.len(), 4); // heading, 2 paragraphs, table

        // Heading
        assert!(matches!(blocks[0].block_type, BlockType::Heading { .. }));
        assert!(blocks[0].text.contains("Payment Terms"));

        // Paragraphs
        assert_eq!(blocks[1].block_type, BlockType::Paragraph);
        assert!(blocks[1].text.contains("30 days"));
        assert_eq!(blocks[2].block_type, BlockType::Paragraph);
        assert!(blocks[2].text.contains("1.5%"));

        // Table
        assert_eq!(blocks[3].block_type, BlockType::Table);
        assert_eq!(blocks[3].children.len(), 3); // 3 rows
                                                 // First row (header)
        assert_eq!(blocks[3].children[0].block_type, BlockType::TableRow);
        assert_eq!(blocks[3].children[0].children.len(), 4);
        assert_eq!(blocks[3].children[0].children[0].text, "Item");
    }

    // ── Code block ──

    #[test]
    fn classify_code_block() {
        let blocks = classify_blocks("    fn main() {\n        println!(\"hello\");\n    }", 1);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_type, BlockType::Code);
    }

    #[test]
    fn single_indented_line_not_code() {
        // A single indented line is a continuation paragraph, not a code block.
        let blocks = classify_blocks("    This is indented but not code.", 1);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_type, BlockType::Paragraph);
    }

    // ── Serde round-trip ──

    #[test]
    fn serde_roundtrip() {
        let ast = DocumentAst {
            page_count: 1,
            source_type: "native".into(),
            pages: vec![AstNode {
                block_type: BlockType::Unknown,
                text: String::new(),
                children: vec![
                    AstNode {
                        block_type: BlockType::Heading { level: 1 },
                        text: "Title".into(),
                        children: vec![],
                        bbox: None,
                        confidence: None,
                    },
                    AstNode {
                        block_type: BlockType::Paragraph,
                        text: "Body text.".into(),
                        children: vec![],
                        bbox: None,
                        confidence: None,
                    },
                ],
                bbox: None,
                confidence: None,
            }],
        };

        let json = serde_json::to_string_pretty(&ast).unwrap();
        let back: DocumentAst = serde_json::from_str(&json).unwrap();
        assert_eq!(back.page_count, 1);
        assert_eq!(back.pages[0].children.len(), 2);
        assert_eq!(
            back.pages[0].children[0].block_type,
            BlockType::Heading { level: 1 }
        );
    }

    // ── Enriched classification: font-size-based heading ──

    fn bbox(top: i32, height: i32, avg_h: f32, conf: f32) -> BlockBbox {
        BlockBbox {
            page: 1,
            left: 10,
            top,
            width: 500,
            height,
            avg_word_height: avg_h,
            avg_confidence: conf,
        }
    }

    #[test]
    fn enriched_font_size_heading_detection() {
        // Page with avg word height ~11.3. One block at 14.0 (>1.2x) → heading.
        let bboxes = vec![
            bbox(120, 25, 10.0, 90.0), // normal paragraph (top > 60, not header)
            bbox(200, 30, 14.0, 88.0), // larger font → heading
            bbox(350, 25, 10.0, 91.0), // normal paragraph
        ];
        let text = "Normal paragraph text here.\n\nPAYMENT TERMS\n\nAnother normal paragraph.";
        let blocks = classify_blocks_enriched(text, 1, &bboxes);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].block_type, BlockType::Paragraph);
        assert!(matches!(blocks[1].block_type, BlockType::Heading { .. }));
        assert!(blocks[1].text.contains("PAYMENT TERMS"));
        assert_eq!(blocks[2].block_type, BlockType::Paragraph);
    }

    #[test]
    fn enriched_title_detection_first_large_block() {
        // First block with 20.0 avg height (>1.3x page avg of 12) → title.
        let bboxes = vec![
            bbox(100, 35, 20.0, 92.0), // large first block → title
            bbox(250, 18, 8.0, 90.0),  // small body text
        ];
        let text = "INVOICE\n\nDetails of the invoice.";
        let blocks = classify_blocks_enriched(text, 1, &bboxes);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].block_type, BlockType::Title);
        assert!(blocks[0].text.contains("INVOICE"));
        assert_eq!(blocks[1].block_type, BlockType::Paragraph);
    }

    #[test]
    fn enriched_header_detection_first_block_near_top() {
        let bboxes = vec![
            bbox(40, 20, 8.0, 85.0),   // very near top, small font ≤ page_avg → header
            bbox(200, 25, 12.0, 90.0), // main content (page_avg ~10.0)
        ];
        let text = "Page 1 of 5\n\nMain content starts here.";
        let blocks = classify_blocks_enriched(text, 1, &bboxes);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].block_type, BlockType::Header);
        assert!(blocks[0].text.contains("Page 1"));
        assert_eq!(blocks[1].block_type, BlockType::Paragraph);
    }

    #[test]
    fn enriched_footer_detection_near_bottom() {
        let bboxes = vec![
            bbox(200, 25, 12.0, 90.0), // main content
            bbox(2600, 20, 8.0, 85.0), // near bottom → footer
        ];
        let text = "Main content starts here.\n\nCompany Confidential";
        let blocks = classify_blocks_enriched(text, 1, &bboxes);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].block_type, BlockType::Paragraph);
        assert_eq!(blocks[1].block_type, BlockType::Footer);
        assert!(blocks[1].text.contains("Confidential"));
    }

    #[test]
    fn enriched_bbox_propagated_to_nodes() {
        let bboxes = vec![bbox(100, 24, 12.0, 87.5)];
        let text = "A single paragraph with bbox.";
        let blocks = classify_blocks_enriched(text, 1, &bboxes);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].bbox.is_some());
        let b = blocks[0].bbox.as_ref().unwrap();
        assert_eq!(b.page, 1);
        assert_eq!(b.x, 10.0);
        assert_eq!(b.y, 100.0);
        assert_eq!(b.width, 500.0);
        assert_eq!(b.height, 24.0);
        assert!((blocks[0].confidence.unwrap() - 87.5).abs() < 0.1);
    }

    #[test]
    fn enriched_falls_back_to_text_heuristics_without_bboxes() {
        // Without bbox data, should still detect heading by text pattern.
        let text = "1. Introduction\n\nBody paragraph here.";
        let blocks = classify_blocks_enriched(text, 1, &[]);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[0].block_type, BlockType::Heading { .. }));
        assert_eq!(blocks[1].block_type, BlockType::Paragraph);
    }

    // ── Multi-paragraph list items ──

    #[test]
    fn multi_paragraph_list_item_merges_indented_continuation() {
        // Multi-line list items (not single-line) so is_list_item detects them.
        // The indented block between them is a continuation of the first item.
        let text = "- First item with enough text to make it multi-word\n\n    Continuation of first item indented.\n\n- Second item also multi-word text";
        let blocks = classify_blocks(text, 1);
        // Should have 1 list with 2 items; first item has 1 continuation child.
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0].block_type, BlockType::List { .. }));
        assert_eq!(blocks[0].children.len(), 2); // 2 ListItems

        // First ListItem should have a continuation paragraph child.
        let item1 = &blocks[0].children[0];
        assert_eq!(item1.block_type, BlockType::ListItem);
        assert_eq!(item1.children.len(), 1);
        assert_eq!(item1.children[0].block_type, BlockType::Paragraph);
        assert!(item1.children[0].text.contains("Continuation"));

        let item2 = &blocks[0].children[1];
        assert_eq!(item2.block_type, BlockType::ListItem);
        assert!(item2.children.is_empty());
    }

    #[test]
    fn standalone_paragraph_not_merged_into_list() {
        // A paragraph after a list that looks like a heading is NOT merged.
        let text = "- Item one\n- Item two\n\nNEW SECTION HEADING";
        let blocks = classify_blocks(text, 1);
        // The all-caps short line should be a Heading, not a continuation.
        assert!(blocks
            .iter()
            .any(|b| matches!(b.block_type, BlockType::Heading { .. })));
        assert!(blocks
            .iter()
            .any(|b| matches!(b.block_type, BlockType::List { .. })));
    }

    #[test]
    fn standalone_paragraph_between_list_items_stays_separate_if_it_looks_like_heading() {
        // A paragraph that looks like a heading between list items is NOT merged.
        // "1. First item" and "2. Second item" are single-line numeric → headings.
        // The middle is also treated independently.
        let text = "1. First item\n\nCOMPLETELY DIFFERENT SECTION\n\n2. Second item";
        let blocks = classify_blocks(text, 1);
        // Should have: heading, heading (all-caps), heading
        assert!(blocks
            .iter()
            .any(|b| matches!(b.block_type, BlockType::Heading { .. })));
        // The all-caps middle section should also be a heading.
        let headings: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b.block_type, BlockType::Heading { .. }))
            .collect();
        assert!(headings.len() >= 2);
    }

    // ── Figure detection ──

    #[test]
    fn figure_detection_merges_caption() {
        let text =
            "Some intro paragraph.\n\nFigure 1: System Architecture\n\nArchitecture diagram caption text.";
        let blocks = classify_blocks(text, 1);
        // Should contain: paragraph, Image (Figure), not a standalone Paragraph for caption.
        assert!(blocks.iter().any(|b| b.block_type == BlockType::Image));
        let fig = blocks
            .iter()
            .find(|b| b.block_type == BlockType::Image)
            .unwrap();
        assert!(fig.text.contains("Figure 1"));
        assert_eq!(fig.children.len(), 1);
        assert_eq!(fig.children[0].block_type, BlockType::Caption);
        assert!(fig.children[0].text.contains("diagram caption"));
    }

    #[test]
    fn fig_abbreviation_detected() {
        let text = "Fig. 2: Revenue Growth\n\nQuarterly revenue growth chart.";
        let blocks = classify_blocks(text, 1);
        assert!(blocks.iter().any(|b| b.block_type == BlockType::Image));
        let fig = blocks
            .iter()
            .find(|b| b.block_type == BlockType::Image)
            .unwrap();
        assert!(fig.text.contains("Fig. 2"));
        assert_eq!(fig.children[0].block_type, BlockType::Caption);
    }

    #[test]
    fn mixed_document_figures_and_paragraphs() {
        let text =
            "Introduction paragraph.\n\nFigure 3: Data Flow\n\nThe data flows from left to right.\n\nConclusion paragraph.";
        let blocks = classify_blocks(text, 1);
        // Should have: paragraph, Image, paragraph
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].block_type, BlockType::Paragraph);
        assert_eq!(blocks[1].block_type, BlockType::Image);
        assert_eq!(blocks[1].children[0].block_type, BlockType::Caption);
        assert_eq!(blocks[2].block_type, BlockType::Paragraph);
    }

    // ── Enriched adapter end-to-end ──

    #[test]
    fn enriched_adapter_uses_bboxes() {
        let dm = doc(vec![PageModel {
            page_number: 1,
            char_count: 200,
            text: "INVOICE\n\nPayment terms here.\n\nFigure 1: Diagram\n\nDiagram description.\n\nTotal: $100.00".into(),
            source: "ocr".into(),
            ocr_confidence: Some(88.0),
        }]);

        // Provide bboxes: title-sized first block, smaller rest.
        let page_bboxes = vec![vec![
            bbox(100, 35, 16.0, 92.0), // title (larger font than body)
            bbox(200, 25, 10.0, 90.0), // paragraph
            bbox(300, 24, 10.5, 88.0), // figure marker
            bbox(400, 25, 10.0, 89.0), // caption
            bbox(550, 25, 10.0, 91.0), // paragraph
        ]];

        let ast = document_model_to_ast_enriched(&dm, &page_bboxes);
        assert_eq!(ast.page_count, 1);
        assert_eq!(ast.pages.len(), 1);

        let blocks = &ast.pages[0].children;
        // Title (font-size detected), Paragraph, Image (with caption), Paragraph
        assert!(blocks.iter().any(|b| b.block_type == BlockType::Title));
        assert!(blocks.iter().any(|b| b.block_type == BlockType::Image));
        assert!(blocks.iter().any(|b| b.block_type == BlockType::Paragraph));

        // Title should have bbox
        let title = blocks
            .iter()
            .find(|b| b.block_type == BlockType::Title)
            .unwrap();
        assert!(title.bbox.is_some());
    }

    #[test]
    fn enriched_adapter_without_bboxes_behaves_like_basic() {
        let dm = doc(vec![page("1. Heading\n\nBody paragraph.")]);
        let enriched = document_model_to_ast_enriched(&dm, &[]);
        let basic = document_model_to_ast(&dm);

        assert_eq!(enriched.page_count, basic.page_count);
        // Both should detect the heading
        let e_blocks = &enriched.pages[0].children;
        let b_blocks = &basic.pages[0].children;
        assert_eq!(e_blocks.len(), b_blocks.len());
        assert_eq!(e_blocks[0].block_type, b_blocks[0].block_type);
    }
}
