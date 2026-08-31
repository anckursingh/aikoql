//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::helpers::*;
use crate::session::*;
use crate::{
    json, EvalContradictionQuery, EvalRecallQuery, EvalStalenessQuery, HashSet, Kernel, J, KOID,
};
pub(crate) fn tool_eval_recall(k: &Kernel, args: &J) -> Result<J, String> {
    let expected: HashSet<KOID> = args
        .get("expected")
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .filter_map(|s| KOID::from_hex(s).ok())
                .collect()
        })
        // justified: absent expected param → empty set
        .unwrap_or_default();
    let report = k
        .eval_recall(EvalRecallQuery {
            context: subject_of(args).into(),
            type_name: args
                .get("type_name")
                .and_then(|t| t.as_str())
                .map(String::from),
            text: args.get("text").and_then(|t| t.as_str()).map(String::from),
            vector: parse_vector(args)?,
            k: args.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize,
            fusion: parse_fusion(args),
            expected,
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "k": report.k,
        "returned": report.returned,
        "expected": report.expected,
        "hits": report.hits,
        "recall": report.recall,
        "missing": report.missing.iter().map(|k| k.to_hex()).collect::<Vec<_>>(),
        "mean_lag_ms": report.mean_lag_ms,
        "max_lag_ms": report.max_lag_ms,
        "p95_lag_ms": report.p95_lag_ms,
    }))
}

pub(crate) fn tool_eval_staleness(k: &Kernel, args: &J) -> Result<J, String> {
    let report = k
        .eval_staleness(EvalStalenessQuery {
            context: subject_of(args).into(),
            type_name: args
                .get("type_name")
                .and_then(|t| t.as_str())
                .map(String::from),
            text: args.get("text").and_then(|t| t.as_str()).map(String::from),
            vector: parse_vector(args)?,
            k: args.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize,
            fusion: parse_fusion(args),
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "results": report.results,
        "mean_lag_ms": report.mean_lag_ms,
        "max_lag_ms": report.max_lag_ms,
        "p95_lag_ms": report.p95_lag_ms,
    }))
}

pub(crate) fn tool_eval_contradictions(k: &Kernel, args: &J) -> Result<J, String> {
    let type_name = args
        .get("type_name")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: type_name")?;
    let property = args
        .get("property")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: property")?;
    let q = EvalContradictionQuery {
        context: subject_of(args).into(),
        type_name: type_name.into(),
        property: property.into(),
        similarity_threshold: args
            .get("threshold")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.9) as f32,
        max_results: args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize,
    };
    let hits = k.eval_contradictions(q).map_err(|e| e.to_string())?;
    Ok(json!({
        "contradictions": hits.iter().map(|c| json!({
            "left": c.left.to_hex(),
            "right": c.right.to_hex(),
            "score": c.score,
            "reason": c.reason,
        })).collect::<Vec<_>>()
    }))
}
