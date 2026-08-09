//! Knowledge IR — the intermediate representation between query frontends
//! and the runtime interpreter (MRFC-0005 §Compiler Layer).
//!
//! Every frontend (MCP, SQL, GraphQL, AIKOQL) compiles to this operator DAG.
//! The runtime interpreter executes it against the Knowledge Kernel.
//!
//! Design: linear pipeline for v1 (no joins, no subqueries). Operators are
//! executed in order; each produces a result set consumed by the next.
//! Full DAG with branching/merging lands when joins or subqueries arrive.

use crate::knowledge::kom::{KError, KResult, Value};

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum PredOp {
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
}

#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq)]
pub enum FuseMode {
    Rrf { k0: usize },
    Weighted { wv: f32, wt: f32 },
    VectorOnly,
    TextOnly,
}

// ---------------------------------------------------------------------------
// IR operators
// ---------------------------------------------------------------------------

/// One node in the Knowledge IR operator DAG.
#[derive(Clone, Debug, PartialEq)]
pub enum IrOp {
    /// Scan all readable KOs of `type_name` as `subject`.
    Scan { type_name: String, subject: String },
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
        embedding_model: Option<String>,
        k: usize,
    },
    /// Full-text search over the current result set.
    TextSearch { query: String, k: usize },
    /// Fuse two ranked result sets into one (RRF or weighted).
    Fuse { mode: FuseMode },
    /// Project specific fields from the result set.
    Project { fields: Vec<String> },
}

// ---------------------------------------------------------------------------
// IR Plan
// ---------------------------------------------------------------------------

/// A complete IR plan: a linear sequence of operators forming a pipeline.
/// Each operator consumes the output of the previous operator.
#[derive(Clone, Debug, PartialEq)]
pub struct IrPlan {
    pub operators: Vec<IrOp>,
    pub description: Option<String>,
}

impl IrPlan {
    pub fn new(operators: Vec<IrOp>) -> Self {
        IrPlan {
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
        if self.operators.is_empty() {
            return Err(KError::InvalidQuery("IR plan has no operators".into()));
        }
        let first = &self.operators[0];
        match first {
            IrOp::Scan { .. } | IrOp::Traverse { .. } => {}
            _ => {
                return Err(KError::InvalidQuery(
                    "first IR operator must be Scan or Traverse".into(),
                ))
            }
        }
        let seen_scan = matches!(first, IrOp::Scan { .. });
        let mut seen_search = false;
        for (i, op) in self.operators.iter().enumerate().skip(1) {
            match op {
                IrOp::Scan { .. } => {
                    return Err(KError::InvalidQuery(format!(
                        "Scan at position {}: only one Scan allowed",
                        i
                    )))
                }
                IrOp::Traverse { .. } => {
                    // Set-based Traverse after Scan is valid — empty start_koid
                    // consumes the input RowSet. Standalone Traverse (first op) with
                    // explicit start_koid is also valid.
                }
                IrOp::Filter { .. } if !seen_scan => {
                    return Err(KError::InvalidQuery(format!(
                        "Filter at position {}: requires Scan",
                        i
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
                IrOp::Fuse { .. } => {} // ok
                _ => {}                 // Filter, Traverse, etc. — validated above by position
            }
        }
        Ok(())
    }
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
            },
            IrOp::AnnSearch {
                vector: vec![1.0, 0.0],
                embedding_model: Some("bge-m3".into()),
                k: 5,
            },
            IrOp::TextSearch {
                query: "cats".into(),
                k: 5,
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
            },
            IrOp::Scan {
                type_name: "b".into(),
                subject: "x".into(),
            },
        ])
        .validate()
        .is_err());
    }
}
