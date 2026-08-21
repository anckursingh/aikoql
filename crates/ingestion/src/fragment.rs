//! D4.5: Knowledge fragments — coherent knowledge units between AST and IR.
//!
//! `KnowledgeFragment` is the output of boundary detection and the input to
//! semantic analysis and retrieval projection. It preserves modality — a
//! table fragment stays a table, a diagram fragment stays a diagram — so
//! interpretation is derived from the source representation, never
//! substituted for it (HLD §59).

use crate::ast::{ChartPayload, DiagramPayload, FormulaPayload, ImagePayload, TablePayload};
use crate::ir::Evidence;
use crate::source::SourceSpan;

/// A coherent unit of knowledge extracted from a document region.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeFragment {
    /// Stable id derived from document position (deterministic — see
    /// boundary.rs; prefixed with the document hash once DocumentAst
    /// carries a document_id).
    pub fragment_id: String,
    pub modality: FragmentModality,
    pub content: FragmentContent,
    #[serde(default)]
    pub context: FragmentContext,
    /// Typed source geometry (page + bbox + optional offsets).
    #[serde(default)]
    pub source: Option<SourceSpan>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    pub confidence: f32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FragmentModality {
    Text,
    Table,
    Image,
    Chart,
    Diagram,
    Formula,
    Code,
    Mixed,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum FragmentContent {
    Text(String),
    Table(TablePayload),
    Image(ImagePayload),
    Chart(ChartPayload),
    Diagram(DiagramPayload),
    Formula(FormulaPayload),
    Code(String),
    /// Composite fragment (e.g. figure + caption); children keep their
    /// own modality and provenance.
    Mixed(Vec<Box<KnowledgeFragment>>),
}

/// Surrounding structure for a fragment — heading path, page, neighbors.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct FragmentContext {
    #[serde(default)]
    pub heading_path: Vec<String>,
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub neighboring_fragments: Vec<String>,
    #[serde(default)]
    pub parent_fragment: Option<String>,
}
