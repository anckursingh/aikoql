# MRFC-0050 — Document OCR, Understanding & Knowledge Ingestion

**Status:** Proposed  
**Target:** Mnemosyne MVP 1.0  
**Scope:** Document ingestion, OCR, layout understanding, semantic
extraction, ontology mapping, entity resolution, provenance, embeddings,
and Knowledge Object creation.

## 1. Purpose

Mnemosyne should treat documents as first-class knowledge sources. OCR
is therefore not a standalone RAG feature: it is an ingestion mechanism
that converts unstructured documents into the same Knowledge Object,
ontology, graph, vector, provenance, security, and AIKOQL model used for
structured sources.

The target flow is:

``` text
Document
  ↓
Document Intake
  ↓
Artifact Store + SHA-256
  ↓
Document Extraction
  ↓
OCR Decision ── no OCR ──┐
  ↓ yes                  │
OCR + page/region data   │
  └──────────┬───────────┘
             ↓
      Canonical Document Model
             ↓
       Layout Understanding
             ↓
       Semantic Extraction
       ├── entities
       ├── relations
       ├── facts
       └── events
             ↓
       Ontology Mapping
             ↓
       Entity Resolution
             ↓
       Knowledge Objects
       ├── graph
       ├── vector
       └── provenance
             ↓
       Knowledge Kernel
             ↓
       AIKOQL / Agents / Studio
```

## 2. MVP Goal

A user should be able to upload/connect a PDF, including a scanned PDF,
and obtain queryable, provenance-preserving Knowledge Objects.

Example:

``` text
contract.pdf
  ↓
Customer
Contract
Company
Amount
ExpiryDate
  ↓
Customer ──SIGNED──> Contract
Company  ──ISSUED──> Contract
```

Every extracted assertion must be traceable to its source document,
version, page, location, extraction method, confidence, and
model/pipeline version.

## 3. MVP Scope

### Required

- PDF, TXT, HTML, DOCX
- native text extraction
- scanned PDF/image OCR
- page-level OCR decision
- page boundaries
- bounding boxes where available
- metadata and checksum
- deduplication
- document versioning
- entity extraction
- relationship extraction
- ontology mapping
- entity resolution
- confidence scoring
- provenance/evidence
- Knowledge Object creation
- graph relationships
- embeddings
- hybrid retrieval
- asynchronous processing
- retries and checkpoints
- tenant isolation
- encryption integration
- MCP/REST access
- Studio document/evidence visibility

### Not MVP-blocking

- handwriting perfection
- video/audio transcription
- custom OCR model training
- GPU orchestration
- advanced multimodal model training
- every document format
- real-time OCR
- distributed document processing
- perfect semantic extraction

## 4. Architectural Principles

### 4.1 Preserve the original

The original artifact is immutable.

``` text
Document Artifact
 ├── immutable content
 ├── SHA-256
 ├── metadata
 └── source reference
```

Derived text/OCR/layout/semantic results may be regenerated.

### 4.2 Extraction is probabilistic

OCR and semantic extraction are not authoritative truth. Every result
carries:

``` text
value
confidence
evidence
method
provider/model
version
timestamp
```

### 4.3 Deterministic stages before probabilistic stages

Prefer:

``` text
PDF parser → native text → quality test → OCR only if needed → semantic extraction
```

A mixed PDF should OCR only pages that need it.

### 4.4 Idempotency

Processing identity:

``` text
document_hash
+ pipeline_version
+ extractor_version
+ ontology_version
```

Same input and same versions must not create duplicate results.

### 4.5 Human validation

Ontology/semantic results follow:

``` text
DISCOVERED → PROPOSED → VALIDATED → PUBLISHED
                    └→ REJECTED
```

### 4.6 Provenance is first-class

``` text
KO
 └── Evidence
      ├── document
      ├── document version
      ├── page
      ├── bounding box
      ├── text span
      ├── extractor/model
      └── confidence
```

## 5. HLD

``` text
                    +----------------------+
                    | Upload / Connectors  |
                    +----------+-----------+
                               |
                               v
                    +----------------------+
                    | Document Intake      |
                    +----------+-----------+
                               |
                    +----------+----------+
                    |                     |
                    v                     v
              Artifact Store       Metadata / KO
                    |
                    v
             Processing Queue
                    |
                    v
             +-------------+
             | Orchestrator|
             +------+------+
                    |
       +------------+-------------+
       |            |             |
       v            v             v
   Extractor     OCR Engine   Layout Engine
       |            |             |
       +------------+-------------+
                    |
                    v
             Document Model
                    |
                    v
          Semantic Extraction
           /       |        \
          v        v         v
      Entities  Relations   Facts
           \       |        /
            +------+-------+
                   |
                   v
            Ontology Mapper
                   |
                   v
            Entity Resolver
                   |
                   v
           Knowledge Objects
          /        |        \
         v         v         v
      Graph      Vector   Provenance
          \        |        /
           +-------+-------+
                   |
                   v
             Knowledge Kernel
             /            \
          AIKOQL         Agents
```

## 6. HLD Components

### Document Intake

Responsibilities:

- accept upload/connector stream
- MIME/type detection
- checksum
- size/format validation
- create Document KO
- create processing job

It must not perform OCR or semantic extraction.

### Artifact Store

Stores immutable binary artifacts.

MVP backends:

``` text
local filesystem / Docker volume
```

Future:

``` text
S3 / Azure Blob / GCS / MinIO
```

The Knowledge Kernel should store references and metadata rather than
duplicate large binaries unnecessarily.

### Document Extractor

Provider-independent interface for PDF/DOCX/HTML/TXT.

``` rust
pub trait DocumentExtractor: Send + Sync {
    fn supports(&self, mime_type: &str) -> bool;
    async fn inspect(&self, input: &DocumentInput) -> Result<DocumentInspection>;
    async fn extract(&self, input: &DocumentInput) -> Result<DocumentModel>;
}
```

### OCR Decision Engine

Decision is page-level:

``` text
native text available?
       ↓
text quality acceptable?
  yes /       \ no
     ↓         ↓
 continue      OCR
```

Signals can include printable-character ratio, text density,
garbage-character ratio, line quality, font metadata, and image-only
status.

### OCR Provider

Do not couple Mnemosyne to one vendor.

``` rust
pub trait OcrProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn recognize(
        &self,
        image: &PageImage,
        options: OcrOptions
    ) -> Result<OcrPage>;
}
```

Possible providers:

``` text
Local OCR
Cloud OCR
Enterprise OCR
Future specialized providers
```

### Layout Engine

OCR text alone is insufficient. Preserve document structure:

``` text
Page
 ├── Heading
 ├── Paragraph
 ├── Table
 │    ├── Row
 │    └── Cell
 ├── Image
 └── Footer
```

### Semantic Extraction

Produces candidate:

``` text
entities
relationships
facts
events
attributes
```

### Ontology Mapping

Maps extracted concepts to the ontology discovered from existing
providers and documents.

``` text
candidate
  ↓
exact match
  ↓
synonym match
  ↓
semantic match
  ↓
schema mapping
  ↓
unresolved
```

### Entity Resolution

Links document entities to existing KOs.

Example:

``` text
PDF: "Ankur Kumar"
Postgres: customer_id=C123, name="Ankur Kumar"
Neo4j: Customer{id=C123}
             ↓
        Customer C123
```

Never silently merge low-confidence candidates.

### Knowledge Commit

Creates/updates:

``` text
Document KO
Domain Entity KOs
Relationships
Evidence/provenance
Embeddings
```

## 7. Canonical Document Model

``` rust
pub struct DocumentModel {
    pub document_id: String,
    pub version: u64,
    pub metadata: DocumentMetadata,
    pub pages: Vec<PageModel>,
}

pub struct PageModel {
    pub page_number: u32,
    pub width: u32,
    pub height: u32,
    pub blocks: Vec<BlockModel>,
}

pub struct BlockModel {
    pub block_type: BlockType,
    pub text: String,
    pub bbox: Option<BoundingBox>,
    pub confidence: Option<f32>,
    pub children: Vec<BlockModel>,
}
```

Block types:

``` text
Title, Heading, Paragraph, List, Table, TableCell,
Image, Caption, Header, Footer, Footnote, Code, Unknown
```

## 8. OCR Model

``` rust
pub struct OcrPage {
    pub page_number: u32,
    pub width: u32,
    pub height: u32,
    pub blocks: Vec<OcrBlock>,
    pub language: Option<String>,
    pub confidence: f32,
}

pub struct OcrBlock {
    pub id: String,
    pub text: String,
    pub confidence: f32,
    pub bbox: BoundingBox,
    pub lines: Vec<OcrLine>,
}

pub struct OcrLine {
    pub text: String,
    pub confidence: f32,
    pub bbox: BoundingBox,
    pub words: Vec<OcrWord>,
}

pub struct OcrWord {
    pub text: String,
    pub confidence: f32,
    pub bbox: BoundingBox,
}
```

``` rust
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
```

Coordinates should be normalized where possible.

## 9. Table Representation

Do not flatten tables into plain text.

``` text
Table
 ├── columns
 └── rows
      └── cells
           ├── row
           ├── column
           ├── span
           ├── text
           └── bbox
```

This is essential because tables are one of the areas where ordinary RAG
loses structure.

## 10. Semantic Extraction

Example:

``` text
"Acme Ltd purchased 10 laptops from Dell on 4 August 2026."
```

Candidate result:

``` text
Acme Ltd
 └── PURCHASED
      └── Laptop
           ├── quantity: 10
           ├── supplier: Dell
           └── date: 2026-08-04
```

All candidates remain provenance-linked.

## 11. Ontology Integration

The document pipeline must reuse the same ontology system used by
PostgreSQL, Neo4j, MongoDB and other providers.

``` text
Postgres schema ─┐
Neo4j schema ────┤
Mongo schema ────┤
Documents ───────┘
        ↓
Ontology Discovery
        ↓
Canonical Ontology
        ↓
Document Entity Mapping
```

This is strategically important: documents become another source of
knowledge rather than a separate RAG subsystem.

## 12. Entity Resolution

Candidate signals:

``` text
name
email
phone
address
external IDs
ontology type
context
graph relationships
semantic similarity
```

Suggested configurable policy:

``` text
>= 0.95  automatic candidate link
0.75-0.95 proposed link
< 0.75 unresolved
```

These are initial engineering defaults, not universal truth.

False merges should be treated as more dangerous than unresolved
entities.

## 13. Provenance

Example:

``` json
{
  "source": {
    "document_id": "doc-123",
    "document_version": 2,
    "page": 7,
    "bbox": [120, 330, 480, 370]
  },
  "extractor": {
    "provider": "ocr-provider",
    "version": "1.2"
  },
  "confidence": 0.97
}
```

This must integrate with existing:

``` text
trace
explain
prove
audit
```

## 14. Embeddings and Chunking

Do not embed only raw OCR text.

Potential representations:

``` text
Document
Page
Section
Chunk
Entity
```

Chunk by structure rather than fixed character count:

``` text
Document → Section → Paragraph/Table → Chunk
```

Each chunk retains:

``` text
document_id
page_range
section
text
entities
relationships
evidence references
```

Tables must retain their structure.

## 15. Pipeline State Machine

``` text
RECEIVED
  ↓
VALIDATING
  ├── FAILED
  ↓
EXTRACTING
  ↓
OCR_REQUIRED?
  ├── no ─────────────┐
  └── yes → OCR       │
                      ↓
               LAYOUT_ANALYSIS
                      ↓
              SEMANTIC_EXTRACTION
                      ↓
                ONTOLOGY_MAPPING
                      ↓
                ENTITY_RESOLUTION
                      ↓
                 KO_COMMITTING
                      ↓
                    INDEXING
                      ↓
                  COMPLETED
```

Retryable failures:

``` text
OCR timeout
provider unavailable
temporary storage failure
AI provider timeout
```

Non-retryable failures:

``` text
corrupt document
unsupported format
invalid authentication
size limit
```

## 16. Checkpointing and Recovery

``` rust
pub struct PipelineCheckpoint {
    pub document_id: String,
    pub document_version: u64,
    pub stage: PipelineStage,
    pub pipeline_version: String,
    pub extractor_version: String,
    pub ontology_version: String,
    pub attempt: u32,
    pub updated_at: HlcTimestamp,
}
```

Stages:

``` text
Received
Validated
Extracted
OcrCompleted
LayoutCompleted
SemanticCompleted
OntologyMapped
EntitiesResolved
KnowledgeCommitted
Indexed
Completed
Failed
```

A crash during semantic extraction must resume from that stage, not
repeat completed OCR.

## 17. Scheduler Integration

Use the existing Scheduler Engine.

Jobs:

``` text
DocumentIngestionJob
OCRJob
LayoutAnalysisJob
SemanticExtractionJob
OntologyMappingJob
EntityResolutionJob
EmbeddingJob
IndexMaintenanceJob
```

Jobs must be idempotent and checkpointed.

## 18. Security and MRFC-0020

Every processing job carries:

``` text
tenant_id
subject
document_id
security_context
```

Required:

- encryption at rest
- tenant isolation
- ACL enforcement
- secure temporary files
- no plaintext sensitive logging
- provider credential isolation
- configurable retention
- audit events
- field-level encryption for sensitive extracted values

Examples:

``` text
salary
tax_id
passport_number
bank_account
```

## 19. LLD Crate Structure

This should fit the current project structure:

``` text
crates/
└── ingestion/
    ├── src/
    │   ├── lib.rs
    │   ├── intake/
    │   │   ├── mod.rs
    │   │   ├── service.rs
    │   │   ├── validator.rs
    │   │   ├── dedup.rs
    │   │   └── metadata.rs
    │   ├── artifact/
    │   │   ├── mod.rs
    │   │   ├── store.rs
    │   │   ├── filesystem.rs
    │   │   └── object_store.rs
    │   ├── extract/
    │   │   ├── mod.rs
    │   │   ├── trait.rs
    │   │   ├── pdf.rs
    │   │   ├── docx.rs
    │   │   ├── html.rs
    │   │   └── text.rs
    │   ├── ocr/
    │   │   ├── mod.rs
    │   │   ├── trait.rs
    │   │   ├── decision.rs
    │   │   ├── provider.rs
    │   │   └── normalization.rs
    │   ├── layout/
    │   │   ├── mod.rs
    │   │   ├── analyzer.rs
    │   │   ├── blocks.rs
    │   │   └── tables.rs
    │   ├── semantic/
    │   │   ├── mod.rs
    │   │   ├── entity.rs
    │   │   ├── relation.rs
    │   │   ├── fact.rs
    │   │   └── classifier.rs
    │   ├── ontology/
    │   │   ├── mod.rs
    │   │   ├── resolver.rs
    │   │   ├── mapper.rs
    │   │   └── confidence.rs
    │   ├── resolution/
    │   │   ├── mod.rs
    │   │   ├── entity.rs
    │   │   ├── candidate.rs
    │   │   └── merge.rs
    │   ├── embedding/
    │   │   ├── mod.rs
    │   │   ├── chunker.rs
    │   │   ├── provider.rs
    │   │   └── indexer.rs
    │   ├── provenance/
    │   │   ├── mod.rs
    │   │   ├── evidence.rs
    │   │   └── source_map.rs
    │   ├── pipeline/
    │   │   ├── mod.rs
    │   │   ├── orchestrator.rs
    │   │   ├── state.rs
    │   │   ├── checkpoint.rs
    │   │   └── retry.rs
    │   ├── jobs/
    │   │   ├── mod.rs
    │   │   ├── ingestion.rs
    │   │   ├── ocr.rs
    │   │   ├── semantic.rs
    │   │   ├── ontology.rs
    │   │   ├── resolution.rs
    │   │   └── embedding.rs
    │   └── models/
    │       ├── mod.rs
    │       ├── document.rs
    │       ├── page.rs
    │       ├── block.rs
    │       ├── ocr.rs
    │       ├── extraction.rs
    │       └── evidence.rs
    └── tests/
        ├── intake.rs
        ├── extraction.rs
        ├── ocr.rs
        ├── layout.rs
        ├── semantic.rs
        ├── ontology.rs
        ├── resolution.rs
        ├── provenance.rs
        └── pipeline.rs
```

## 20. Core Interfaces

### Document source

``` rust
pub trait DocumentSource: Send + Sync {
    async fn open(
        &self,
        source: DocumentSourceRef
    ) -> Result<DocumentInput>;
}
```

### Artifact store

``` rust
pub trait ArtifactStore: Send + Sync {
    async fn put(
        &self,
        artifact: DocumentArtifact
    ) -> Result<ArtifactRef>;

    async fn get(
        &self,
        reference: &ArtifactRef
    ) -> Result<DocumentStream>;

    async fn exists(
        &self,
        hash: &str
    ) -> Result<bool>;
}
```

### Extractor

``` rust
pub trait DocumentExtractor: Send + Sync {
    fn supports(&self, mime: &str) -> bool;

    async fn extract(
        &self,
        input: &DocumentInput
    ) -> Result<DocumentModel>;
}
```

### OCR

``` rust
pub trait OcrProvider: Send + Sync {
    fn provider_id(&self) -> &str;

    async fn recognize(
        &self,
        page: &PageImage,
        options: OcrOptions
    ) -> Result<OcrPage>;
}
```

### Semantic provider

``` rust
pub trait SemanticProvider: Send + Sync {
    async fn extract_entities(
        &self,
        document: &DocumentModel
    ) -> Result<Vec<EntityCandidate>>;

    async fn extract_relations(
        &self,
        document: &DocumentModel,
        entities: &[EntityCandidate]
    ) -> Result<Vec<RelationCandidate>>;
}
```

### Ontology resolver

``` rust
pub trait OntologyResolver: Send + Sync {
    async fn resolve_entity(
        &self,
        candidate: &EntityCandidate
    ) -> Result<OntologyMatch>;

    async fn resolve_relation(
        &self,
        candidate: &RelationCandidate
    ) -> Result<OntologyRelationMatch>;
}
```

### Entity resolver

``` rust
pub trait EntityResolver: Send + Sync {
    async fn find_candidates(
        &self,
        entity: &ResolvedEntity
    ) -> Result<Vec<EntityCandidateMatch>>;

    async fn resolve(
        &self,
        entity: &ResolvedEntity,
        candidates: Vec<EntityCandidateMatch>
    ) -> Result<EntityResolution>;
}
```

## 21. Pipeline Orchestrator

``` rust
pub struct DocumentPipeline {
    intake: Arc<DocumentIntake>,
    extractor: Arc<ExtractorRegistry>,
    ocr: Arc<OcrRegistry>,
    layout: Arc<LayoutAnalyzer>,
    semantic: Arc<SemanticEngine>,
    ontology: Arc<OntologyResolver>,
    resolver: Arc<EntityResolver>,
    embeddings: Arc<EmbeddingService>,
    kernel: Arc<KnowledgeKernel>,
}
```

Conceptual execution:

``` rust
pub async fn process(
    &self,
    job: DocumentJob
) -> Result<DocumentProcessingResult> {
    let document = self.intake.accept(job).await?;
    let extracted = self.extract(document).await?;
    let normalized = self.normalize(extracted).await?;
    let semantic = self.semantic.extract(&normalized).await?;
    let mapped = self.ontology.map(semantic).await?;
    let resolved = self.resolver.resolve(mapped).await?;
    let kos = self.commit_kos(resolved).await?;
    self.embeddings.index(kos).await?;
    Ok(result)
}
```

Production implementation must checkpoint after each stage.

## 22. API

``` http
POST /api/v1/documents
GET  /api/v1/documents/{document_id}/status
GET  /api/v1/documents/{document_id}/knowledge
GET  /api/v1/documents/{document_id}/evidence
POST /api/v1/documents/{document_id}/reprocess
```

Example upload response:

``` json
{
  "document_id": "doc_123",
  "version": 1,
  "status": "RECEIVED"
}
```

Status:

``` json
{
  "document_id": "doc_123",
  "stage": "SEMANTIC_EXTRACTION",
  "progress": 0.72,
  "status": "PROCESSING"
}
```

Reprocess may specify a stage:

``` json
{
  "stage": "ONTOLOGY_MAPPING"
}
```

## 23. MCP

Recommended tools:

``` text
document_ingest
document_status
document_get
document_reprocess
document_evidence
document_entities
document_relationships
document_ontology
document_delete
```

Agent flow:

``` text
document_ingest
 → document_status
 → document_ontology
 → aikoql
 → explain
 → prove
```

## 24. AIKOQL

Document-derived knowledge must be queryable through the same ontology.

``` aikoql
MATCH Document
WHERE mime_type = "application/pdf"
RETURN filename, page_count
```

``` aikoql
MATCH Contract
WHERE expiry_date < "2027-01-01"
RETURN customer, expiry_date
```

``` aikoql
TRAVERSE Customer OWNS Contract DEPTH 2
```

The planner should not expose the physical source unless requested
through provenance/explain.

## 25. Studio

Add:

``` text
Documents
 ├── Documents
 ├── Processing Queue
 ├── OCR
 ├── Extracted Entities
 ├── Relationships
 └── Evidence
```

Document detail should show:

``` text
contract.pdf
Status: Completed
Pages: 12
OCR pages: 3, 7
Entities: 43
Relationships: 27
KOs created: 18
```

Evidence viewer:

``` text
Document
 ↓
Page
 ↓
Bounding box
 ↓
Extracted text
 ↓
Ontology concept
 ↓
Knowledge Object
```

## 26. Acceptance Criteria

### AC-01 Intake

Upload a supported PDF:

- Document KO exists
- checksum exists
- metadata exists
- processing job exists

### AC-02 Deduplication

Same bytes + same pipeline versions do not create duplicate processing
results.

### AC-03 Native text

A good text PDF does not invoke OCR unnecessarily.

### AC-04 OCR fallback

Scanned pages invoke OCR and produce text, confidence, page number and
bounding boxes where supported.

### AC-05 Mixed PDF

Only pages requiring OCR are processed by OCR.

### AC-06 Layout

Tables preserve rows, columns, cells, page and coordinates.

### AC-07 Semantic extraction

Entities/relationships contain confidence and evidence.

### AC-08 Ontology

Mappings use the existing ontology and uncertain mappings remain
proposed.

### AC-09 Entity resolution

Existing entities produce candidate links with confidence.

### AC-10 Provenance

Every published extracted fact can be traced to
document/page/location/method/confidence.

### AC-11 Idempotency

Reprocessing the same document/pipeline version does not duplicate KOs.

### AC-12 Recovery

A crash resumes from the last completed stage.

### AC-13 Security

Cross-tenant artifact and knowledge access is impossible.

### AC-14 Encryption

Configured sensitive fields use MRFC-0020 encryption.

### AC-15 AIKOQL

Document-derived KOs are queryable through the ontology.

### AC-16 Agent

MCP/Python can ingest, inspect status, query knowledge and retrieve
evidence.

## 27. Performance and Observability

Measure independently:

``` text
native extraction
OCR
layout
semantic extraction
ontology mapping
entity resolution
embedding
KO commit
end-to-end
```

Metrics:

``` text
documents/sec
pages/sec
OCR pages/sec
entities/sec
relationships/sec
P50/P95/P99 latency
CPU
memory
storage
provider cost
```

Expose:

``` text
mnemosyne_documents_ingested_total
mnemosyne_documents_failed_total
mnemosyne_ocr_pages_total
mnemosyne_ocr_failures_total
mnemosyne_extraction_duration_seconds
mnemosyne_ontology_mapping_duration_seconds
mnemosyne_entity_resolution_duration_seconds
mnemosyne_document_pipeline_duration_seconds
```

## 28. Accuracy

OCR:

``` text
CER — Character Error Rate
WER — Word Error Rate
```

Extraction:

``` text
Precision
Recall
F1
```

Ontology:

``` text
mapping accuracy
precision
recall
abstention rate
```

Entity resolution:

``` text
precision
recall
false merge rate
false split rate
```

False merges should be treated as more dangerous than unresolved
entities.

## 29. Testing

### Unit

- extractor
- OCR decision
- OCR normalization
- layout
- tables
- semantic extraction
- ontology mapping
- entity resolution
- chunking
- provenance
- checkpoint/retry

### Golden corpus

Include:

``` text
text PDF
scanned PDF
mixed PDF
invoice
contract
resume
bank statement
table-heavy document
multi-language document
```

### Failure

Test:

``` text
corrupt PDF
OCR timeout
provider timeout
storage failure
process crash
duplicate upload
partial completion
```

### Security

Test:

``` text
tenant isolation
artifact authorization
temporary-file protection
secret leakage
log redaction
```

## 30. Implementation Phases

### D1 — Foundation

``` text
Document KO
ArtifactStore
DocumentSource
DocumentExtractor
DocumentModel
```

### D2 — OCR

``` text
OCR provider trait
OCR decision engine
OCR normalization
page/region provenance
```

### D3 — Layout

``` text
blocks
sections
tables
coordinates
```

### D4 — Semantic extraction

``` text
entities
relations
facts
confidence
```

### D5 — Ontology

``` text
mapping
confidence
proposals
validation
```

### D6 — Entity resolution

``` text
candidate search
matching
safe linking
```

### D7 — Knowledge commit

``` text
KOs
relationships
provenance
audit
```

### D8 — Vector

``` text
chunking
embedding
indexing
hybrid retrieval
```

### D9 — Agent/Studio

``` text
MCP
REST
Document Explorer
Evidence viewer
```

## 31. Critical Architectural Decisions

### OCR does not belong in the Kernel

Correct:

``` text
ingestion → canonical document model → Knowledge Kernel
```

Incorrect:

``` text
Kernel → PDF parser/OCR/LLM
```

The Kernel receives validated knowledge and provenance.

### OCR is a provider

``` text
OcrProvider
 ├── Local
 ├── Cloud
 └── Enterprise
```

This enables offline deployments, provider selection, failover, cost
control and benchmarking.

### Raw and derived data are separate

``` text
RAW
 └── original document

DERIVED
 ├── extracted text
 ├── OCR
 ├── layout
 ├── entities
 ├── relationships
 ├── embeddings
 └── ontology mappings
```

Derived data can be regenerated; raw data remains immutable.

### Pipeline versions are explicit

Every derived result should retain:

``` text
pipeline_version
extractor_version
ocr_provider
ocr_model_version
semantic_provider
semantic_model_version
ontology_version
embedding_model
```

This makes reprocessing and scientific benchmarking reproducible.

## 32. End-to-End Example

Input:

``` text
customer-contract.pdf
```

1.  Calculate SHA-256.
2.  Create Document KO.
3.  Inspect 12 pages.
4.  Detect 9 native-text pages and 3 scanned pages.
5.  OCR only pages 3, 7 and 11.
6.  Build canonical DocumentModel.
7.  Extract Customer, Company, Contract, Date and Amount.
8.  Extract relationships such as Customer-SIGNED-Contract.
9.  Map concepts against the canonical ontology.
10. Resolve Customer against existing PostgreSQL/Neo4j/Mongo KOs.
11. Commit Document and domain KOs with evidence.
12. Create embeddings and indexes.
13. Query through AIKOQL.
14. Let an agent retrieve the answer plus evidence.

Example agent question:

> Which contracts for customer C123 expire this year?

Expected result includes:

``` text
Contract
expiry date
customer
source document
page
bounding box
confidence
```

## 33. Strategic Role in Mnemosyne

The final architecture becomes:

``` text
                    AI AGENTS
                        |
                 Agent SDK / MCP
                        |
                     AIKOQL
                        |
              Knowledge / Query Layer
                        |
        +---------------+---------------+
        |               |               |
   PostgreSQL         Neo4j          MongoDB
        |               |               |
        +---------------+---------------+
                        |
                  Documents
                        |
                        v
              Knowledge Discovery
                        |
        +---------------+---------------+
        |               |               |
     Ontology        Entity         Semantic
    Discovery       Resolution      Extraction
        |               |               |
        +---------------+---------------+
                        |
                Knowledge Objects
                        |
              +---------+---------+
              |         |         |
            Graph     Vector   Provenance
              |         |         |
              +---------+---------+
                        |
                 Knowledge Kernel
                        |
                  Storage Engines
```

The strategic point is:

> **OCR is not the product. Document understanding is not a separate RAG
> system. They are mechanisms for converting unstructured information
> into the same Knowledge Object universe as structured data.**

That makes document ingestion a critical part of Mnemosyne’s
semantic-federation story.

## 34. MVP Definition of Done

``` text
[ ] PDF upload
[ ] Document KO
[ ] SHA-256 deduplication
[ ] Native PDF extraction
[ ] Scanned PDF OCR
[ ] Mixed PDF page-level OCR
[ ] Page/region provenance
[ ] Layout blocks
[ ] Structured tables
[ ] Entity extraction
[ ] Relationship extraction
[ ] Ontology mapping
[ ] Entity resolution
[ ] Confidence
[ ] Provenance query
[ ] KO creation
[ ] Graph indexing
[ ] Vector indexing
[ ] AIKOQL query
[ ] MCP/Python agent access
[ ] Studio document/evidence view
[ ] Crash recovery
[ ] Idempotent processing
[ ] Tenant isolation
[ ] Encryption
[ ] Metrics
[ ] Golden corpus
```

## 35. Immediate Recommendation

Do not build a huge document-AI platform first.

Build the smallest complete vertical slice:

``` text
Scanned PDF
   ↓
OCR
   ↓
Canonical DocumentModel
   ↓
Entity + relationship extraction
   ↓
Ontology mapping
   ↓
Entity resolution
   ↓
Knowledge Objects
   ↓
Graph + Vector + Provenance
   ↓
AIKOQL
   ↓
Agent
   ↓
Answer + Evidence
```

Then demonstrate it together with:

``` text
PostgreSQL Customer
      +
Neo4j Account
      +
MongoDB SupportCase
      +
Scanned PDF Contract
      ↓
Ontology Auto Discovery
      ↓
Unified Knowledge Model
      ↓
AIKOQL
      ↓
Agent
      ↓
Explainable answer + evidence
```

This is the document capability that should support the **Mnemosyne MVP
1.0** rather than becoming another independent feature set.
