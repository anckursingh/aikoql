//! D4: Knowledge IR — staging layer between DocumentAst and kernel commit.
//!
//! Semantic analyzers produce `KnowledgeIr` from `DocumentAst`. The IR holds
//! candidate entities, relations, facts, events, and temporal assertions, each
//! carrying provenance evidence. Validation and ontology resolution happen in
//! later phases (D5-D7) before kernel commit.
//!
//! # Architecture
//! - `Evidence` — per-candidate provenance (document, page, extractor, confidence)
//! - `EntityCandidate` — named entity with type hint and mentions
//! - `RelationCandidate` — subject–predicate–object triple
//! - `FactCandidate` — statement with linked entities
//! - `EventCandidate` — temporal event with participants
//! - `TemporalAssertion` — time-bound claim
//! - `KnowledgeIr` — container for all candidate types
//! - `SemanticAnalyzer` — trait: `analyze(ast, fragments) → KnowledgeIr`

use crate::ast::{BlockType, DocumentAst};
use crate::boundary::{KnowledgeBoundaryDetector, RuleBoundaryDetector};
use crate::fragment::{FragmentContent, FragmentModality, KnowledgeFragment};
use crate::source::EvidenceSource;
use crate::visual::{MODEL_CHART, MODEL_DIAGRAM, MODEL_FORMULA, MODEL_IMAGE};
use aikoql_kernel::ContentTrust;

/// serde adapter for `ContentTrust` — the kernel crate is std-only (no
/// serde), so the IR serializes trust as its string form ("trusted" /
/// "untrusted" / "unknown"). Unknown strings fail closed to `None`.
mod ct_serde {
    use aikoql_kernel::ContentTrust;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<ContentTrust>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(ct) => serializer.serialize_str(ct.as_str()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<ContentTrust>, D::Error> {
        let s = Option::<String>::deserialize(deserializer)?;
        Ok(s.and_then(|v| ContentTrust::from_str(&v)))
    }
}

// ---------------------------------------------------------------------------
// Evidence — provenance for every candidate
// ---------------------------------------------------------------------------

/// Ties a candidate back to its source in the document.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Evidence {
    /// Document identifier (filename, URI, or KOID).
    pub document_id: Option<String>,
    /// Page number where the evidence was found (1-based).
    pub page: Option<u32>,
    /// Typed evidence source (HLD §14): paragraph span, table cell, chart
    /// point, region, or asset. `None` when the candidate derives from a
    /// non-spatial source (merge, code index) — provenance then lives in
    /// `document_id`/`extractor`.
    #[serde(default)]
    pub source: Option<EvidenceSource>,
    /// Name of the extractor that produced this candidate.
    pub extractor: String,
    /// Model or version identifier (e.g. "mock-v1", "gpt-4o").
    pub model: Option<String>,
    /// Confidence score 0.0–1.0 from the extractor.
    pub confidence: f32,
}

impl Default for Evidence {
    fn default() -> Self {
        Evidence {
            document_id: None,
            page: None,
            source: None,
            extractor: "unknown".into(),
            model: None,
            confidence: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Candidate types
// ---------------------------------------------------------------------------

/// A named entity found in the document.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntityCandidate {
    /// Canonical name (e.g. "Acme Corp", "John Smith").
    pub name: String,
    /// Hint at the ontology type (e.g. "Organization", "Person", "Location").
    pub type_hint: Option<String>,
    /// Text spans where this entity appears.
    pub mentions: Vec<String>,
    /// Confidence from the extractor.
    pub confidence: f32,
    /// Provenance evidence.
    pub evidence: Evidence,
}

/// A relationship between two entities.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RelationCandidate {
    /// Subject entity name (resolved to EntityCandidate.name).
    pub subject: String,
    /// Predicate / relationship type (e.g. "employed_by", "located_in").
    pub predicate: String,
    /// Object entity name.
    pub object: String,
    /// Confidence from the extractor.
    pub confidence: f32,
    /// Provenance evidence.
    pub evidence: Evidence,
}

/// A factual statement extracted from the document.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FactCandidate {
    /// The statement text (e.g. "Acme Corp was founded in 2019").
    pub statement: String,
    /// Entity names referenced in this statement.
    pub entities: Vec<String>,
    /// Confidence from the extractor.
    pub confidence: f32,
    /// Provenance evidence.
    pub evidence: Evidence,
}

/// An event described in the document with temporal context.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EventCandidate {
    /// Description of the event.
    pub description: String,
    /// What triggered or caused this event, if mentioned.
    pub trigger: Option<String>,
    /// Entity names participating in this event.
    pub participants: Vec<String>,
    /// Temporal assertions linked to this event.
    pub temporal: Vec<TemporalAssertion>,
    /// Confidence from the extractor.
    pub confidence: f32,
    /// Provenance evidence.
    pub evidence: Evidence,
}

/// A time-bound claim (date, duration, or relative time reference).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TemporalAssertion {
    /// The raw temporal text (e.g. "January 2024", "Q3 2025", "last week").
    pub text: String,
    /// ISO-8601 start time, if parseable.
    pub start_time: Option<String>,
    /// ISO-8601 end time, if parseable.
    pub end_time: Option<String>,
    /// Confidence from the extractor.
    pub confidence: f32,
    /// Provenance evidence.
    pub evidence: Evidence,
}

// ---------------------------------------------------------------------------
// Knowledge IR container
// ---------------------------------------------------------------------------

/// Staging representation between DocumentAst and kernel commit.
///
/// Produced by `SemanticAnalyzer::analyze()`. Candidates are validated,
/// reconciled, and resolved to ontology types in phases D5-D7 before
/// being committed as KnowledgeObjects.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeIr {
    pub entities: Vec<EntityCandidate>,
    pub relations: Vec<RelationCandidate>,
    pub facts: Vec<FactCandidate>,
    pub events: Vec<EventCandidate>,
    pub temporal: Vec<TemporalAssertion>,
    /// Source document identifier for all candidates.
    pub document_id: Option<String>,
    /// Trust level of the ingested content (R8). `None` = not tagged —
    /// treated conservatively as untrusted. ingest-dir stamps `Trusted`
    /// (reviewed local repo); uploads are stamped `Untrusted` by
    /// `deploy_document` and carried into the re-compiled IR.
    #[serde(with = "ct_serde", default)]
    pub content_trust: Option<ContentTrust>,
    /// Total pages processed.
    pub page_count: u32,
    /// Name of the extractor that produced this IR.
    pub extractor: String,
}

impl KnowledgeIr {
    /// True when no candidates of any kind were found.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
            && self.relations.is_empty()
            && self.facts.is_empty()
            && self.events.is_empty()
            && self.temporal.is_empty()
    }

    /// Total candidate count across all categories.
    pub fn total_candidates(&self) -> usize {
        self.entities.len()
            + self.relations.len()
            + self.facts.len()
            + self.events.len()
            + self.temporal.len()
    }
}

// ---------------------------------------------------------------------------
// SemanticAnalyzer trait
// ---------------------------------------------------------------------------

/// Pluggable semantic analysis: DocumentAst → KnowledgeIr.
///
/// Implementations range from simple regex heuristics (mock) to LLM-based
/// extraction (OpenAI, Anthropic). The trait is the stable contract between
/// the ingestion pipeline and semantic backends.
pub trait SemanticAnalyzer: Send + Sync {
    /// Human-readable name (e.g. "mock", "openai-gpt-4o").
    fn name(&self) -> &str;

    /// Analyze a document AST and produce structured knowledge candidates.
    ///
    /// `fragments` is the boundary stream for the same AST (HLD §57: the
    /// semantic leg consumes fragments). Analyzers that need full node
    /// structure (the markdown section classifier) may ignore it; when it
    /// is empty — degraded boundary detection — analyzers should fall back
    /// to the AST so semantic extraction never hard-fails.
    fn analyze(&self, ast: &DocumentAst, fragments: &[KnowledgeFragment]) -> KnowledgeIr;
}

// ---------------------------------------------------------------------------
// Mock semantic analyzer — rule-based extraction for testing
// ---------------------------------------------------------------------------

/// A mock analyzer that extracts entities and relations using simple heuristics.
///
/// Strategy:
/// - **Entities**: capitalized phrases ≥2 words from headings and paragraphs.
/// - **Relations**: co-occurring entity pairs within the same paragraph.
/// - **Facts**: headings as fact statements.
/// - **Events**: paragraphs containing date-like patterns.
/// - **Temporal**: date-like substrings extracted from text.
pub struct MockSemanticAnalyzer {
    /// Confidence value assigned to all mock candidates.
    pub confidence: f32,
}

impl Default for MockSemanticAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl MockSemanticAnalyzer {
    pub fn new() -> Self {
        MockSemanticAnalyzer { confidence: 0.85 }
    }

    pub fn with_confidence(confidence: f32) -> Self {
        MockSemanticAnalyzer { confidence }
    }
}

impl SemanticAnalyzer for MockSemanticAnalyzer {
    fn name(&self) -> &str {
        "mock"
    }

    fn analyze(&self, ast: &DocumentAst, fragments: &[KnowledgeFragment]) -> KnowledgeIr {
        if fragments.is_empty() {
            // Degraded boundary detection: keep the AST fallback so a
            // detector failure never empties the semantic IR.
            return self.analyze_ast(ast);
        }
        self.analyze_fragments(ast, fragments)
    }
}

impl MockSemanticAnalyzer {
    /// AST fallback (pre-fragment behavior, kept for fail-soft parity).
    fn analyze_ast(&self, ast: &DocumentAst) -> KnowledgeIr {
        let mut ir = KnowledgeIr {
            document_id: None,
            page_count: ast.page_count,
            extractor: "mock".into(),
            ..Default::default()
        };

        let extractor: String = "mock".into();

        for (pi, page) in ast.pages.iter().enumerate() {
            let page_num = (pi + 1) as u32;
            let page_text = collect_text(&page.children);

            // Extract entities: capitalized multi-word phrases
            for entity_name in extract_capitalized_phrases(&page_text) {
                if !ir.entities.iter().any(|e| e.name == entity_name) {
                    let mentions: Vec<String> = page
                        .children
                        .iter()
                        .filter(|c| c.text.as_deref().unwrap_or_default().contains(&entity_name))
                        .map(|c| c.text.clone().unwrap_or_default())
                        .collect();
                    ir.entities.push(EntityCandidate {
                        name: entity_name.clone(),
                        type_hint: guess_type(&entity_name),
                        mentions: if mentions.is_empty() {
                            vec![entity_name.clone()]
                        } else {
                            mentions
                        },
                        confidence: self.confidence,
                        evidence: Evidence {
                            document_id: None,
                            page: Some(page_num),
                            source: None,
                            extractor: extractor.clone(),
                            model: Some("mock-v1".into()),
                            confidence: self.confidence,
                        },
                    });
                }
            }

            // Extract facts from headings
            for child in &page.children {
                if matches!(child.block_type, BlockType::Heading { .. })
                    || matches!(child.block_type, BlockType::Title)
                {
                    let entities: Vec<String> =
                        extract_capitalized_phrases(child.text.as_deref().unwrap_or_default());
                    if !child.text.as_deref().unwrap_or_default().is_empty() && !entities.is_empty()
                    {
                        ir.facts.push(FactCandidate {
                            statement: child.text.clone().unwrap_or_default(),
                            entities,
                            confidence: self.confidence,
                            evidence: Evidence {
                                document_id: None,
                                page: Some(page_num),
                                source: None,
                                extractor: extractor.clone(),
                                model: Some("mock-v1".into()),
                                confidence: self.confidence,
                            },
                        });
                    }
                }
            }

            // Extract temporal assertions
            for date_match in extract_date_patterns(&page_text) {
                let (start, end) = parse_iso_date(&date_match);
                ir.temporal.push(TemporalAssertion {
                    text: date_match.clone(),
                    start_time: start,
                    end_time: end,
                    confidence: self.confidence,
                    evidence: Evidence {
                        document_id: None,
                        page: Some(page_num),
                        source: None,
                        extractor: extractor.clone(),
                        model: Some("mock-v1".into()),
                        confidence: self.confidence,
                    },
                });
            }
        }

        // Extract relations: co-occurring entity pairs in the same paragraph
        if ir.entities.len() >= 2 {
            for page in &ast.pages {
                let page_text = collect_text(&page.children);
                for paragraph in page_text.split('\n').filter(|p| !p.trim().is_empty()) {
                    let entities_in_para: Vec<&String> = ir
                        .entities
                        .iter()
                        .filter(|e| paragraph.contains(&e.name))
                        .map(|e| &e.name)
                        .collect();

                    for i in 0..entities_in_para.len() {
                        for j in (i + 1)..entities_in_para.len() {
                            let subj = entities_in_para[i].clone();
                            let obj = entities_in_para[j].clone();
                            // Skip if this pair already exists.
                            if ir
                                .relations
                                .iter()
                                .any(|r| r.subject == subj && r.object == obj)
                            {
                                continue;
                            }
                            ir.relations.push(RelationCandidate {
                                subject: subj,
                                predicate: "related_to".into(),
                                object: obj,
                                confidence: self.confidence * 0.7,
                                evidence: Evidence {
                                    document_id: None,
                                    page: None,
                                    source: None,
                                    extractor: extractor.clone(),
                                    model: Some("mock-v1".into()),
                                    confidence: self.confidence * 0.7,
                                },
                            });
                        }
                    }
                }
            }
        }

        ir
    }

    /// Fragment-stream semantic interpretation (HLD §57): modality-aware
    /// extraction — table cells become facts with cell-level provenance.
    fn analyze_fragments(&self, ast: &DocumentAst, fragments: &[KnowledgeFragment]) -> KnowledgeIr {
        let extractor: String = "mock".into();
        let mut ir = KnowledgeIr {
            document_id: None,
            page_count: ast.page_count,
            extractor: extractor.clone(),
            ..Default::default()
        };

        // Entities + temporal: the same heuristics as the AST fallback,
        // applied to each fragment's rendered text. Headings reach the
        // semantic leg as fragment context, so both paths scan them.
        // Visual fragments (PR-F) are skipped here: their rendered text is
        // the visual payload, and the visual loop below owns those entities
        // with typed (DiagramNode) evidence instead of generic mock-v1.
        for frag in fragments {
            let mut texts: Vec<String> = frag.context.heading_path.clone();
            if !matches!(
                frag.modality,
                FragmentModality::Image
                    | FragmentModality::Chart
                    | FragmentModality::Diagram
                    | FragmentModality::Formula
            ) {
                texts.push(crate::chunking::fragment_text(frag));
            }
            for text in &texts {
                for entity_name in extract_capitalized_phrases(text) {
                    if !ir.entities.iter().any(|e| e.name == entity_name) {
                        let mentions: Vec<String> = fragments
                            .iter()
                            .map(crate::chunking::fragment_text)
                            .filter(|t| t.contains(&entity_name))
                            .collect();
                        ir.entities.push(EntityCandidate {
                            name: entity_name.clone(),
                            type_hint: guess_type(&entity_name),
                            mentions: if mentions.is_empty() {
                                vec![entity_name.clone()]
                            } else {
                                mentions
                            },
                            confidence: self.confidence,
                            evidence: fragment_evidence(frag, &extractor, self.confidence),
                        });
                    }
                }
                for date_match in extract_date_patterns(text) {
                    let (start, end) = parse_iso_date(&date_match);
                    ir.temporal.push(TemporalAssertion {
                        text: date_match.clone(),
                        start_time: start,
                        end_time: end,
                        confidence: self.confidence,
                        evidence: fragment_evidence(frag, &extractor, self.confidence),
                    });
                }
            }
        }

        // Heading facts: headings are context, not fragments — one fact per
        // unique heading, matching the AST fallback's per-node behavior.
        let mut seen_headings: Vec<&String> = Vec::new();
        for frag in fragments {
            for heading in &frag.context.heading_path {
                let entities = extract_capitalized_phrases(heading);
                if !heading.trim().is_empty()
                    && !entities.is_empty()
                    && !seen_headings.contains(&heading)
                {
                    seen_headings.push(heading);
                    ir.facts.push(FactCandidate {
                        statement: heading.clone(),
                        entities,
                        confidence: self.confidence,
                        evidence: fragment_evidence(frag, &extractor, self.confidence),
                    });
                }
            }
        }

        // Modality-aware: table cells are knowledge with cell-level
        // provenance (HLD §14 differentiator).
        for frag in fragments {
            let FragmentContent::Table(table) = &frag.content else {
                continue;
            };
            for cell in &table.cells {
                if cell.text.trim().is_empty() {
                    continue;
                }
                let header = table
                    .headers
                    .iter()
                    .find(|h| h.id == cell.column_id)
                    .map(|h| h.text.as_str())
                    .unwrap_or("");
                ir.facts.push(FactCandidate {
                    statement: if header.is_empty() {
                        cell.text.clone()
                    } else {
                        format!("{}: {}", header, cell.text)
                    },
                    entities: Vec::new(),
                    confidence: self.confidence * cell.confidence,
                    evidence: Evidence {
                        document_id: None,
                        page: frag.context.page,
                        source: Some(EvidenceSource::TableCell {
                            table_id: frag.fragment_id.clone(),
                            cell_id: format!("{}-{}", cell.row_id, cell.column_id),
                        }),
                        extractor: extractor.clone(),
                        model: Some("mock-v1".into()),
                        confidence: self.confidence,
                    },
                });
            }
        }

        // PR-F: visual fragments → typed knowledge (HLD §10–§13). Diagram
        // nodes/edges become entities/relations with diagram-level evidence;
        // charts/formulas/images contribute facts carrying their model
        // version (DoD row 11).
        for frag in fragments {
            match &frag.content {
                FragmentContent::Diagram(diagram) => {
                    for node in &diagram.nodes {
                        if ir.entities.iter().any(|e| e.name == node.label) {
                            continue;
                        }
                        ir.entities.push(EntityCandidate {
                            name: node.label.clone(),
                            type_hint: Some("DiagramNode".into()),
                            mentions: vec![node.label.clone()],
                            confidence: self.confidence * node.confidence,
                            evidence: Evidence {
                                document_id: None,
                                page: frag.context.page,
                                source: Some(EvidenceSource::DiagramNode {
                                    diagram_id: frag.fragment_id.clone(),
                                    node_id: node.id.clone(),
                                }),
                                extractor: extractor.clone(),
                                model: Some(MODEL_DIAGRAM.into()),
                                confidence: self.confidence * node.confidence,
                            },
                        });
                    }
                    for edge in &diagram.edges {
                        let predicate = edge.label.clone().unwrap_or_else(|| "related_to".into());
                        if ir.relations.iter().any(|r| {
                            r.subject == edge.source
                                && r.object == edge.target
                                && r.predicate == predicate
                        }) {
                            continue;
                        }
                        ir.relations.push(RelationCandidate {
                            subject: edge.source.clone(),
                            object: edge.target.clone(),
                            predicate,
                            confidence: self.confidence * edge.confidence,
                            evidence: Evidence {
                                document_id: None,
                                page: frag.context.page,
                                source: Some(EvidenceSource::DiagramEdge {
                                    diagram_id: frag.fragment_id.clone(),
                                    edge_id: format!("{}->{}", edge.source, edge.target),
                                }),
                                extractor: extractor.clone(),
                                model: Some(MODEL_DIAGRAM.into()),
                                confidence: self.confidence * edge.confidence,
                            },
                        });
                    }
                }
                FragmentContent::Chart(chart) => {
                    let title = chart
                        .title
                        .clone()
                        .unwrap_or_else(|| "Untitled chart".into());
                    ir.facts.push(FactCandidate {
                        statement: format!("Chart: {} ({:?})", title, chart.chart_type),
                        entities: Vec::new(),
                        confidence: self.confidence,
                        evidence: Evidence {
                            document_id: None,
                            page: frag.context.page,
                            source: frag
                                .source
                                .as_ref()
                                .and_then(|s| s.bbox.as_ref())
                                .map(|b| EvidenceSource::Region { bbox: b.clone() }),
                            extractor: extractor.clone(),
                            model: Some(MODEL_CHART.into()),
                            confidence: self.confidence,
                        },
                    });
                }
                FragmentContent::Formula(formula) => {
                    if let Some(text) = formula.plain_text.clone().or_else(|| formula.latex.clone())
                    {
                        ir.facts.push(FactCandidate {
                            statement: format!("Formula: {}", text),
                            entities: Vec::new(),
                            confidence: self.confidence,
                            evidence: Evidence {
                                document_id: None,
                                page: frag.context.page,
                                source: None,
                                extractor: extractor.clone(),
                                model: Some(MODEL_FORMULA.into()),
                                confidence: self.confidence,
                            },
                        });
                    }
                }
                FragmentContent::Image(image) => {
                    let caption = image
                        .caption
                        .clone()
                        .unwrap_or_else(|| format!("asset {}", image.asset.content_hash));
                    ir.facts.push(FactCandidate {
                        statement: format!("Image: {}", caption),
                        entities: Vec::new(),
                        confidence: self.confidence,
                        evidence: Evidence {
                            document_id: None,
                            page: frag.context.page,
                            source: frag
                                .source
                                .as_ref()
                                .and_then(|s| s.bbox.as_ref())
                                .map(|b| EvidenceSource::Region { bbox: b.clone() }),
                            extractor: extractor.clone(),
                            model: Some(MODEL_IMAGE.into()),
                            confidence: self.confidence,
                        },
                    });
                    // OCR fill (HLD §33): scanned text becomes knowledge with
                    // the provider name as the model (DoD row 14).
                    if let Some(ocr_text) = image.ocr_text.clone() {
                        let snippet: String = ocr_text.chars().take(200).collect();
                        ir.facts.push(FactCandidate {
                            statement: format!("OCR text: {}", snippet),
                            entities: Vec::new(),
                            confidence: self.confidence,
                            evidence: Evidence {
                                document_id: None,
                                page: frag.context.page,
                                source: frag
                                    .source
                                    .as_ref()
                                    .and_then(|s| s.bbox.as_ref())
                                    .map(|b| EvidenceSource::Region { bbox: b.clone() }),
                                extractor: extractor.clone(),
                                model: Some(
                                    image
                                        .ocr_model
                                        .clone()
                                        .unwrap_or_else(|| MODEL_IMAGE.into()),
                                ),
                                confidence: self.confidence,
                            },
                        });
                    }
                }
                _ => {}
            }
        }

        // Relations: co-occurring entity pairs within a text fragment.
        if ir.entities.len() >= 2 {
            for frag in fragments {
                if frag.modality != FragmentModality::Text {
                    continue;
                }
                let FragmentContent::Text(text) = &frag.content else {
                    continue;
                };
                let entities_in: Vec<&String> = ir
                    .entities
                    .iter()
                    .filter(|e| text.contains(&e.name))
                    .map(|e| &e.name)
                    .collect();
                for i in 0..entities_in.len() {
                    for j in (i + 1)..entities_in.len() {
                        let subj = entities_in[i].clone();
                        let obj = entities_in[j].clone();
                        if ir
                            .relations
                            .iter()
                            .any(|r| r.subject == subj && r.object == obj)
                        {
                            continue;
                        }
                        ir.relations.push(RelationCandidate {
                            subject: subj,
                            predicate: "related_to".into(),
                            object: obj,
                            confidence: self.confidence * 0.7,
                            evidence: fragment_evidence(frag, &extractor, self.confidence * 0.7),
                        });
                    }
                }
            }
        }

        ir
    }
}

// ---------------------------------------------------------------------------
// Mock helpers — heuristic extraction
// ---------------------------------------------------------------------------

/// Evidence for a candidate derived from a fragment: page from fragment
/// context, region from the fragment's bbox when one exists.
fn fragment_evidence(frag: &KnowledgeFragment, extractor: &str, confidence: f32) -> Evidence {
    Evidence {
        document_id: None,
        page: frag.context.page,
        source: frag
            .source
            .as_ref()
            .and_then(|s| s.bbox.clone())
            .map(|b| EvidenceSource::Region { bbox: b }),
        extractor: extractor.to_string(),
        model: Some("mock-v1".into()),
        confidence,
    }
}

/// Walk all AST nodes and collect their text, one line per node.
fn collect_text(nodes: &[crate::AstNode]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for node in nodes {
        if let Some(text) = &node.text {
            if !text.trim().is_empty() {
                lines.push(text.clone());
            }
        }
        if !node.children.is_empty() {
            lines.push(collect_text(&node.children));
        }
    }
    lines.join("\n")
}

/// Extract capitalized multi-word phrases (potential named entities).
///
/// Uses a sliding window of 2–3 words within runs of consecutive capitalized
/// words. This avoids merging distinct entities like "Acme Corporation" and
/// "Annual Report" into one long phrase.
fn extract_capitalized_phrases(text: &str) -> Vec<String> {
    let mut entities: Vec<String> = Vec::new();
    // Split and clean trailing punctuation from each word.
    let raw_words: Vec<&str> = text.split_whitespace().collect();
    let words: Vec<String> = raw_words
        .iter()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .collect();

    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        if w.len() >= 2 && w.chars().next().is_some_and(|c| c.is_uppercase()) {
            // Find the end of this capitalized run.
            let mut end = i + 1;
            while end < words.len() {
                let next = &words[end];
                if next.len() >= 2 && next.chars().next().is_some_and(|c| c.is_uppercase()) {
                    end += 1;
                } else {
                    break;
                }
            }
            let run_len = end - i;
            // Emit 2-word sliding windows within the run.
            if run_len >= 2 {
                for start in i..(end - 1) {
                    let name = words[start..start + 2].join(" ");
                    if name.len() > 4 && !entities.contains(&name) {
                        entities.push(name);
                    }
                }
            }
            // Emit 3-word sliding windows within the run.
            if run_len >= 3 {
                for start in i..(end - 2) {
                    let name = words[start..start + 3].join(" ");
                    if name.len() > 4 && !entities.contains(&name) {
                        entities.push(name);
                    }
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    entities
}

/// Guess an ontology type from the entity name using keyword heuristics.
fn guess_type(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let org_keywords = [
        "inc",
        "corp",
        "ltd",
        "llc",
        "company",
        "corporation",
        "group",
        "holdings",
        "enterprise",
        "industries",
        "solutions",
        "technologies",
        "systems",
        "bank",
        "insurance",
        "capital",
        "partners",
        "consulting",
    ];
    let person_keywords = ["dr.", "mr.", "mrs.", "ms.", "prof.", "sir"];
    let location_keywords = [
        "city",
        "town",
        "county",
        "state",
        "province",
        "district",
        "region",
        "street",
        "avenue",
        "road",
        "lane",
        "boulevard",
        "drive",
        "court",
        "place",
        "square",
    ];

    for kw in &org_keywords {
        if lower.contains(kw) {
            return Some("Organization".into());
        }
    }
    for kw in &person_keywords {
        if lower.starts_with(kw) {
            return Some("Person".into());
        }
    }
    for kw in &location_keywords {
        if lower.contains(kw) {
            return Some("Location".into());
        }
    }
    // Default: check if it looks like a person (two words, first is short-ish)
    let parts: Vec<&str> = name.split_whitespace().collect();
    if parts.len() == 2 && parts[0].len() <= 15 {
        return Some("Person".into());
    }
    Some("Thing".into())
}

/// Find date-like patterns in text.
fn extract_date_patterns(text: &str) -> Vec<String> {
    let mut dates: Vec<String> = Vec::new();
    // Simple patterns: "Month YYYY", "Month DD, YYYY", "YYYY-MM-DD", "QN YYYY"
    let months = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
        "Jan",
        "Feb",
        "Mar",
        "Apr",
        "Jun",
        "Jul",
        "Aug",
        "Sep",
        "Oct",
        "Nov",
        "Dec",
    ];

    let words: Vec<&str> = text.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        let w_clean = w.trim_matches(|c: char| !c.is_alphanumeric());
        // "Month YYYY" or "Month DD, YYYY"
        if months.contains(&w_clean) && i + 1 < words.len() {
            let next = words[i + 1].trim_matches(|c: char| !c.is_alphanumeric());
            if next.len() == 4 && next.chars().all(|c| c.is_ascii_digit()) {
                dates.push(format!("{} {}", w_clean, next));
            } else if next.chars().all(|c| c.is_ascii_digit()) && i + 2 < words.len() {
                let year = words[i + 2].trim_matches(|c: char| !c.is_alphanumeric());
                if year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()) {
                    dates.push(format!("{} {}, {}", w_clean, next, year));
                }
            }
        }
        // "QN YYYY" (e.g. "Q3 2025")
        if w_clean.starts_with('Q')
            && w_clean.len() == 2
            && w_clean[1..].chars().all(|c| c.is_ascii_digit())
            && i + 1 < words.len()
        {
            let year = words[i + 1].trim_matches(|c: char| !c.is_alphanumeric());
            if year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()) {
                dates.push(format!("{} {}", w_clean, year));
            }
        }
        // "YYYY-MM-DD"
        if w_clean.len() == 10
            && w_clean.chars().nth(4) == Some('-')
            && w_clean.chars().nth(7) == Some('-')
        {
            let parts: Vec<&str> = w_clean.split('-').collect();
            if parts.len() == 3
                && parts[0].chars().all(|c| c.is_ascii_digit())
                && parts[1].chars().all(|c| c.is_ascii_digit())
                && parts[2].chars().all(|c| c.is_ascii_digit())
            {
                dates.push(w_clean.to_string());
            }
        }
    }
    dates
}

/// Parse a date string into ISO-8601 start/end, returning None if unparseable.
fn parse_iso_date(text: &str) -> (Option<String>, Option<String>) {
    let months_map: std::collections::HashMap<&str, &str> = [
        ("January", "01"),
        ("February", "02"),
        ("March", "03"),
        ("April", "04"),
        ("May", "05"),
        ("June", "06"),
        ("July", "07"),
        ("August", "08"),
        ("September", "09"),
        ("October", "10"),
        ("November", "11"),
        ("December", "12"),
        ("Jan", "01"),
        ("Feb", "02"),
        ("Mar", "03"),
        ("Apr", "04"),
        ("May", "05"),
        ("Jun", "06"),
        ("Jul", "07"),
        ("Aug", "08"),
        ("Sep", "09"),
        ("Oct", "10"),
        ("Nov", "11"),
        ("Dec", "12"),
    ]
    .iter()
    .cloned()
    .collect();

    // "YYYY-MM-DD"
    if text.len() == 10 && text.chars().nth(4) == Some('-') {
        return (
            Some(format!("{}T00:00:00Z", text)),
            Some(format!("{}T23:59:59Z", text)),
        );
    }

    // "Month YYYY"
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() == 2 {
        if let Some(month) = months_map.get(parts[0]) {
            if parts[1].len() == 4 && parts[1].chars().all(|c| c.is_ascii_digit()) {
                let start = format!("{}-{}-01T00:00:00Z", parts[1], month);
                let end_day = month_end_day(month, parts[1]).unwrap_or("31");
                let end = format!("{}-{}-{}T23:59:59Z", parts[1], month, end_day);
                return (Some(start), Some(end));
            }
        }
        // "QN YYYY"
        if parts[0].starts_with('Q') {
            let q: u32 = parts[0][1..].parse().unwrap_or(0);
            if (1..=4).contains(&q) {
                let (start_month, end_month) = match q {
                    1 => ("01", "03"),
                    2 => ("04", "06"),
                    3 => ("07", "09"),
                    4 => ("10", "12"),
                    _ => unreachable!(),
                };
                let start = format!("{}-{}-01T00:00:00Z", parts[1], start_month);
                let end_day = month_end_day(end_month, parts[1]).unwrap_or("31");
                let end = format!("{}-{}-{}T23:59:59Z", parts[1], end_month, end_day);
                return (Some(start), Some(end));
            }
        }
    }

    // "Month DD, YYYY"
    if parts.len() == 3 {
        if let Some(month) = months_map.get(parts[0]) {
            let day = parts[1].trim_matches(',');
            if day.chars().all(|c| c.is_ascii_digit()) && parts[2].len() == 4 {
                let day_padded = format!("{:0>2}", day);
                let start = format!("{}-{}-{}T00:00:00Z", parts[2], month, day_padded);
                let end = format!("{}-{}-{}T23:59:59Z", parts[2], month, day_padded);
                return (Some(start), Some(end));
            }
        }
    }

    (None, None)
}

fn month_end_day(month: &str, year: &str) -> Option<&'static str> {
    let y: i32 = year.parse().ok()?;
    match month {
        "01" | "03" | "05" | "07" | "08" | "10" | "12" => Some("31"),
        "04" | "06" | "09" | "11" => Some("30"),
        "02" => {
            if (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0) {
                Some("29")
            } else {
                Some("28")
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Convenience: DocumentModel → KnowledgeIr pipeline
// ---------------------------------------------------------------------------

/// Full pipeline: DocumentModel → DocumentAst → KnowledgeIr.
///
/// Uses the mock analyzer by default. Swap in an LLM-backed analyzer for
/// production use.
pub fn document_model_to_ir(
    doc: &crate::DocumentModel,
    analyzer: &dyn SemanticAnalyzer,
) -> KnowledgeIr {
    let ast = crate::document_model_to_ast(doc);
    // Semantic leg (HLD §57): AST → fragments → IR. Fails soft — a degraded
    // detector leaves the analyzer its AST fallback.
    let fragments = RuleBoundaryDetector.detect(&ast).unwrap_or_default();
    analyzer.analyze(&ast, &fragments)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{AstNode, BlockType, DocumentAst};
    use crate::visual::DiagramAnalyzer;

    fn make_ast(pages: Vec<Vec<AstNode>>) -> DocumentAst {
        let page_count = pages.len() as u32;
        let pages = pages
            .into_iter()
            .map(|children| AstNode {
                block_type: BlockType::Unknown,
                text: None,
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
            document_id: None,
        }
    }

    fn paragraph(text: &str) -> AstNode {
        AstNode {
            block_type: BlockType::Paragraph,
            text: Some(text.to_string()),
            children: vec![],
            bbox: None,
            confidence: None,
            ..Default::default()
        }
    }

    fn heading(level: u8, text: &str) -> AstNode {
        AstNode {
            block_type: BlockType::Heading { level },
            text: Some(text.to_string()),
            children: vec![],
            bbox: None,
            confidence: None,
            ..Default::default()
        }
    }

    fn title(text: &str) -> AstNode {
        AstNode {
            block_type: BlockType::Title,
            text: Some(text.to_string()),
            children: vec![],
            bbox: None,
            confidence: None,
            ..Default::default()
        }
    }

    fn table_node() -> AstNode {
        AstNode {
            block_type: BlockType::Table,
            text: None,
            children: vec![],
            bbox: None,
            confidence: None,
            payload: Some(crate::ast::AstPayload::Table(crate::ast::TablePayload {
                headers: vec![
                    crate::ast::TableHeader {
                        id: "h0".into(),
                        text: "Item".into(),
                        level: 0,
                        parent_id: None,
                    },
                    crate::ast::TableHeader {
                        id: "h1".into(),
                        text: "Qty".into(),
                        level: 0,
                        parent_id: None,
                    },
                ],
                rows: vec![crate::ast::TableRow {
                    id: "0".into(),
                    index: 0,
                }],
                cells: vec![
                    crate::ast::TableCell {
                        id: "c0".into(),
                        row_id: "0".into(),
                        column_id: "h0".into(),
                        text: "Widget".into(),
                        value: None,
                        bbox: None,
                        confidence: 1.0,
                    },
                    crate::ast::TableCell {
                        id: "c1".into(),
                        row_id: "0".into(),
                        column_id: "h1".into(),
                        text: "10".into(),
                        value: None,
                        bbox: None,
                        confidence: 1.0,
                    },
                ],
                footnotes: vec![],
            })),
            ..Default::default()
        }
    }

    // ── Entity extraction ──

    #[test]
    fn extracts_capitalized_entities_from_paragraphs() {
        let ast = make_ast(vec![vec![paragraph(
            "Acme Corporation announced a partnership with Globex Industries in New York.",
        )]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        let names: Vec<&str> = ir.entities.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"Acme Corporation"));
        assert!(names.contains(&"Globex Industries"));
        assert!(names.contains(&"New York"));
    }

    #[test]
    fn deduplicates_entities_across_pages() {
        let ast = make_ast(vec![
            vec![paragraph("Acme Corporation is a leader in widgets.")],
            vec![paragraph("Acme Corporation was founded in 2019.")],
        ]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        let count = ir
            .entities
            .iter()
            .filter(|e| e.name == "Acme Corporation")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn skips_single_capitalized_words() {
        let ast = make_ast(vec![vec![paragraph("Widgets are great.")]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        assert!(ir.entities.is_empty());
    }

    #[test]
    fn entity_has_type_hint() {
        let ast = make_ast(vec![vec![paragraph(
            "Acme Corp and John Smith met at City Hall on Main Street.",
        )]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        let org = ir.entities.iter().find(|e| e.name == "Acme Corp").unwrap();
        assert_eq!(org.type_hint.as_deref(), Some("Organization"));

        let person = ir.entities.iter().find(|e| e.name == "John Smith").unwrap();
        assert_eq!(person.type_hint.as_deref(), Some("Person"));
    }

    // ── Fact extraction ──

    #[test]
    fn extracts_facts_from_headings() {
        let ast = make_ast(vec![vec![
            heading(1, "Acme Corporation Reports Record Revenue"),
            paragraph("The company announced Q3 2025 results today."),
        ]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        assert!(!ir.facts.is_empty());
        let fact = &ir.facts[0];
        assert!(fact
            .statement
            .contains("Acme Corporation Reports Record Revenue"));
        assert!(!fact.entities.is_empty());
    }

    #[test]
    fn title_becomes_fact() {
        let ast = make_ast(vec![vec![
            title("Annual Report 2024"),
            paragraph("Prepared by Acme Corporation."),
        ]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        let fact = ir
            .facts
            .iter()
            .find(|f| f.statement.contains("Annual Report 2024"));
        assert!(fact.is_some());
    }

    // ── Relation extraction ──

    #[test]
    fn extracts_relations_between_cooccurring_entities() {
        let ast = make_ast(vec![vec![paragraph(
            "Acme Corporation partnered with Globex Industries to develop New Technology.",
        )]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        assert!(!ir.relations.is_empty());
        // All three entities should have relations between them.
        let pairs: Vec<(&str, &str)> = ir
            .relations
            .iter()
            .map(|r| (r.subject.as_str(), r.object.as_str()))
            .collect();
        assert!(
            pairs.contains(&("Acme Corporation", "Globex Industries"))
                || pairs.contains(&("Globex Industries", "Acme Corporation"))
        );
    }

    #[test]
    fn no_relations_with_single_entity() {
        let ast = make_ast(vec![vec![paragraph(
            "Acme Corporation is the market leader.",
        )]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        assert!(ir.relations.is_empty());
    }

    // ── Temporal extraction ──

    #[test]
    fn extracts_month_year_dates() {
        let ast = make_ast(vec![vec![paragraph(
            "The agreement was signed in January 2024 and renewed March 2025.",
        )]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        let texts: Vec<&str> = ir.temporal.iter().map(|t| t.text.as_str()).collect();
        assert!(
            texts.contains(&"January 2024"),
            "expected January 2024 in {:?}",
            texts
        );
        assert!(
            texts.contains(&"March 2025"),
            "expected March 2025 in {:?}",
            texts
        );
    }

    #[test]
    fn extracts_iso_dates() {
        let ast = make_ast(vec![vec![paragraph("Effective date: 2024-06-15.")]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        let iso = ir.temporal.iter().find(|t| t.text == "2024-06-15").unwrap();
        assert_eq!(iso.start_time.as_deref(), Some("2024-06-15T00:00:00Z"));
    }

    #[test]
    fn extracts_quarter_year_dates() {
        let ast = make_ast(vec![vec![paragraph(
            "Results for Q3 2025 exceeded expectations.",
        )]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        let q3 = ir.temporal.iter().find(|t| t.text == "Q3 2025").unwrap();
        assert!(q3.start_time.as_deref().unwrap().starts_with("2025-07"));
        assert!(q3.end_time.as_deref().unwrap().starts_with("2025-09"));
    }

    #[test]
    fn temporal_has_evidence() {
        let ast = make_ast(vec![vec![paragraph("Signed January 2024.")]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        let t = &ir.temporal[0];
        assert_eq!(t.evidence.page, Some(1));
        assert_eq!(t.evidence.extractor, "mock");
        assert!((t.evidence.confidence - 0.85).abs() < 0.001);
    }

    // ── Evidence propagation ──

    #[test]
    fn entity_evidence_includes_page_number() {
        let ast = make_ast(vec![
            vec![paragraph("Page 1: Acme Corporation.")],
            vec![paragraph("Page 2: Globex Industries.")],
        ]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        let acme = ir
            .entities
            .iter()
            .find(|e| e.name == "Acme Corporation")
            .unwrap();
        assert_eq!(acme.evidence.page, Some(1));

        let globex = ir
            .entities
            .iter()
            .find(|e| e.name == "Globex Industries")
            .unwrap();
        assert_eq!(globex.evidence.page, Some(2));
    }

    // ── KnowledgeIr container ──

    #[test]
    fn knowledge_ir_is_empty_for_blank_document() {
        let ast = make_ast(vec![]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        assert!(ir.is_empty());
        assert_eq!(ir.total_candidates(), 0);
    }

    #[test]
    fn knowledge_ir_total_candidates_counts_all_types() {
        let ast = make_ast(vec![vec![
            heading(1, "Acme Corporation Fiscal Year 2024"),
            paragraph("Acme Corporation reported revenue in January 2024."),
        ]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        assert!(ir.total_candidates() > 0);
        // Should have at least: 1 entity (Acme Corporation), 1 fact (heading), 1 temporal (January 2024)
        assert!(!ir.entities.is_empty());
        assert!(!ir.facts.is_empty());
        assert!(!ir.temporal.is_empty());
    }

    #[test]
    fn knowledge_ir_stores_extractor_name() {
        let ast = make_ast(vec![vec![paragraph("Test.")]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        assert_eq!(ir.extractor, "mock");
    }

    // ── Pipeline integration ──

    #[test]
    fn document_model_to_ir_pipeline() {
        use crate::{DocumentModel, PageModel};

        let doc = DocumentModel {
            page_count: 1,
            pages: vec![PageModel {
                page_number: 1,
                text: "Acme Corporation\n\nAnnual Report for Fiscal Year 2024\n\nPrepared by Globex Industries in January 2025.".into(),
                char_count: 90,
                source: "native".into(),
                ocr_confidence: None,
                images: vec![],
            }],
            total_chars: 90,
            ocr_stats: None,
        };

        let analyzer = MockSemanticAnalyzer::new();
        let ir = document_model_to_ir(&doc, &analyzer);

        assert!(!ir.is_empty());
        assert_eq!(ir.extractor, "mock");
        assert_eq!(ir.page_count, 1);

        let names: Vec<&str> = ir.entities.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"Acme Corporation"),
            "expected Acme Corporation in entities: {:?}",
            names
        );
        assert!(
            names.contains(&"Globex Industries"),
            "expected Globex Industries in entities: {:?}",
            names
        );
        assert!(
            names.contains(&"Fiscal Year"),
            "expected Fiscal Year in entities: {:?}",
            names
        );
    }

    // ── MockSemanticAnalyzer configurability ──

    #[test]
    fn mock_analyzer_custom_confidence() {
        let ast = make_ast(vec![vec![paragraph("Acme Corporation is great.")]]);

        let analyzer = MockSemanticAnalyzer::with_confidence(0.42);
        let ir = analyzer.analyze(&ast, &[]);

        assert!((ir.entities[0].confidence - 0.42).abs() < 0.001);
        assert!((ir.entities[0].evidence.confidence - 0.42).abs() < 0.001);
    }

    // ── SemanticAnalyzer trait ──

    #[test]
    fn mock_implements_semantic_analyzer_trait() {
        let ast = make_ast(vec![vec![paragraph("Acme Corporation.")]]);

        // Use trait object to verify trait impl.
        let analyzer: &dyn SemanticAnalyzer = &MockSemanticAnalyzer::new();
        assert_eq!(analyzer.name(), "mock");

        let ir = analyzer.analyze(&ast, &[]);
        assert!(!ir.entities.is_empty());
    }

    // ── Edge cases ──

    #[test]
    fn empty_document_produces_empty_ir() {
        let ast = make_ast(vec![vec![]]);
        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);
        assert!(ir.is_empty());
    }

    #[test]
    fn no_false_positives_on_common_words() {
        let ast = make_ast(vec![vec![paragraph(
            "The quick brown fox jumps over the lazy dog.",
        )]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        // "The", "Fox" — single words should not be entities.
        // No capitalized multi-word phrases in this sentence.
        assert!(ir.entities.is_empty());
    }

    #[test]
    fn handles_list_items_in_ast() {
        let ast = make_ast(vec![vec![
            heading(1, "Vendors"),
            AstNode {
                block_type: BlockType::List { ordered: false },
                text: None,
                children: vec![
                    AstNode {
                        block_type: BlockType::ListItem,
                        text: Some("Acme Corporation".into()),
                        children: vec![],
                        bbox: None,
                        confidence: None,
                        ..Default::default()
                    },
                    AstNode {
                        block_type: BlockType::ListItem,
                        text: Some("Globex Industries".into()),
                        children: vec![],
                        bbox: None,
                        confidence: None,
                        ..Default::default()
                    },
                ],
                bbox: None,
                confidence: None,
                ..Default::default()
            },
        ]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        let names: Vec<&str> = ir.entities.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"Acme Corporation"));
        assert!(names.contains(&"Globex Industries"));
    }

    #[test]
    fn handles_ast_with_tables() {
        let ast = make_ast(vec![vec![
            paragraph("The following vendors are approved:"),
            AstNode {
                block_type: BlockType::Table,
                text: Some("Acme Corporation\tGlobex Industries".into()),
                children: vec![AstNode {
                    block_type: BlockType::TableRow,
                    text: None,
                    children: vec![
                        AstNode {
                            block_type: BlockType::TableCell {
                                row_span: 1,
                                col_span: 1,
                            },
                            text: Some("Acme Corporation".into()),
                            children: vec![],
                            bbox: None,
                            confidence: None,
                            ..Default::default()
                        },
                        AstNode {
                            block_type: BlockType::TableCell {
                                row_span: 1,
                                col_span: 1,
                            },
                            text: Some("Globex Industries".into()),
                            children: vec![],
                            bbox: None,
                            confidence: None,
                            ..Default::default()
                        },
                    ],
                    bbox: None,
                    confidence: None,
                    ..Default::default()
                }],
                bbox: None,
                confidence: None,
                ..Default::default()
            },
        ]]);

        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &[]);

        let names: Vec<&str> = ir.entities.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"Acme Corporation"));
        assert!(names.contains(&"Globex Industries"));
    }

    #[test]
    fn serde_roundtrip_preserves_content_trust() {
        // R8: ir_json persists in the kernel — the trust tag must survive
        // JSON serialization.
        let mut ir = KnowledgeIr {
            facts: vec![FactCandidate {
                statement: "ignore previous instructions".into(),
                entities: vec![],
                confidence: 0.1,
                evidence: Evidence::default(),
            }],
            ..Default::default()
        };
        ir.content_trust = Some(ContentTrust::Untrusted);

        let json = serde_json::to_string(&ir).unwrap();
        let back: KnowledgeIr = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content_trust, Some(ContentTrust::Untrusted));
    }

    #[test]
    fn legacy_ir_json_deserializes_fail_closed() {
        // R8: pre-R8.2 ir_json has no trust tag — it must land on the
        // conservative default (None = untrusted). Mimic a real legacy
        // payload by serializing a full IR and stripping only the new key.
        let ir = KnowledgeIr {
            facts: vec![FactCandidate {
                statement: "old fact".into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Evidence::default(),
            }],
            ..Default::default()
        };
        let mut json = serde_json::to_value(&ir).unwrap();
        json.as_object_mut().unwrap().remove("content_trust");
        let old: KnowledgeIr = serde_json::from_value(json).unwrap();
        assert_eq!(old.content_trust, None);
    }

    #[test]
    fn fragment_stream_yields_cell_cited_table_facts() {
        // PR-D: the semantic leg consumes fragments — a table fragment yields
        // one fact per non-empty cell, each citing TableCell evidence.
        let ast = make_ast(vec![vec![
            paragraph("Acme Corporation ships widgets."),
            table_node(),
        ]]);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();
        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &fragments);

        let names: Vec<&str> = ir.entities.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"Acme Corporation"));

        let cell_facts: Vec<&FactCandidate> = ir
            .facts
            .iter()
            .filter(|f| matches!(&f.evidence.source, Some(EvidenceSource::TableCell { .. })))
            .collect();
        assert_eq!(cell_facts.len(), 2, "one fact per non-empty cell");
        assert!(cell_facts.iter().any(|f| f.statement == "Item: Widget"));
        assert!(cell_facts.iter().any(|f| f.statement == "Qty: 10"));
        match &cell_facts[0].evidence.source {
            Some(EvidenceSource::TableCell { table_id, cell_id }) => {
                assert!(
                    table_id.starts_with("frag-p1-b"),
                    "table_id is the fragment id"
                );
                assert!(cell_id.contains('-'), "cell_id names row and column");
            }
            other => panic!("expected TableCell evidence, got {:?}", other),
        }
    }

    #[test]
    fn diagram_fragment_yields_graph_entities_and_relations() {
        // PR-F: a diagram fragment's nodes/edges become entities/relations
        // with DiagramNode/DiagramEdge evidence and the model version
        // persisted (DoD row 11).
        let mut diagram = AstNode {
            block_type: BlockType::Diagram,
            text: Some("Client -> Gateway --> Ledger".into()),
            children: vec![],
            bbox: None,
            confidence: None,
            ..Default::default()
        };
        diagram.payload = crate::visual::MockDiagramAnalyzer
            .analyze(&diagram)
            .map(crate::ast::AstPayload::Diagram);
        let ast = make_ast(vec![vec![diagram]]);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();
        let analyzer = MockSemanticAnalyzer::new();
        let ir = analyzer.analyze(&ast, &fragments);

        let names: Vec<&str> = ir.entities.iter().map(|e| e.name.as_str()).collect();
        for expected in ["Client", "Gateway", "Ledger"] {
            assert!(names.contains(&expected), "diagram node '{}'", expected);
        }
        let client = ir
            .entities
            .iter()
            .find(|e| e.name == "Client")
            .expect("Client entity");
        match &client.evidence.source {
            Some(EvidenceSource::DiagramNode {
                diagram_id,
                node_id,
            }) => {
                assert!(diagram_id.starts_with("frag-p1-b"));
                assert_eq!(*node_id, "client");
            }
            other => panic!("expected DiagramNode evidence, got {:?}", other),
        }
        assert_eq!(client.evidence.model.as_deref(), Some("mock-diagram-v1"));

        assert_eq!(ir.relations.len(), 2, "one relation per diagram edge");
        let first = &ir.relations[0];
        assert_eq!(first.subject, "client");
        assert_eq!(first.object, "gateway");
        assert_eq!(first.predicate, "related_to");
        assert_eq!(
            first.evidence.model.as_deref(),
            Some("mock-diagram-v1"),
            "model version persisted on relations"
        );
    }

    #[test]
    fn fragment_and_ast_paths_agree_on_entities() {
        // Fail-soft parity: with a healthy detector both paths extract the
        // same entities, so a degraded detector is invisible to callers.
        let page_text = "1. Payment Terms\n\nAcme Corporation ships widgets.";
        let dm = crate::DocumentModel {
            page_count: 1,
            pages: vec![crate::PageModel {
                page_number: 1,
                text: page_text.into(),
                char_count: page_text.len(),
                source: "native".into(),
                ocr_confidence: None,
                images: vec![],
            }],
            total_chars: page_text.len(),
            ocr_stats: None,
        };
        let ast = crate::document_model_to_ast(&dm);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();
        let analyzer = MockSemanticAnalyzer::new();

        let from_ast = analyzer.analyze(&ast, &[]);
        let from_fragments = analyzer.analyze(&ast, &fragments);

        // The AST fallback concatenates blocks, so capitalized runs can span
        // block boundaries ("Terms Acme" from heading + paragraph). The
        // fragment path keeps blocks separate — cleaner, and never invents
        // entities the AST path wouldn't find.
        let ast_names: std::collections::BTreeSet<&str> =
            from_ast.entities.iter().map(|e| e.name.as_str()).collect();
        let frag_names: std::collections::BTreeSet<&str> = from_fragments
            .entities
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(frag_names.contains("Acme Corporation"));
        assert!(frag_names.contains("Payment Terms"));
        assert!(!from_ast.facts.is_empty());
        assert!(!from_fragments.facts.is_empty());
        assert!(
            frag_names.is_subset(&ast_names),
            "fragment path must not invent entities: {:?} not subset of {:?}",
            frag_names,
            ast_names
        );
    }

    #[test]
    fn legacy_bbox_text_evidence_deserializes_to_typed_source() {
        // PR-D: bbox_text is gone from the wire format; old payloads must
        // still deserialize, dropping the legacy string key.
        let legacy = r#"{"document_id":null,"page":1,"bbox_text":"(1,2,3,4)","extractor":"mock","model":null,"confidence":0.9}"#;
        let ev: Evidence = serde_json::from_str(legacy).expect("legacy evidence deserializes");
        assert_eq!(
            ev.source, None,
            "legacy bbox_text is dropped, not resurrected"
        );
        assert_eq!(ev.page, Some(1));
    }
}
