//! Mnemosyne Compiler — AIKOQL → Knowledge IR.
//!
//! Two frontends, one target:
//! - `Compiler::compile(json)` — JSON-based AIKOQL (simple, agent-friendly)
//! - `parser::compile(source)` — text-based AIKOQL (human-friendly, per MRFC-0010)
//!
//! Both produce `IrPlan` for execution by the runtime.
//!
//! MRFC-0005 §Compiler Layer, MRFC-0010 §Parser Architecture.

use mnemosyne_kernel::ir::*;
use serde::Deserialize;

pub mod parser;
pub mod planner;
pub mod semantic;

// ---------------------------------------------------------------------------
// AIKOQL JSON schema
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AiKoqlQuery {
    #[serde(default)]
    scan: Option<ScanClause>,
    #[serde(default)]
    traverse: Option<TraverseClause>,
    #[serde(default)]
    filter: Option<FilterClause>,
    #[serde(default)]
    search: Option<SearchClause>,
    #[serde(default)]
    fuse: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ScanClause {
    #[serde(rename = "type")]
    type_name: String,
    subject: String,
}

#[derive(Debug, Deserialize)]
struct TraverseClause {
    start: String,
    #[serde(default)]
    rel_type: Option<String>,
    #[serde(default = "default_depth")]
    depth: usize,
}

fn default_depth() -> usize {
    1
}

#[derive(Debug, Deserialize)]
struct FilterClause {
    #[serde(flatten)]
    predicates: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct SearchClause {
    #[serde(default)]
    vector: Option<Vec<f32>>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default = "default_k")]
    k: usize,
}

fn default_k() -> usize {
    5
}

// ---------------------------------------------------------------------------
// Compiler
// ---------------------------------------------------------------------------

pub struct Compiler;

impl Compiler {
    /// Compile an AIKOQL JSON string into a validated `IrPlan`.
    pub fn compile(json: &str) -> Result<IrPlan, String> {
        let query: AiKoqlQuery =
            serde_json::from_str(json).map_err(|e| format!("parse error: {}", e))?;
        Self::compile_query(&query)
    }

    fn compile_query(q: &AiKoqlQuery) -> Result<IrPlan, String> {
        let mut ops = Vec::new();

        // Must have either scan or traverse.
        match (&q.scan, &q.traverse) {
            (None, None) => return Err("AIKOQL: either 'scan' or 'traverse' is required".into()),
            (Some(_), Some(_)) => {
                return Err("AIKOQL: 'scan' and 'traverse' are mutually exclusive".into())
            }
            _ => {}
        }

        // Compile scan.
        if let Some(scan) = &q.scan {
            ops.push(IrOp::Scan {
                type_name: scan.type_name.clone(),
                subject: scan.subject.clone(),
            });
        }

        // Compile traverse.
        if let Some(traverse) = &q.traverse {
            ops.push(IrOp::Traverse {
                start_koid: traverse.start.clone(),
                rel_type: traverse.rel_type.clone(),
                depth: traverse.depth,
            });
        }

        // Compile filter.
        if let Some(filter) = &q.filter {
            let mut predicates = Vec::new();
            for (prop, val) in &filter.predicates {
                let value = json_to_value(val)?;
                predicates.push(Predicate::eq(prop.clone(), value));
            }
            if !predicates.is_empty() {
                ops.push(IrOp::Filter { predicates });
            }
        }

        // Compile search.
        if let Some(search) = &q.search {
            if let Some(ref v) = search.vector {
                if q.scan.is_none() {
                    return Err("AIKOQL: vector search requires 'scan'".into());
                }
                ops.push(IrOp::AnnSearch {
                    vector: v.clone(),
                    embedding_model: search.model.clone(),
                    k: search.k,
                });
            }
            if let Some(ref t) = search.text {
                if q.scan.is_none() {
                    return Err("AIKOQL: text search requires 'scan'".into());
                }
                ops.push(IrOp::TextSearch {
                    query: t.clone(),
                    k: search.k,
                });
            }
        }

        // Compile fuse.
        if let Some(fuse) = &q.fuse {
            let has_vector = q.search.as_ref().and_then(|s| s.vector.as_ref()).is_some();
            let has_text = q.search.as_ref().and_then(|s| s.text.as_ref()).is_some();
            if !has_vector || !has_text {
                return Err("AIKOQL: 'fuse' requires both vector and text search".into());
            }
            let mode = match fuse.as_str() {
                "rrf" => FuseMode::Rrf { k0: 60 },
                "weighted" => FuseMode::Weighted { wv: 0.5, wt: 0.5 },
                "vector" => FuseMode::VectorOnly,
                "text" => FuseMode::TextOnly,
                other => return Err(format!("AIKOQL: unknown fuse mode '{}'", other)),
            };
            ops.push(IrOp::Fuse { mode });
        }

        let plan = IrPlan::new(ops).with_description("AIKOQL query".to_string());
        plan.validate()
            .map_err(|e| format!("AIKOQL1014: conflicting clauses — {}", e))?;
        Ok(plan)
    }
}

fn json_to_value(v: &serde_json::Value) -> Result<mnemosyne_kernel::Value, String> {
    use mnemosyne_kernel::Value;
    Ok(match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                return Err("unsupported number".into());
            }
        }
        serde_json::Value::String(s) => Value::Text(s.clone()),
        serde_json::Value::Array(_) => return Err("arrays not supported in filter values".into()),
        serde_json::Value::Object(_) => return Err("objects not supported in filter values".into()),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_scan_only() {
        let json = r#"{"scan": {"type": "fact", "subject": "alice"}}"#;
        let plan = Compiler::compile(json).unwrap();
        assert_eq!(plan.operators.len(), 1);
        match &plan.operators[0] {
            IrOp::Scan { type_name, subject } => {
                assert_eq!(type_name, "fact");
                assert_eq!(subject, "alice");
            }
            _ => panic!("expected Scan"),
        }
    }

    #[test]
    fn compile_scan_filter_search_fuse() {
        let json = r#"{
            "scan": {"type": "note", "subject": "alice"},
            "filter": {"category": "pets"},
            "search": {"vector": [1.0, 0.0], "text": "cats", "model": "bge-m3", "k": 10},
            "fuse": "rrf"
        }"#;
        let plan = Compiler::compile(json).unwrap();
        assert_eq!(plan.operators.len(), 5); // Scan + Filter + AnnSearch + TextSearch + Fuse
    }

    #[test]
    fn compile_traverse() {
        let json = r#"{"traverse": {"start": "abcdef1234567890abcdef1234567890", "rel_type": "refs", "depth": 2}}"#;
        let plan = Compiler::compile(json).unwrap();
        assert_eq!(plan.operators.len(), 1);
    }

    #[test]
    fn rejects_no_scan_or_traverse() {
        assert!(Compiler::compile(r#"{"filter": {}}"#).is_err());
    }

    #[test]
    fn rejects_both_scan_and_traverse() {
        let json =
            r#"{"scan": {"type": "f", "subject": "a"}, "traverse": {"start": "00", "depth": 1}}"#;
        assert!(Compiler::compile(json).is_err());
    }

    #[test]
    fn rejects_unknown_field() {
        assert!(
            Compiler::compile(r#"{"scan": {"type": "f", "subject": "a"}, "bogus": 1}"#).is_err()
        );
    }
}
