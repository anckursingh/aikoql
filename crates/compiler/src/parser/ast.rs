//! AIKOQL AST types per MRFC-0010 §3 (Parser).
//!
//! The AST represents the syntactic structure of a query before semantic
//! analysis. It preserves source spans for diagnostics.

use super::lexer::Span;

#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    Match(MatchStatement),
    Create(CreateStatement),
    Update(UpdateStatement),
    Delete(DeleteStatement),
    Ingest(IngestStatement),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchStatement {
    pub entity: String,
    pub predicates: Vec<Predicate>,
    pub similarity: Option<SimilarityClause>,
    pub traverse: Option<TraverseClause>,
    pub projection: Projection,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IngestStatement {
    pub source: String,
    pub extract_tables: bool,
    pub extract_entities: bool,
    pub build_relationships: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CreateStatement {
    pub entity: String,
    pub properties: Vec<(String, Expr)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpdateStatement {
    pub entity: String,
    pub koid: String,
    pub properties: Vec<(String, Expr)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeleteStatement {
    pub entity: String,
    pub koid: String,
}

// ---------------------------------------------------------------------------
// Clauses
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Predicate {
    Eq { property: String, value: Expr },
    Neq { property: String, value: Expr },
    Gt { property: String, value: Expr },
    Lt { property: String, value: Expr },
    Gte { property: String, value: Expr },
    Lte { property: String, value: Expr },
    And { left: Box<Predicate>, right: Box<Predicate> },
    Or { left: Box<Predicate>, right: Box<Predicate> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct SimilarityClause {
    pub query: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraverseClause {
    pub relation: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Projection {
    Star,
    Explain,
    Fields(Vec<String>),
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
}
