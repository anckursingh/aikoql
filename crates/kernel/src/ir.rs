//! Knowledge IR — the intermediate representation between query frontends
//! and the runtime interpreter (MRFC-0005 §Compiler Layer).
//!
//! Every frontend (MCP, SQL, GraphQL, aikoql) compiles to this operator DAG.
//! The runtime interpreter executes it against the Knowledge Kernel.
//!
//! Design: linear pipeline for v1 (no joins, no subqueries). Operators are
//! executed in order; each produces a result set consumed by the next.
//! Full DAG with branching/merging lands when joins or subqueries arrive.

use crate::knowledge::kom::{KError, KResult, Value};

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PredOp {
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Predicate {
    pub property: String,
    pub op: PredOp,
    pub value: Value,
}

impl Predicate {
    pub fn eq(property: impl Into<String>, value: Value) -> Self {
        Predicate {
            property: property.into(),
            op: PredOp::Eq,
            value,
        }
    }
    pub fn neq(property: impl Into<String>, value: Value) -> Self {
        Predicate {
            property: property.into(),
            op: PredOp::Neq,
            value,
        }
    }
    pub fn gt(property: impl Into<String>, value: Value) -> Self {
        Predicate {
            property: property.into(),
            op: PredOp::Gt,
            value,
        }
    }
    pub fn lt(property: impl Into<String>, value: Value) -> Self {
        Predicate {
            property: property.into(),
            op: PredOp::Lt,
            value,
        }
    }
    pub fn gte(property: impl Into<String>, value: Value) -> Self {
        Predicate {
            property: property.into(),
            op: PredOp::Gte,
            value,
        }
    }
    pub fn lte(property: impl Into<String>, value: Value) -> Self {
        Predicate {
            property: property.into(),
            op: PredOp::Lte,
            value,
        }
    }
}

// ---------------------------------------------------------------------------
// Fusion mode
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FuseMode {
    Rrf { k0: usize },
    Weighted { wv: f32, wt: f32 },
    VectorOnly,
    TextOnly,
}

// ---------------------------------------------------------------------------
// Temporal operators (v0.3 K2)
// ---------------------------------------------------------------------------

/// Temporal query operator. `AsOf`/`Historical` are transaction time —
/// MVCC reconstruction of the versions the kernel had committed. `Between`
/// is valid time — rows whose [valid_from, valid_to) interval overlaps the
/// half-open [from, to) window (timeless facts overlap any window).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TemporalOp {
    AsOf(u64),
    Between { from: u64, to: u64 },
    Historical,
}

// ---------------------------------------------------------------------------
// P5-M2 (ND-02) types: ordering, aggregation, join
// ---------------------------------------------------------------------------

/// One ORDER BY key. `desc` = DESC direction (ASC is the default).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SortKey {
    pub field: String,
    pub desc: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AggFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

/// Join kind (P5-M6, ND-06): INNER drops unmatched left rows, LEFT keeps
/// them with a None right side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum JoinKind {
    Inner,
    Left,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AggCall {
    pub func: AggFunc,
    /// `None` for `COUNT(*)`.
    pub field: Option<String>,
}

// ---------------------------------------------------------------------------
// IR operators
// ---------------------------------------------------------------------------

/// One node in the Knowledge IR operator DAG.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum IrOp {
    /// Scan all readable KOs of `type_name` as `subject`.
    /// R9: `roles`/`tenant` are the planner's authorization hints — the
    /// runtime builds a full `Subject` from them so ACL evaluation sees the
    /// caller's roles and tenant scope (previously only the bare name arrived).
    Scan {
        type_name: String,
        subject: String,
        roles: Vec<String>,
        tenant: Option<String>,
    },
    /// Filter the current result set by property predicates.
    Filter { predicates: Vec<Predicate> },
    /// Traverse graph edges from the current KOID set.
    Traverse {
        start_koid: String,
        rel_type: Option<String>,
        depth: usize,
    },
    /// ANN vector similarity search over the current result set.
    AnnSearch {
        vector: Vec<f32>,
        /// Text to embed at query time (when vector is empty). Requires an
        /// embedding provider wired into the kernel; degrades to text search
        /// when no provider is available.
        query_text: Option<String>,
        embedding_model: Option<String>,
        k: usize,
    },
    /// Full-text search over the current result set.
    TextSearch {
        query: String,
        k: usize,
        /// Scoring method: absent = Jaccard (default), "bm25" = Tantivy BM25
        /// via the IndexCoordinator when a text-index maintainer is attached;
        /// falls back to Jaccard otherwise.
        scoring: Option<String>,
    },
    /// Fuse two ranked result sets into one (RRF or weighted).
    Fuse { mode: FuseMode },
    /// v0.3 K2: temporal query operator (AS_OF / BETWEEN / HISTORICAL).
    Temporal { op: TemporalOp },
    /// v0.3 K1 leftover: protocol-level epistemic filter — keep rows whose
    /// epistemic status (an extension-backed field) is in `allowed`.
    EpistemicFilter { allowed: Vec<String> },
    /// QL-006: provenance filter — keep rows whose evidence trail contains
    /// an entry with this source artifact (exact match).
    ProvenanceFilter { source: String },
    /// EXE-006: pagination over the final deterministic row order — skip
    /// `offset` rows, then keep at most `limit`.
    Limit { limit: usize, offset: usize },
    /// Project specific fields from the result set.
    Project { fields: Vec<String> },
    /// P5-M2 (ND-02): ORDER BY — deterministic sort over the final row
    /// order. Lands after Project, before Limit. Executes in P5-M5.
    Sort { keys: Vec<SortKey> },
    /// P5-M2 (ND-02): GROUP BY + aggregates — lands right after Filter
    /// (filter-then-aggregate: grouping never sees rows the WHERE clause
    /// removed, the authorization-safe order pinned by M5's ag008).
    /// Executes in P5-M5.
    Aggregate {
        keys: Vec<String>,
        aggs: Vec<AggCall>,
    },
    /// P5-M2/P5-M6 (ND-02/ND-06): JOIN <right_type> ON <on_left> ==
    /// <on_right>. Consumes the left-side RowSet; the right side is scanned
    /// at execution time with the caller's subject/roles/tenant — the join
    /// can never see rows outside that scope (the cross-tenant fail-closed
    /// pin, jn006). Executes in P5-M6.
    Join {
        right_type: String,
        on_left: String,
        on_right: String,
        kind: JoinKind,
    },
    /// Ingest an artifact into the knowledge base via the ingestion pipeline
    /// (§62): the runtime reads the artifact at `artifact_ref`, hashes it, and
    /// deploys the Document KO (`aikoql:document`). Standalone operator — an
    /// INGEST plan has exactly one op.
    Ingest { artifact_ref: String },
}

// ---------------------------------------------------------------------------
// IR Plan
// ---------------------------------------------------------------------------

/// P5-M3 (ND-03): the logical plan — a linear sequence of operators
/// forming a pipeline, storage-independent (qm004: no v2/engine types may
/// appear here). Each operator consumes the output of the previous one.
/// `version` stamps the serialized form (qm003).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LogicalPlan {
    pub version: u32,
    pub operators: Vec<IrOp>,
    pub description: Option<String>,
}

/// The logical plan's wire-format version (qm003). Bump on any breaking
/// plan-shape change.
pub const PLAN_VERSION: u32 = 1;

/// Pre-M3 name: the logical plan IS what `IrPlan` always was. The alias keeps
/// every constructor site compiling; new code should say `LogicalPlan`.
pub type IrPlan = LogicalPlan;

impl LogicalPlan {
    pub fn new(operators: Vec<IrOp>) -> Self {
        LogicalPlan {
            version: PLAN_VERSION,
            operators,
            description: None,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Validate the plan structure. Returns `Ok(())` if the operator
    /// sequence is legal, or an error describing the first violation.
    pub fn validate(&self) -> KResult<()> {
        let ops: Vec<&IrOp> = self.operators.iter().collect();
        validate_ops(&ops)
    }
}

// ---------------------------------------------------------------------------
// P5-M3 (ND-03): physical plan
// ---------------------------------------------------------------------------

/// Storage/index strategy for one operator. v1 has exactly one strategy per
/// operator kind — the seam exists so P5-M9's CBO can add alternatives
/// (e.g. full-scan vs index-lookup for Filter).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Strategy {
    /// Scan all readable rows of a type (no index).
    FullScan,
    /// Scan answered from a property index: the following Filter's Eq key
    /// lookup (P5-M9, ND-08). Chosen only when the stats are fresh AND the
    /// index verifies clean against the heads, so the assisted scan answers
    /// exactly the committed truth.
    PropertyIndex,
    /// ANN vector similarity via the vector index.
    VectorIndex,
    /// Full-text search via the text index (BM25).
    TextIndex,
    /// In-memory row processing — no storage strategy.
    Inline,
    /// Join executed as a nested loop over the (filtered) left side and a
    /// scan of the right side (P5-M6, ND-06). Hash join is the documented
    /// upgrade path — strategy selection lands with the P5-M9 CBO seam.
    NestedLoop,
}

/// One physical operator: a logical op plus the strategy that executes it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PhysicalOp {
    pub op: IrOp,
    pub strategy: Strategy,
}

/// The executable plan: logical pipeline + per-operator strategy. The runtime
/// interpreter consumes this form (P5-M3).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PhysicalPlan {
    pub version: u32,
    pub operators: Vec<PhysicalOp>,
    pub description: Option<String>,
    /// P5-M21 (PR6 P0-07): the journal head the optimizer pinned at optimize
    /// time. The executor re-pins before serving an index assist —
    /// `applied_seq == pinned_head` at exec time or the scan falls back to
    /// the full scan. 0 = unpinned (a plan the CBO never assisted).
    /// Executor-internal state: not on the wire (qm003 keeps its byte pin).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pinned_head: u64,
}

/// serde helper: 0 is the unpinned sentinel — it never needs the wire.
fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl PhysicalPlan {
    pub fn new(operators: Vec<PhysicalOp>) -> Self {
        PhysicalPlan {
            version: PLAN_VERSION,
            operators,
            description: None,
            pinned_head: 0,
        }
    }

    /// Lower a logical pipeline to the physical form using the v1 strategy
    /// rules: Scan → FullScan, AnnSearch → VectorIndex, TextSearch →
    /// TextIndex, everything else → Inline.
    pub fn from_ops(operators: Vec<IrOp>) -> Self {
        PhysicalPlan::new(
            operators
                .into_iter()
                .map(|op| {
                    let strategy = match op {
                        IrOp::Scan { .. } => Strategy::FullScan,
                        IrOp::AnnSearch { .. } => Strategy::VectorIndex,
                        IrOp::TextSearch { .. } => Strategy::TextIndex,
                        // ND-06 acceptance: EXPLAIN exposes the join strategy.
                        IrOp::Join { .. } => Strategy::NestedLoop,
                        _ => Strategy::Inline,
                    };
                    PhysicalOp { op, strategy }
                })
                .collect(),
        )
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Validate the operator sequence (same rules as the logical plan).
    pub fn validate(&self) -> KResult<()> {
        let ops: Vec<&IrOp> = self.operators.iter().map(|p| &p.op).collect();
        validate_ops(&ops)
    }

    /// One line per operator: the logical op plus its strategy — the EXPLAIN
    /// surface (qm002: the strategy must be visible here).
    pub fn summary(&self) -> Vec<String> {
        self.operators
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let name = format!("{:?}", p.op);
                let name = name.split('{').next().unwrap_or(&name).trim();
                format!("{:>2}: {:<22} [{:?}]", i, name, p.strategy)
            })
            .collect()
    }
}

/// Shared validation: first op Scan/Traverse/Ingest, one Scan, search ops
/// after Scan, Fuse after a search op. (IrPlan::validate delegates here.)
fn validate_ops(ops: &[&IrOp]) -> KResult<()> {
    if ops.is_empty() {
        return Err(KError::InvalidQuery("IR plan has no operators".into()));
    }
    let first = ops[0];
    match first {
        IrOp::Scan { .. } | IrOp::Traverse { .. } | IrOp::Ingest { .. } => {}
        _ => {
            return Err(KError::InvalidQuery(
                "first IR operator must be Scan, Traverse or Ingest".into(),
            ))
        }
    }
    let seen_scan = matches!(first, IrOp::Scan { .. });
    let mut seen_search = false;
    for (i, op) in ops.iter().enumerate().skip(1) {
        match op {
            IrOp::Scan { .. } => {
                return Err(KError::InvalidQuery(format!(
                    "Scan at position {}: only one Scan allowed",
                    i
                )))
            }
            IrOp::Traverse { .. } => {}
            IrOp::Filter { .. } if !seen_scan => {
                return Err(KError::InvalidQuery(format!(
                    "Filter at position {}: requires Scan",
                    i
                )))
            }
            IrOp::Temporal { .. }
            | IrOp::EpistemicFilter { .. }
            | IrOp::ProvenanceFilter { .. }
            | IrOp::Limit { .. }
            | IrOp::Sort { .. }
            | IrOp::Aggregate { .. }
            | IrOp::Join { .. }
                if !seen_scan =>
            {
                return Err(KError::InvalidQuery(format!(
                    "{:?} at position {}: requires Scan",
                    op, i
                )))
            }
            IrOp::AnnSearch { .. } | IrOp::TextSearch { .. } => {
                if !seen_scan {
                    return Err(KError::InvalidQuery(format!(
                        "{:?} at position {}: requires Scan",
                        op, i
                    )));
                }
                seen_search = true;
            }
            IrOp::Fuse { .. } if !seen_search => {
                return Err(KError::InvalidQuery(
                    "Fuse requires at least one search operator".into(),
                ))
            }
            IrOp::Fuse { .. } => {}
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ir_plan_for_scan_and_filter() {
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "fact".into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq("temperature", Value::Int(35))],
            },
        ])
        .with_description("find hot facts");
        assert_eq!(plan.operators.len(), 2);
        assert_eq!(plan.description.as_deref(), Some("find hot facts"));
    }

    #[test]
    fn ir_plan_for_hybrid_recall() {
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "note".into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::AnnSearch {
                vector: vec![1.0, 0.0],
                query_text: None,
                embedding_model: Some("bge-m3".into()),
                k: 5,
            },
            IrOp::TextSearch {
                query: "cats".into(),
                k: 5,
                scoring: None,
            },
            IrOp::Fuse {
                mode: FuseMode::Rrf { k0: 60 },
            },
        ]);
        assert_eq!(plan.operators.len(), 4);
    }

    #[test]
    fn ir_plan_for_traverse() {
        let plan = IrPlan::new(vec![IrOp::Traverse {
            start_koid: "abcdef1234567890abcdef1234567890".into(),
            rel_type: Some("references".into()),
            depth: 2,
        }])
        .with_description("find related notes");
        assert_eq!(plan.operators.len(), 1);
    }

    #[test]
    fn validate_rejects_empty_plan() {
        assert!(IrPlan::new(vec![]).validate().is_err());
    }

    #[test]
    fn validate_rejects_filter_as_first_op() {
        assert!(IrPlan::new(vec![IrOp::Filter { predicates: vec![] }])
            .validate()
            .is_err());
    }

    #[test]
    fn validate_accepts_scan_filter() {
        assert!(IrPlan::new(vec![
            IrOp::Scan {
                type_name: "fact".into(),
                subject: "a".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Filter { predicates: vec![] },
        ])
        .validate()
        .is_ok());
    }

    #[test]
    fn validate_rejects_second_scan() {
        assert!(IrPlan::new(vec![
            IrOp::Scan {
                type_name: "a".into(),
                subject: "x".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Scan {
                type_name: "b".into(),
                subject: "x".into(),
                roles: vec![],
                tenant: None,
            },
        ])
        .validate()
        .is_err());
    }

    fn scan() -> IrOp {
        IrOp::Scan {
            type_name: "fact".into(),
            subject: "a".into(),
            roles: vec![],
            tenant: None,
        }
    }

    #[test]
    fn validate_accepts_temporal_and_epistemic_ops() {
        assert!(IrPlan::new(vec![
            scan(),
            IrOp::Temporal {
                op: TemporalOp::AsOf(1_000),
            },
            IrOp::EpistemicFilter {
                allowed: vec!["verified".into(), "asserted".into()],
            },
            IrOp::Filter { predicates: vec![] },
        ])
        .validate()
        .is_ok());
    }

    #[test]
    fn validate_rejects_temporal_without_scan() {
        assert!(IrPlan::new(vec![IrOp::Temporal {
            op: TemporalOp::Historical,
        }])
        .validate()
        .is_err());
        assert!(IrPlan::new(vec![IrOp::EpistemicFilter {
            allowed: vec!["verified".into()],
        }])
        .validate()
        .is_err());
    }
}
