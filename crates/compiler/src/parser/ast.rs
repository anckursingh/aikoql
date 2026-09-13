//! Aikoql AST types per MRFC-0010 §3 (Parser).
//!
//! The AST represents the syntactic structure of a query before semantic
//! analysis. It preserves source spans for diagnostics.

use serde::{Deserialize, Serialize};

use super::lexer::Span;

/// Stable-AST contract (ND-02): the version stamped on every parsed
/// statement. Bump on any breaking AST shape change.
pub const AST_VERSION: u32 = 1;

/// A parsed statement with its AST version stamp — the serializable form of
/// the AST (kq011). `parse` still returns the bare `Statement` for the
/// compiler-internal callers; `parse_versioned` is the stable contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VersionedStatement {
    pub version: u32,
    pub statement: Statement,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpanNode<T> {
    pub node: T,
    pub span: Span,
}

impl<T> SpanNode<T> {
    pub fn new(node: T, span: Span) -> Self {
        SpanNode { node, span }
    }
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

// Transient parse products, not hot-path data — same layout tradeoff as the
// ingestion AST (ast.rs / fragment.rs allows).
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Statement {
    Match(MatchStatement),
    Create(CreateStatement),
    Update(UpdateStatement),
    Delete(DeleteStatement),
    Ingest(IngestStatement),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MatchStatement {
    pub entity: String,
    pub predicates: Vec<Predicate>,
    pub similarity: Option<SimilarityClause>,
    pub traverse: Option<TraverseClause>,
    /// v0.3 K2: temporal clause (AS_OF / BETWEEN / HISTORICAL).
    pub temporal: Option<TemporalClause>,
    /// v0.3 K1 leftover: epistemic filter clause (EPISTEMIC <status>, ...).
    pub epistemic: Option<EpistemicClause>,
    /// QL-006: provenance filter (SOURCE "artifact") — keep rows whose
    /// evidence trail contains the given source artifact (exact match).
    pub provenance: Option<String>,
    /// EXE-006: pagination over the final deterministic row order.
    /// OFFSET is only valid together with LIMIT (parser-enforced).
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    /// P5-M2 (ND-02): ORDER BY <field> [ASC|DESC], ... — ordering lands in
    /// the pipeline before LIMIT; the Sort op executes in P5-M5.
    pub order_by: Option<OrderByClause>,
    /// P5-M2 (ND-02): GROUP BY <field> | <agg>(<field>|*), ... — the
    /// Aggregate op executes in P5-M5.
    pub group_by: Option<GroupByClause>,
    /// P5-M2 (ND-02): JOIN <type> ON <left> == <right> — the Join op
    /// executes in P5-M6.
    pub join: Option<JoinClause>,
    pub projection: Projection,
}

/// P5-M2 (ND-02): `ORDER BY` — a comma-separated key list; each key carries
/// its own direction (`ASC` is the default, `DESC` binds to the key it
/// follows — kq002).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrderByClause {
    pub keys: Vec<OrderKey>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrderKey {
    pub field: String,
    pub desc: bool,
}

/// P5-M2 (ND-02): `GROUP BY` — a comma-separated list mixing grouping keys
/// (bare fields) and aggregate calls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupByClause {
    pub keys: Vec<String>,
    pub aggs: Vec<AggCall>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AggFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AggCall {
    pub func: AggFunc,
    /// `None` for `COUNT(*)`.
    pub field: Option<String>,
}

/// P5-M2 (ND-02): `JOIN <right_type> ON <left> == <right>` — always INNER in
/// the v1 grammar; LEFT JOIN arrives with the P5-M6 join engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JoinClause {
    pub right_type: String,
    pub on: JoinOn,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JoinOn {
    pub left: String,
    pub right: String,
}

/// v0.3 K2: temporal query clause. `AsOf`/`Historical` are transaction time
/// (millis since epoch); `Between` is a valid-time interval [from, to).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TemporalClause {
    AsOf(u64),
    Between { from: u64, to: u64 },
    Historical,
}

/// v0.3 K1 leftover: protocol-level epistemic filter — keep rows whose
/// epistemic status is one of `allowed` (status names, e.g. "verified").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EpistemicClause {
    pub allowed: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IngestStatement {
    pub source: String,
    pub extract_tables: bool,
    pub extract_entities: bool,
    pub build_relationships: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateStatement {
    pub entity: String,
    pub properties: Vec<(String, Expr)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateStatement {
    pub entity: String,
    pub koid: String,
    pub properties: Vec<(String, Expr)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeleteStatement {
    pub entity: String,
    pub koid: String,
}

// ---------------------------------------------------------------------------
// Clauses
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Predicate {
    Eq {
        property: String,
        value: Expr,
    },
    Neq {
        property: String,
        value: Expr,
    },
    Gt {
        property: String,
        value: Expr,
    },
    Lt {
        property: String,
        value: Expr,
    },
    Gte {
        property: String,
        value: Expr,
    },
    Lte {
        property: String,
        value: Expr,
    },
    And {
        left: Box<Predicate>,
        right: Box<Predicate>,
    },
    Or {
        left: Box<Predicate>,
        right: Box<Predicate>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimilarityClause {
    pub query: String,
    /// Optional scoring method: BM25 for Tantivy-backed keyword retrieval.
    pub score: Option<ScoringMethod>,
    /// Optional retrieval method: Embedding for vector ANN search.
    pub using: Option<UsingMethod>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScoringMethod {
    Bm25,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum UsingMethod {
    Embedding,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraverseClause {
    pub relation: String,
    /// P3-M4 §62: optional DEPTH (default 1 at lowering). `None` = absent.
    pub depth: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Projection {
    Star,
    Explain,
    Fields(Vec<String>),
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
}
