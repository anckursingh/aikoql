# AIKOQL Multimodal Document Ingestion --- HLD + LLD

**Status:** Architecture proposal based on the current
`feature/mvp-launch` implementation and PR #1\
**Repository:** `anckursingh/aikoql`\
**PR:** #1 --- v0.2 encryption-at-rest + v0.3 Agent Knowledge OS
roadmap\
**Audience:** AIKOQL maintainers, Rust engineers, architects,
contributors

------------------------------------------------------------------------

## 1. Executive decision

AIKOQL should **not** become another RAG chunking framework.

The existing ingestion implementation already contains a strong semantic
pipeline:

``` text
DocumentModel
    -> DocumentAst
    -> KnowledgeIr
    -> Ontology
    -> Entity Resolution
    -> Reconciliation
    -> Knowledge Objects
```

It also has a separate:

``` text
DocumentAst
    -> DocumentChunk
    -> Embedding
```

retrieval path.

The architectural change recommended here is:

> **Make a multimodal Knowledge Representation the canonical ingestion
> product, and make text/vector/lexical/visual retrieval representations
> projections of that canonical representation.**

The target is:

``` text
Source Document
    |
    v
Physical / Multimodal Extraction
    |
    v
Canonical Multimodal Document AST
    |
    v
Knowledge Boundary Detection
    |
    v
Knowledge Fragments
    |
    v
Multimodal Semantic Analysis
    |
    v
Knowledge IR
    |
    v
Ontology / Resolution / Reconciliation
    |
    v
Knowledge Object Builder
    |
    +------------------+------------------+
    |                  |                  |
    v                  v                  v
Object Store       Graph Projection   Retrieval Projections
                                      |
                                      +-- lexical
                                      +-- text vector
                                      +-- visual vector
                                      +-- structured table
```

This preserves the current AIKOQL direction while correcting the main
architectural weakness: the current pipeline treats embedded chunks as a
parallel first-class output instead of treating retrieval as a
projection.

------------------------------------------------------------------------

# 2. What exists today

PR #1 is open against `main`, with `feature/mvp-launch` as the head. It
includes encryption-at-rest work and the v0.3 Agent Knowledge OS
maturity roadmap.

The current ingestion crate already describes itself as a
document-to-Knowledge-Object pipeline and exposes:

-   OCR
-   provider-independent AST
-   Knowledge IR
-   ontology discovery
-   entity resolution
-   reconciliation
-   chunking
-   embeddings
-   markdown compiler
-   code compiler
-   multi-source merge
-   staleness
-   context compiler
-   incremental ingestion
-   directory ingestion

The current `DocumentAst` is explicitly intended as the stable contract
between physical extraction and semantic analysis.

The current pipeline is:

``` text
D3 AST
    -> D4 IR
    -> D5 ontology
    -> D6 resolution
    -> D7 reconciliation
    -> D8 chunking + embedding
```

`CompilationResult` currently contains both:

``` rust
pub ir: KnowledgeIr,
pub embedded_chunks: Vec<EmbeddedChunk>,
```

This is the central design point to change.

------------------------------------------------------------------------

# 3. Current implementation assessment

## 3.1 Strengths --- preserve

### A. Provider-independent AST

`crates/ingestion/src/ast.rs` already defines:

``` rust
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
```

This is an excellent foundation.

Do **not** replace this with a text-only model.

### B. Bounding boxes already exist

``` rust
pub struct BoundingBox {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
```

This is critical for tables, figures, charts, diagrams and visual
provenance.

### C. Knowledge IR already models semantics

The current IR contains:

``` rust
EntityCandidate
RelationCandidate
FactCandidate
EventCandidate
TemporalAssertion
```

with provenance.

This should remain the semantic staging layer.

### D. Evidence is already first-class

The current evidence model includes:

``` rust
document_id
page
bbox_text
extractor
model
confidence
```

This is the right direction, but it needs to become multimodal and
typed.

### E. Existing chunking is useful

`DocumentChunk` already carries:

``` rust
text
position
structure
entity_mentions
evidence
```

and `EmbeddedChunk` adds:

``` rust
embedding
embedding_provider
```

This is a good retrieval implementation.

It should be retained, but demoted to a retrieval projection.

------------------------------------------------------------------------

# 4. Current architectural weaknesses

## 4.1 Document extraction currently destroys document structure

`DocumentModel` currently reduces extraction to:

``` rust
pub struct PageModel {
    pub page_number: u32,
    pub text: String,
    pub char_count: usize,
    pub source: String,
    pub ocr_confidence: Option<f32>,
}
```

This means PDF/DOCX/HTML extraction can lose:

-   table structure
-   image identity
-   chart identity
-   diagram geometry
-   formulas
-   hyperlinks
-   drawing relationships
-   captions
-   visual reading order
-   source asset references

The AST tries to reconstruct some structure later using heuristics.

That is acceptable for an MVP, but not as the long-term canonical model.

------------------------------------------------------------------------

# 5. Target architecture

## 5.1 Three-layer document model

AIKOQL should distinguish:

### Layer 1 --- Physical document

Exactly what existed in the source.

``` text
Document
  Page
    Asset
      image
      vector drawing
      table
      chart
      formula
```

### Layer 2 --- Canonical multimodal AST

Provider-independent representation.

``` text
DocumentAst
  Page
    TextBlock
    Table
    Figure
    Chart
    Diagram
    Formula
    Code
```

### Layer 3 --- Knowledge representation

Semantic interpretation.

``` text
KnowledgeFragment
    -> Entity
    -> Fact
    -> Claim
    -> Event
    -> Relation
```

Never collapse Layer 1 into Layer 3 without preserving provenance.

------------------------------------------------------------------------

# 6. New canonical types

## 6.1 SourceSpan

Replace the current string-like `bbox_text` representation with typed
source geometry.

Add to `ast.rs` or a new `source.rs`:

``` rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceSpan {
    pub document_id: Option<String>,
    pub page: u32,

    /// Character offsets where applicable.
    pub start_offset: Option<usize>,
    pub end_offset: Option<usize>,

    /// Physical page region.
    pub bbox: Option<BoundingBox>,

    /// Optional parent AST node.
    pub node_id: Option<String>,
}
```

For visual objects:

``` rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VisualAssetRef {
    pub asset_id: String,
    pub mime_type: String,
    pub content_hash: String,
    pub source: SourceSpan,
}
```

------------------------------------------------------------------------

# 7. Multimodal AST

Extend `BlockType`.

Recommended:

``` rust
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
    Figure,
    Chart,
    Diagram,
    Formula,

    Caption,
    Header,
    Footer,
    Footnote,
    Code,
    Unknown,
}
```

The distinction between `Image`, `Figure`, `Chart`, and `Diagram`
matters.

An arbitrary image is not automatically a chart.

A chart is not merely an image.

A diagram contains graph-like semantics.

------------------------------------------------------------------------

# 8. AstNode should become multimodal

Current:

``` rust
pub struct AstNode {
    pub block_type: BlockType,
    pub text: String,
    pub children: Vec<AstNode>,
    pub bbox: Option<BoundingBox>,
    pub confidence: Option<f32>,
}
```

Recommended evolution:

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AstNode {
    pub node_id: String,

    pub block_type: BlockType,

    /// Text representation if available.
    pub text: Option<String>,

    pub children: Vec<AstNode>,

    pub bbox: Option<BoundingBox>,

    pub confidence: Option<f32>,

    /// Original visual asset if this node has one.
    pub asset: Option<VisualAssetRef>,

    /// Typed structured payload.
    pub payload: Option<AstPayload>,
}
```

Add:

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum AstPayload {
    Table(TablePayload),
    Chart(ChartPayload),
    Diagram(DiagramPayload),
    Formula(FormulaPayload),
}
```

------------------------------------------------------------------------

# 9. Tables

## 9.1 Why tables need first-class representation

Never turn:

``` text
Product | 2025 | 2026
A       | 120  | 150
B       | 80   | 110
```

into:

``` text
Product 2025 2026 A 120 150 B 80 110
```

and rely on embeddings.

The row/column relationships are semantic information.

## 9.2 Rust model

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TablePayload {
    pub headers: Vec<TableHeader>,
    pub rows: Vec<TableRow>,
    pub cells: Vec<TableCell>,
    pub footnotes: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TableHeader {
    pub id: String,
    pub text: String,
    pub level: u8,
    pub parent_id: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TableRow {
    pub id: String,
    pub index: usize,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TableCell {
    pub id: String,
    pub row_id: String,
    pub column_id: String,
    pub text: String,
    pub value: Option<ScalarValue>,
    pub bbox: Option<BoundingBox>,
    pub confidence: f32,
}
```

Add:

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum ScalarValue {
    Text(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Date(String),
    Currency {
        amount: f64,
        currency: String,
    },
}
```

The original text must remain available.

------------------------------------------------------------------------

# 10. Charts

Charts must retain both visual evidence and structured interpretation.

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ChartPayload {
    pub chart_type: ChartType,
    pub title: Option<String>,
    pub x_axis: Option<Axis>,
    pub y_axis: Option<Axis>,
    pub series: Vec<ChartSeries>,
    pub extracted_data: Option<TablePayload>,
}
```

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum ChartType {
    Bar,
    Line,
    Pie,
    Scatter,
    Area,
    Histogram,
    Combo,
    Unknown,
}
```

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Axis {
    pub label: Option<String>,
    pub unit: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}
```

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ChartSeries {
    pub name: String,
    pub values: Vec<ChartPoint>,
}
```

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ChartPoint {
    pub x: String,
    pub y: f64,
    pub confidence: f32,
    pub bbox: Option<BoundingBox>,
}
```

A chart should therefore produce:

``` text
Visual chart
    +
structured data
    +
semantic interpretation
    +
source evidence
```

------------------------------------------------------------------------

# 11. Diagrams

Diagrams are especially valuable for AIKOQL because they naturally map
to relationships.

Example:

``` text
Client -> Gateway -> Payment Service -> Ledger
```

should become:

``` text
DiagramPayload
  nodes:
    Client
    Gateway
    Payment Service
    Ledger

  edges:
    Client -> Gateway
    Gateway -> Payment Service
    Payment Service -> Ledger
```

Rust:

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DiagramPayload {
    pub nodes: Vec<DiagramNode>,
    pub edges: Vec<DiagramEdge>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DiagramNode {
    pub id: String,
    pub label: String,
    pub node_type: Option<String>,
    pub bbox: Option<BoundingBox>,
    pub confidence: f32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DiagramEdge {
    pub source: String,
    pub target: String,
    pub label: Option<String>,
    pub confidence: f32,
    pub bbox: Option<BoundingBox>,
}
```

The visual diagram remains evidence.

The extracted graph becomes knowledge.

------------------------------------------------------------------------

# 12. Formulas

Do not store formulas only as OCR text.

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FormulaPayload {
    pub latex: Option<String>,
    pub mathml: Option<String>,
    pub plain_text: Option<String>,
}
```

This allows future mathematical reasoning while preserving the original
formula.

------------------------------------------------------------------------

# 13. Images

Images should support multiple representations.

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ImagePayload {
    pub asset: VisualAssetRef,
    pub ocr_text: Option<String>,
    pub caption: Option<String>,
    pub detected_objects: Vec<DetectedObject>,
    pub visual_embedding: Option<Vec<f32>>,
}
```

Do not make the generated caption the canonical representation.

It is only one derived representation.

------------------------------------------------------------------------

# 14. Multimodal Evidence

Current:

``` rust
pub struct Evidence {
    pub document_id: Option<String>,
    pub page: Option<u32>,
    pub bbox_text: Option<String>,
    pub extractor: String,
    pub model: Option<String>,
    pub confidence: f32,
}
```

Change to:

``` rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Evidence {
    pub document_id: Option<String>,
    pub page: Option<u32>,

    pub source: EvidenceSource,

    pub extractor: String,
    pub model: Option<String>,

    pub confidence: f32,
}
```

``` rust
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
```

This is a major AIKOQL differentiator.

A claim can now have evidence from:

``` text
paragraph
+
table cell
+
chart
+
diagram
```

------------------------------------------------------------------------

# 15. KnowledgeFragment

Introduce a new abstraction between AST and semantic IR.

New file:

``` text
crates/ingestion/src/fragment.rs
```

Recommended model:

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeFragment {
    pub fragment_id: String,

    pub modality: FragmentModality,

    pub content: FragmentContent,

    pub context: FragmentContext,

    pub evidence: Vec<Evidence>,

    pub confidence: f32,
}
```

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
```

``` rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum FragmentContent {
    Text(String),
    Table(TablePayload),
    Image(ImagePayload),
    Chart(ChartPayload),
    Diagram(DiagramPayload),
    Formula(FormulaPayload),
    Code(String),
    Mixed(Vec<Box<KnowledgeFragment>>),
}
```

Context:

``` rust
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct FragmentContext {
    pub heading_path: Vec<String>,
    pub page: Option<u32>,
    pub neighboring_fragments: Vec<String>,
    pub parent_fragment: Option<String>,
}
```

------------------------------------------------------------------------

# 16. Semantic boundary detection

Do not call this a sentence tokenizer.

Introduce:

``` rust
pub trait KnowledgeBoundaryDetector: Send + Sync {
    fn name(&self) -> &str;

    fn detect(
        &self,
        ast: &DocumentAst,
    ) -> Result<Vec<KnowledgeFragment>, BoundaryError>;
}
```

Implement:

``` text
RuleBoundaryDetector
EmbeddingBoundaryDetector
TransformerBoundaryDetector
HybridBoundaryDetector
```

The first production implementation should be:

``` text
HybridBoundaryDetector
```

with:

``` text
structure
+
sentence boundaries
+
semantic similarity
+
modality boundaries
+
optional transformer score
```

A transformer is a pluggable implementation, not an architectural
dependency.

------------------------------------------------------------------------

# 17. Why transformer belongs here

The transformer should answer:

> "Does this boundary separate two semantically distinct knowledge
> units?"

Not:

> "How do I split text into chunks?"

Example:

``` text
Sentence A
Sentence B
Sentence C
----------------
Sentence D
Sentence E
```

If semantic similarity between C and D drops sharply, create a boundary.

However, hard boundaries must still exist around:

-   table
-   figure
-   chart
-   diagram
-   formula
-   code
-   heading
-   page/section transitions when required

Therefore:

``` text
Boundary score =
    structural_score
  + linguistic_score
  + semantic_score
  + modality_score
```

------------------------------------------------------------------------

# 18. SemanticAnalyzer change

Current:

``` rust
pub trait SemanticAnalyzer: Send + Sync {
    fn name(&self) -> &str;
    fn analyze(&self, ast: &DocumentAst) -> KnowledgeIr;
}
```

Recommended:

``` rust
pub trait SemanticAnalyzer: Send + Sync {
    fn name(&self) -> &str;

    fn analyze(
        &self,
        fragments: &[KnowledgeFragment],
    ) -> Result<KnowledgeIr, SemanticAnalysisError>;
}
```

The semantic analyzer now operates over coherent knowledge units instead
of having to rediscover segmentation from the entire AST.

------------------------------------------------------------------------

# 19. Knowledge IR enhancement

Current IR:

``` rust
pub struct KnowledgeIr {
    pub entities: Vec<EntityCandidate>,
    pub relations: Vec<RelationCandidate>,
    pub facts: Vec<FactCandidate>,
    pub events: Vec<EventCandidate>,
    pub temporal: Vec<TemporalAssertion>,
    pub document_id: Option<String>,
    pub content_trust: Option<ContentTrust>,
    pub page_count: u32,
    pub extractor: String,
}
```

Recommended:

``` rust
pub struct KnowledgeIr {
    pub fragments: Vec<KnowledgeFragment>,

    pub entities: Vec<EntityCandidate>,
    pub relations: Vec<RelationCandidate>,
    pub facts: Vec<FactCandidate>,
    pub events: Vec<EventCandidate>,
    pub temporal: Vec<TemporalAssertion>,

    pub document_id: Option<String>,
    pub content_trust: Option<ContentTrust>,
    pub page_count: u32,
    pub extractor: String,
}
```

Every candidate should reference source fragments.

For example:

``` rust
pub struct FactCandidate {
    pub statement: String,
    pub entities: Vec<String>,
    pub source_fragments: Vec<String>,
    pub confidence: f32,
    pub evidence: Vec<Evidence>,
}
```

Similarly:

``` rust
pub struct RelationCandidate {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub source_fragments: Vec<String>,
    pub confidence: f32,
    pub evidence: Vec<Evidence>,
}
```

This avoids forcing all provenance into a single page-level evidence
object.

------------------------------------------------------------------------

# 20. Entity candidates need better provenance

Current:

``` rust
pub mentions: Vec<String>
pub evidence: Evidence
```

Recommended:

``` rust
pub mentions: Vec<EntityMention>
pub evidence: Vec<Evidence>
```

``` rust
pub struct EntityMention {
    pub text: String,
    pub fragment_id: String,
    pub source: EvidenceSource,
    pub confidence: f32,
}
```

This is necessary when the same entity occurs:

-   in prose
-   in a table
-   in a diagram
-   in a chart legend

------------------------------------------------------------------------

# 21. Retrieval projection

Current `DocumentChunk` should remain.

Add:

``` rust
pub struct RetrievalProjection {
    pub projection_id: String,
    pub fragment_ids: Vec<String>,
    pub modality: RetrievalModality,
    pub text: Option<String>,
    pub metadata: RetrievalMetadata,
}
```

``` rust
pub enum RetrievalModality {
    Text,
    Table,
    Visual,
    Mixed,
}
```

Then:

``` rust
pub trait RetrievalProjector: Send + Sync {
    fn name(&self) -> &str;

    fn project(
        &self,
        fragments: &[KnowledgeFragment],
    ) -> Vec<RetrievalProjection>;
}
```

The existing `DocumentChunker` can be adapted:

``` rust
impl RetrievalProjector for DocumentChunkerAdapter {
    ...
}
```

------------------------------------------------------------------------

# 22. Refactor chunking.rs rather than deleting it

Current:

``` rust
pub trait DocumentChunker {
    fn chunk(
        &self,
        ast: &DocumentAst,
        ir: Option<&KnowledgeIr>
    ) -> Vec<DocumentChunk>;
}
```

Target:

``` rust
pub trait DocumentChunker: Send + Sync {
    fn name(&self) -> &str;

    fn chunk(
        &self,
        fragments: &[KnowledgeFragment],
    ) -> Vec<DocumentChunk>;
}
```

The chunker should no longer be responsible for semantic segmentation.

It should answer:

> "How should knowledge fragments be packaged for this retrieval
> backend?"

------------------------------------------------------------------------

# 23. Embeddings need modality awareness

Current:

``` rust
pub trait EmbeddingProvider: Send + Sync {
    fn name(&self) -> &str;
    fn dimensions(&self) -> usize;
    fn embed(&self, text: &str) -> Vec<f32>;
}
```

Keep this trait for text.

Add:

``` rust
pub trait MultimodalEmbeddingProvider: Send + Sync {
    fn name(&self) -> &str;

    fn dimensions(&self) -> usize;

    fn embed_text(&self, text: &str) -> Vec<f32>;

    fn embed_image(&self, image: &[u8]) -> Vec<f32>;

    fn embed_multimodal(
        &self,
        input: &MultimodalEmbeddingInput,
    ) -> Vec<f32>;
}
```

``` rust
pub enum MultimodalEmbeddingInput<'a> {
    Text(&'a str),
    Image(&'a [u8]),
    TextImage {
        text: &'a str,
        image: &'a [u8],
    },
}
```

Do not require this provider in the base build.

The architecture must work without a multimodal model.

------------------------------------------------------------------------

# 24. Visual retrieval

Add:

``` rust
pub struct VisualIndexRecord {
    pub asset_id: String,
    pub document_id: String,
    pub page: u32,
    pub bbox: BoundingBox,
    pub embedding: Vec<f32>,
    pub semantic_caption: Option<String>,
    pub fragment_ids: Vec<String>,
}
```

This allows:

``` text
query
  -> visual similarity
  -> visual object
  -> KnowledgeFragment
  -> Knowledge Object
  -> evidence
```

The visual index is therefore an access path, not the source of truth.

------------------------------------------------------------------------

# 25. Pipeline refactor

Current `compile_document()` does:

``` rust
let ast = document_model_to_ast(doc);

let raw_ir = document_model_to_ir(doc, analyzer);

let ontology = discover_ontology_from_ir(&ir);

let resolution = resolve_entities(...);

let commit_plan = reconcile_and_plan(...);

let embedded_chunks =
    chunk_and_embed(&ast, Some(&ir), chunker, embedder);
```

The target is:

``` rust
let ast = document_model_to_ast(doc)?;

let fragments =
    boundary_detector.detect(&ast)?;

let ir =
    analyzer.analyze(&fragments)?;

let ontology =
    discover_ontology_from_ir(&ir);

let resolution =
    resolve_entities(&ir, resolver, existing_kos);

let commit_plan =
    reconcile_and_plan(
        &ir,
        &ontology,
        &resolution,
        existing_kos,
        reconciler,
    );

let retrieval =
    retrieval_projector.project(&ir.fragments);

let embedded =
    embed_retrieval_projections(
        &retrieval,
        embedder,
    );
```

The crucial change is:

``` text
AST -> fragments -> IR
```

rather than:

``` text
AST -> IR
AST -> chunks
```

------------------------------------------------------------------------

# 26. New compilation result

Recommended:

``` rust
pub struct CompilationResult {
    pub document_ast: DocumentAst,

    pub fragments: Vec<KnowledgeFragment>,

    pub ir: KnowledgeIr,

    pub ontology: OntologyProposal,

    pub resolution: ResolutionResult,

    pub commit_plan: KnowledgeCommitPlan,

    pub retrieval_projections: Vec<RetrievalProjection>,

    pub embedded_chunks: Vec<EmbeddedChunk>,

    pub evidence_trail: EvidenceTrail,

    pub stats: PipelineStats,

    pub secret_findings: Vec<SecretFinding>,
}
```

This makes the stages observable and testable.

------------------------------------------------------------------------

# 27. Extraction architecture

The current `extract_document()` only supports:

``` text
PDF
DOCX
HTML
text/*
```

and mostly converts them into page text.

This must evolve toward an extractor registry.

``` rust
pub trait DocumentExtractor: Send + Sync {
    fn name(&self) -> &str;

    fn supported_types(&self) -> &[&str];

    fn extract(
        &self,
        source: &DocumentSource,
    ) -> Result<DocumentModel, ExtractionError>;
}
```

``` rust
pub struct DocumentSource {
    pub path: String,
    pub mime_type: String,
    pub metadata: HashMap<String, String>,
}
```

Then:

``` rust
pub struct ExtractorRegistry {
    extractors: Vec<Box<dyn DocumentExtractor>>,
}
```

The registry selects the appropriate extractor.

------------------------------------------------------------------------

# 28. Separate physical extraction from semantic interpretation

Do not make the PDF parser call an LLM.

Correct:

``` text
PDF parser
    -> physical document model

OCR
    -> physical text/layout

table parser
    -> physical table

image extractor
    -> asset

chart detector
    -> chart candidate

diagram detector
    -> diagram candidate

          ↓

Canonical AST

          ↓

Semantic analyzer
```

This separation keeps the ingestion crate testable and avoids provider
lock-in.

------------------------------------------------------------------------

# 29. PDF pipeline

Target:

``` text
PDF
 |
 +-- native text extraction
 |
 +-- page geometry
 |
 +-- image extraction
 |
 +-- vector graphics extraction
 |
 +-- table detection
 |
 +-- chart/figure classification
 |
 +-- OCR only where required
 |
 v
DocumentModel
```

OCR should remain a fallback for pages or regions that require it.

Do not OCR the entire document unnecessarily.

------------------------------------------------------------------------

# 30. DOCX pipeline

The current implementation reads `word/document.xml` and strips XML
tags.

This is insufficient for multimodal ingestion.

DOCX extraction should preserve:

-   paragraphs
-   runs
-   headings
-   tables
-   images
-   captions
-   hyperlinks
-   drawing relationships
-   styles

Target:

``` text
DOCX
 |
 +-- document.xml
 +-- styles.xml
 +-- relationships
 +-- media/*
 |
 v
Canonical DocumentModel
```

Do not use `strip_xml_tags()` as the long-term DOCX parser.

It is acceptable only as a minimal fallback.

------------------------------------------------------------------------

# 31. HTML pipeline

The current HTML implementation strips tags.

This loses:

-   headings
-   tables
-   images
-   alt text
-   links
-   semantic sections
-   lists

Replace:

``` rust
strip_xml_tags(&html)
```

with an HTML parser that creates the same canonical AST used by
PDF/DOCX.

The point is:

> All source formats converge into the same `DocumentAst`.

------------------------------------------------------------------------

# 32. Multimodal processing strategy

Every visual element should go through classification:

``` text
Visual Asset
 |
 +-- photograph
 +-- diagram
 +-- chart
 +-- scanned text
 +-- screenshot
 +-- formula
 +-- unknown
```

The classifier should be pluggable:

``` rust
pub trait VisualClassifier: Send + Sync {
    fn classify(
        &self,
        asset: &VisualAsset,
    ) -> Result<VisualClassification, VisualError>;
}
```

``` rust
pub enum VisualClassification {
    Image,
    Chart,
    Diagram,
    Formula,
    Screenshot,
    ScannedText,
    Unknown,
}
```

------------------------------------------------------------------------

# 33. Avoid expensive model invocation

Use staged processing.

``` text
                    visual asset
                         |
                         v
                 cheap classifier
                         |
          +--------------+--------------+
          |              |              |
        text          table/chart      diagram
          |              |              |
       OCR only       specialist       VLM
       if needed       parser          if needed
```

Do not invoke a VLM for every image.

This is critical for local/self-hosted AIKOQL.

------------------------------------------------------------------------

# 34. Knowledge Object construction

The current ingestion path is entity-oriented.

Do not restrict the future model to:

``` text
one KO per entity
```

Introduce object types such as:

``` text
Entity
Claim
Fact
Event
Observation
Table
Chart
Diagram
Document
```

Example:

``` text
KO: Company/Acme

KO: Acquisition/123

KO: Company/Globex

KO: Chart/RevenueChart-4

KO: Fact/Revenue-2025
```

Relationships:

``` text
Acme
  --acquired-->
Globex

Revenue-2025
  --about-->
Acme

Revenue-2025
  --supported_by-->
RevenueChart-4
```

------------------------------------------------------------------------

# 35. Evidence graph

The final knowledge model should preserve:

``` text
Knowledge Object
    |
    +-- derived_from --> KnowledgeFragment
                              |
                              +-- source_span
                              +-- table_cell
                              +-- diagram_edge
                              +-- chart_point
                              +-- image_region
                              +-- text_span
```

This enables explainability.

An agent can answer:

> Why do you believe this relationship exists?

with:

``` text
KO relationship
   ->
Evidence
   ->
diagram edge
   ->
page 14
   ->
original visual asset
```

------------------------------------------------------------------------

# 36. Query implications

AIKOQL should eventually support multiple query surfaces.

### Text

``` sql
MATCH Document
SIMILAR "device identity"
RETURN *
```

### Structured table

Conceptually:

``` sql
MATCH Table
WHERE columns CONTAIN "revenue"
RETURN rows
```

### Knowledge

``` sql
MATCH Company
TRAVERSE acquired
RETURN target
```

### Evidence

``` sql
MATCH Relation
WHERE subject = "Acme"
SHOW EVIDENCE
```

### Multimodal

Conceptually:

``` sql
MATCH Visual
SIMILAR IMAGE ...
RETURN RELATED KNOWLEDGE
```

The exact syntax should be designed later.

The ingestion architecture must simply preserve the information required
to support it.

------------------------------------------------------------------------

# 37. Current chunking implementation: specific changes

## Keep

-   heading-aware chunking
-   paragraph-aware splitting
-   fixed window strategy
-   heading paths
-   entity enrichment
-   evidence

## Change

Remove semantic ownership from the chunker.

Current:

``` text
DocumentChunker
    = semantic segmentation + retrieval segmentation
```

Target:

``` text
KnowledgeBoundaryDetector
    = semantic segmentation

DocumentChunker
    = retrieval segmentation
```

This distinction is mandatory.

------------------------------------------------------------------------

# 38. Current mock embedding implementation

The current character n-gram implementation is appropriate as a
deterministic test double.

It should remain.

Do not pretend it is a production semantic embedding.

Use:

``` text
MockEmbeddingProvider
```

for:

-   deterministic tests
-   pipeline tests
-   CI
-   offline development

Add real providers later behind the existing trait.

------------------------------------------------------------------------

# 39. Transformer implementation plan

Do not make a transformer mandatory.

Phase 1:

``` text
RuleBoundaryDetector
```

Phase 2:

``` text
EmbeddingBoundaryDetector
```

Phase 3:

``` text
TransformerBoundaryDetector
```

Phase 4:

``` text
HybridBoundaryDetector
```

Recommended production architecture:

``` text
HybridBoundaryDetector
 |
 +-- structural boundary
 +-- linguistic boundary
 +-- semantic similarity
 +-- transformer score
 +-- modality transition
```

The transformer should produce a score:

``` rust
pub struct BoundaryScore {
    pub probability: f32,
    pub model: String,
}
```

The final decision belongs to the boundary policy.

------------------------------------------------------------------------

# 40. Rust error model

Avoid:

``` rust
Result<T, String>
```

for the new pipeline.

Introduce typed errors:

``` rust
#[derive(Debug, thiserror::Error)]
pub enum IngestionError {
    #[error("unsupported MIME type: {0}")]
    UnsupportedMimeType(String),

    #[error("extraction failed: {0}")]
    Extraction(String),

    #[error("AST construction failed: {0}")]
    Ast(String),

    #[error("boundary detection failed: {0}")]
    Boundary(String),

    #[error("semantic analysis failed: {0}")]
    Semantic(String),

    #[error("retrieval projection failed: {0}")]
    Projection(String),

    #[error("embedding failed: {0}")]
    Embedding(String),

    #[error("invalid document: {0}")]
    InvalidDocument(String),
}
```

This should be used throughout the new path.

------------------------------------------------------------------------

# 41. Async architecture

The current trait design is synchronous.

For local CPU-heavy processing, this is acceptable.

For real model providers, the architecture should eventually support:

``` rust
#[async_trait::async_trait]
pub trait AsyncSemanticAnalyzer: Send + Sync {
    async fn analyze(
        &self,
        fragments: &[KnowledgeFragment],
    ) -> Result<KnowledgeIr, SemanticAnalysisError>;
}
```

Do not immediately convert the entire ingestion crate to async.

Instead, define clear boundaries so expensive remote/local inference can
later be isolated.

------------------------------------------------------------------------

# 42. Concurrency

Recommended parallelism:

``` text
Document
 |
 +-- Page 1 ──+
 +-- Page 2 ──+
 +-- Page 3 ──+--> extraction
 +-- Page 4 ──+
```

Then:

``` text
AST assembly
    |
    v
semantic batching
    |
    v
IR
```

Use bounded concurrency.

Never spawn unbounded tasks for:

-   pages
-   images
-   OCR
-   VLM calls
-   embeddings

A configurable semaphore should control expensive operations.

------------------------------------------------------------------------

# 43. Caching

Multimodal extraction will be expensive.

Add content-addressed caching:

``` text
sha256(asset)
    |
    v
ExtractionCache
```

Cache keys should include:

``` text
document hash
+
extractor version
+
model version
+
configuration version
```

Example:

``` rust
pub struct ProcessingCacheKey {
    pub content_hash: String,
    pub processor: String,
    pub processor_version: String,
    pub model: Option<String>,
}
```

This makes re-ingestion significantly cheaper.

------------------------------------------------------------------------

# 44. Idempotency

The ingestion system must be deterministic at the object/provenance
level.

A document should have:

``` text
document_content_hash
```

and each extracted asset:

``` text
asset_content_hash
```

Knowledge fragments should have stable IDs derived from:

``` text
document hash
+
source span
+
fragment semantic version
```

Do not generate random IDs for persistent source fragments unless
necessary.

------------------------------------------------------------------------

# 45. Incremental ingestion

The current project already has incremental ingestion.

Extend it to asset-level changes.

Instead of:

``` text
document changed
    -> reprocess entire document
```

support:

``` text
document hash unchanged
    -> skip

page changed
    -> process page

image hash changed
    -> process image

table region changed
    -> process table

semantic model changed
    -> re-run semantic projection only
```

This is one of the biggest long-term performance benefits of making the
representation layered.

------------------------------------------------------------------------

# 46. Model-version independence

Persist model provenance:

``` rust
pub struct DerivationMetadata {
    pub extractor: String,
    pub extractor_version: String,
    pub model: Option<String>,
    pub model_version: Option<String>,
    pub timestamp: u64,
}
```

Then:

``` text
Document
   |
   +-- AST v1
   |
   +-- Semantic IR v1
   |
   +-- Visual extraction v2
```

A new model should be able to regenerate projections without destroying
the canonical source representation.

------------------------------------------------------------------------

# 47. Pipeline phases after refactor

Replace:

``` text
D3 AST
D4 IR
D5 Ontology
D6 Resolution
D7 Reconciliation
D8 Chunking
```

with:

``` text
D1 Source Acquisition
D2 Physical Extraction
D3 Multimodal AST
D4 Knowledge Boundary Detection
D5 Knowledge Fragments
D6 Semantic IR
D7 Ontology Discovery
D8 Entity Resolution
D9 Reconciliation
D10 Knowledge Object Build
D11 Retrieval Projection
D12 Embedding / Indexing
```

The existing D3-D8 naming can be preserved temporarily for
compatibility, but the implementation should move toward this logical
model.

------------------------------------------------------------------------

# 48. Revised pipeline API

Recommended:

``` rust
pub struct IngestionPipeline<'a> {
    pub boundary_detector: &'a dyn KnowledgeBoundaryDetector,
    pub analyzer: &'a dyn SemanticAnalyzer,
    pub resolver: &'a dyn EntityResolver,
    pub reconciler: &'a dyn KnowledgeReconciler,
    pub projector: &'a dyn RetrievalProjector,
    pub embedder: &'a dyn EmbeddingProvider,
}
```

Then:

``` rust
impl<'a> IngestionPipeline<'a> {
    pub fn compile(
        &self,
        doc: &DocumentModel,
        existing_kos: &[KnowledgeBaseEntry],
    ) -> Result<CompilationResult, IngestionError> {
        let ast = document_model_to_ast(doc);

        let fragments =
            self.boundary_detector.detect(&ast)
                .map_err(IngestionError::Boundary)?;

        let ir =
            self.analyzer.analyze(&fragments)
                .map_err(IngestionError::Semantic)?;

        let ontology = discover_ontology_from_ir(&ir);

        let resolution =
            resolve_entities(&ir, self.resolver, existing_kos);

        let commit_plan =
            reconcile_and_plan(
                &ir,
                &ontology,
                &resolution,
                existing_kos,
                self.reconciler,
            );

        let projections =
            self.projector.project(&fragments);

        let embedded_chunks =
            embed_projections(&projections, self.embedder)
                .map_err(IngestionError::Embedding)?;

        Ok(CompilationResult {
            document_ast: ast,
            fragments,
            ir,
            ontology,
            resolution,
            commit_plan,
            retrieval_projections: projections,
            embedded_chunks,
            ..Default::default()
        })
    }
}
```

------------------------------------------------------------------------

# 49. Compatibility strategy

Do not rewrite everything in one PR.

## Phase 1 --- internal compatibility

Keep:

``` rust
compile_document(...)
```

but implement it through:

``` text
IngestionPipeline
```

behind the existing API.

## Phase 2

Add:

``` rust
compile_document_multimodal(...)
```

for experimentation.

## Phase 3

Make the multimodal pipeline the default.

## Phase 4

Deprecate direct AST-to-chunk APIs.

------------------------------------------------------------------------

# 50. Recommended module layout

Target:

``` text
crates/ingestion/src/
|
+-- source/
|   +-- mod.rs
|   +-- pdf.rs
|   +-- docx.rs
|   +-- html.rs
|   +-- text.rs
|
+-- document/
|   +-- mod.rs
|   +-- model.rs
|   +-- spans.rs
|   +-- assets.rs
|
+-- ast.rs
|
+-- fragment.rs
|
+-- boundary/
|   +-- mod.rs
|   +-- rule.rs
|   +-- embedding.rs
|   +-- transformer.rs
|   +-- hybrid.rs
|
+-- visual/
|   +-- mod.rs
|   +-- classifier.rs
|   +-- image.rs
|   +-- chart.rs
|   +-- diagram.rs
|   +-- formula.rs
|
+-- table/
|   +-- mod.rs
|   +-- parser.rs
|
+-- ir.rs
+-- ontology.rs
+-- resolution.rs
+-- commit.rs
|
+-- projection/
|   +-- mod.rs
|   +-- text.rs
|   +-- table.rs
|   +-- visual.rs
|
+-- chunking.rs
+-- embedding.rs
+-- pipeline.rs
```

Do not create all directories immediately. Introduce them as
implementation work requires.

------------------------------------------------------------------------

# 51. Testing architecture

AIKOQL's ingestion tests must become multimodal.

## Text tests

``` text
sentence boundary
heading hierarchy
entity extraction
relation extraction
fact extraction
temporal extraction
```

## Table tests

``` text
row reconstruction
column reconstruction
merged cells
nested headers
numeric values
currency
footnotes
```

## Chart tests

``` text
chart type
axis extraction
series extraction
data point extraction
trend extraction
```

## Diagram tests

``` text
node extraction
edge extraction
relationship direction
labels
confidence
```

## Image tests

``` text
OCR
caption
classification
visual embedding
asset provenance
```

## Cross-modal tests

``` text
paragraph -> chart
paragraph -> table
diagram -> text
chart -> claim
table -> claim
```

------------------------------------------------------------------------

# 52. Golden document suite

Create:

``` text
tests/fixtures/multimodal/
|
+-- plain-text.pdf
+-- scanned.pdf
+-- tables.pdf
+-- complex-table.pdf
+-- charts.pdf
+-- architecture-diagram.pdf
+-- mixed-report.pdf
+-- formulas.pdf
+-- images.pdf
+-- annual-report.pdf
```

For each fixture store expected:

``` text
AST
fragments
entities
facts
relations
evidence
retrieval projections
```

Use golden JSON snapshots.

------------------------------------------------------------------------

# 53. Evaluation metrics

Do not only measure final LLM answer quality.

Measure each stage.

## Extraction

``` text
layout accuracy
table cell accuracy
OCR accuracy
asset detection
```

## Fragmentation

``` text
boundary precision
boundary recall
semantic coherence
```

## Semantic extraction

``` text
entity precision / recall
relation precision / recall
fact accuracy
event accuracy
```

## Visual

``` text
chart data accuracy
diagram edge accuracy
visual classification accuracy
```

## Provenance

``` text
evidence resolution accuracy
source-span accuracy
```

## Retrieval

``` text
Recall@K
MRR
NDCG
hybrid retrieval recall
visual retrieval recall
```

## End-to-end

``` text
answer correctness
citation correctness
evidence correctness
```

------------------------------------------------------------------------

# 54. Security considerations

Multimodal ingestion creates new attack surfaces.

Maintain the existing secret filtering and content trust model.

Additionally:

### Image prompt injection

An image may contain:

``` text
"Ignore previous instructions..."
```

OCR/VLM output must be treated as untrusted content.

### Document prompt injection

Do not allow extracted instructions to alter ingestion policy.

### Model output

All model-generated claims must carry:

``` text
extractor
model
confidence
source evidence
```

### Asset limits

Enforce:

``` text
maximum file size
maximum page count
maximum image dimensions
maximum image count
maximum OCR time
maximum VLM calls
```

------------------------------------------------------------------------

# 55. Performance model

The pipeline should distinguish:

### Cheap

``` text
hashing
native text extraction
AST construction
basic classification
```

### Moderate

``` text
OCR
table parsing
embeddings
```

### Expensive

``` text
VLM
transformer semantic boundary detection
complex chart extraction
diagram understanding
```

Use a policy:

``` rust
pub struct ProcessingPolicy {
    pub enable_ocr: bool,
    pub enable_visual_analysis: bool,
    pub enable_chart_analysis: bool,
    pub enable_diagram_analysis: bool,
    pub enable_transformer_boundaries: bool,

    pub max_visual_assets: usize,
    pub max_model_calls: usize,
}
```

This is essential for a self-hosted database.

------------------------------------------------------------------------

# 56. What NOT to build

AIKOQL should deliberately avoid:

1.  A proprietary replacement for every PDF parser.
2.  A mandatory giant VLM.
3.  A mandatory transformer dependency.
4.  A vector database disguised as the Knowledge Object store.
5.  Turning every sentence into a Knowledge Object.
6.  Replacing original evidence with generated captions.
7.  Making embeddings the canonical representation.
8.  Making GraphRAG the product architecture.
9.  Reprocessing unchanged assets unnecessarily.

------------------------------------------------------------------------

# 57. Recommended implementation sequence

## PR-A --- Canonical multimodal AST

Implement:

``` text
SourceSpan
VisualAssetRef
AstPayload
ChartPayload
DiagramPayload
FormulaPayload
TablePayload
```

No model changes yet.

## PR-B --- Extraction preservation

Improve:

``` text
PDF
DOCX
HTML
```

to preserve multimodal structure.

Do not add expensive AI yet.

## PR-C --- KnowledgeFragment

Add:

``` text
fragment.rs
KnowledgeFragment
FragmentContent
FragmentContext
```

and rule-based boundary detection.

## PR-D --- Semantic pipeline

Change:

``` text
AST -> IR
```

to:

``` text
AST -> Fragment -> IR
```

## PR-E --- Retrieval projection

Refactor chunking:

``` text
Fragment -> RetrievalProjection -> DocumentChunk -> Embedding
```

## PR-F --- Visual analysis

Add:

``` text
VisualClassifier
ChartAnalyzer
DiagramAnalyzer
ImageAnalyzer
```

with mock implementations first.

## PR-G --- Transformer boundary detector

Add the transformer only after baseline metrics exist.

## PR-H --- Multimodal retrieval

Add visual embeddings and visual index.

## PR-I --- Query surface

Expose multimodal evidence through AIKOQL.

------------------------------------------------------------------------

# 58. Definition of done

The ingestion redesign is complete when:

-   [ ] Text documents work as well as current implementation.
-   [ ] Tables remain structured.
-   [ ] Chart data can be represented structurally.
-   [ ] Diagram nodes/edges can be represented.
-   [ ] Images retain original assets.
-   [ ] Formulas retain mathematical representation.
-   [ ] Every semantic candidate has typed provenance.
-   [ ] Every visual-derived fact can resolve to a page/region.
-   [ ] Retrieval chunks are derived projections.
-   [ ] Transformer boundary detection is optional.
-   [ ] Model versions are persisted.
-   [ ] Asset processing is content-addressed.
-   [ ] Incremental ingestion works at asset/page level.
-   [ ] No mandatory heavyweight AI dependency is introduced.
-   [ ] Existing K1-K5 kernel semantics remain intact.
-   [ ] Existing encryption/security behavior remains intact.
-   [ ] Existing ingestion tests remain green.
-   [ ] Multimodal golden fixtures exist.
-   [ ] CI measures extraction and semantic regression.

------------------------------------------------------------------------

# 59. Final architectural position

The core AIKOQL differentiator should be:

``` text
Traditional RAG:

Document
   -> text
   -> chunks
   -> embeddings
   -> retrieval
   -> LLM


AIKOQL:

Document
   -> multimodal canonical representation
   -> knowledge fragments
   -> semantic knowledge
   -> provenance
   -> Knowledge Objects
   -> multiple retrieval projections
   -> AIKOQL query
   -> agent / LLM
```

The critical architectural invariant is:

> **Never destroy information merely to make retrieval easier.**

A table should remain a table.

A chart should remain a chart.

A diagram should remain a diagram.

An image should remain an image.

The semantic interpretation should be **derived from the original
representation**, not substituted for it.

That gives AIKOQL five independent but connected representations:

``` text
                 Knowledge Object
                       |
       +---------------+---------------+
       |               |               |
       v               v               v
    Semantic        Graph          Retrieval
       |                               |
       |                    +----------+----------+
       |                    |                     |
       v                    v                     v
   Evidence             Text Vector          Visual Vector
```

This is the architecture that keeps AIKOQL focused on its original
objective: **an AI-native knowledge database/query system**, rather than
turning the project into another RAG framework.

------------------------------------------------------------------------

# 60. Immediate engineering recommendation

Do **not** begin by adding a transformer.

The first implementation milestone should be:

``` text
DocumentModel
     |
     v
Multimodal DocumentAst
     |
     v
KnowledgeFragment
     |
     v
KnowledgeIr
```

Specifically, implement these five primitives first:

``` text
1. SourceSpan
2. VisualAssetRef
3. AstPayload
4. KnowledgeFragment
5. KnowledgeBoundaryDetector
```

Then refactor the current `DocumentChunker` into a retrieval projection.

Once that exists, benchmark:

``` text
Rule boundary
vs
Embedding boundary
vs
Transformer boundary
vs
Hybrid boundary
```

The transformer decision should be based on measured improvement in:

``` text
boundary quality
fact extraction
relation extraction
retrieval recall
ingestion cost
latency
```

rather than intuition.

------------------------------------------------------------------------

## Appendix A --- Mapping current files to target architecture

  -----------------------------------------------------------------------------
  Current file                  Current role            Target role
  ----------------------------- ----------------------- -----------------------
  `src/lib.rs`                  extraction + public     source/document API
                                exports                 

  `src/ast.rs`                  structural AST          multimodal canonical
                                                        AST

  `src/ir.rs`                   semantic IR             semantic IR + fragment
                                                        references

  `src/chunking.rs`             segmentation +          retrieval projection
                                retrieval               

  `src/embedding.rs`            text vectors            text embedding provider

  `src/ocr.rs`                  OCR                     physical extraction

  `src/pipeline.rs`             D3-D8 orchestration     full ingestion
                                                        orchestrator

  `src/ontology.rs`             ontology discovery      retain

  `src/resolution.rs`           entity resolution       retain

  `src/commit.rs`               reconciliation          retain

  `src/ingest_dir.rs`           filesystem ingestion    retain, route through
                                                        new pipeline

  `src/ingest_incremental.rs`   incremental processing  extend to assets

  `src/context.rs`              context compilation     eventually consume
                                                        multimodal evidence

  `src/merge.rs`                multi-source merge      retain, extend
                                                        fragment/evidence
                                                        provenance
  -----------------------------------------------------------------------------

------------------------------------------------------------------------

## Appendix B --- Architectural invariants

### Invariant 1

`DocumentAst` is provider-independent.

### Invariant 2

Original source evidence is never replaced by model-generated text.

### Invariant 3

Knowledge Objects are the canonical semantic representation.

### Invariant 4

Embeddings are projections, not source of truth.

### Invariant 5

Visual information is first-class.

### Invariant 6

Every extracted claim/relation/fact has provenance.

### Invariant 7

Heavy AI models are optional.

### Invariant 8

Model/provider replacement must not require changing the Knowledge
Object schema.

### Invariant 9

Incremental ingestion operates at document/page/asset granularity.

### Invariant 10

The ingestion pipeline must remain usable offline with mock/rule
implementations.

------------------------------------------------------------------------

## Appendix C --- First target Rust API

The minimum new public surface should eventually look approximately
like:

``` rust
pub trait KnowledgeBoundaryDetector: Send + Sync {
    fn name(&self) -> &str;

    fn detect(
        &self,
        ast: &DocumentAst,
    ) -> Result<Vec<KnowledgeFragment>, BoundaryError>;
}

pub trait SemanticAnalyzer: Send + Sync {
    fn name(&self) -> &str;

    fn analyze(
        &self,
        fragments: &[KnowledgeFragment],
    ) -> Result<KnowledgeIr, SemanticAnalysisError>;
}

pub trait RetrievalProjector: Send + Sync {
    fn name(&self) -> &str;

    fn project(
        &self,
        fragments: &[KnowledgeFragment],
    ) -> Vec<RetrievalProjection>;
}
```

Together:

``` text
DocumentAst
    |
    | KnowledgeBoundaryDetector
    v
KnowledgeFragment[]
    |
    | SemanticAnalyzer
    v
KnowledgeIr
    |
    +--> Knowledge Objects
    |
    | RetrievalProjector
    v
RetrievalProjection[]
    |
    | EmbeddingProvider
    v
Indexes
```

This should be the stable architectural contract for the next stage of
AIKOQL.
