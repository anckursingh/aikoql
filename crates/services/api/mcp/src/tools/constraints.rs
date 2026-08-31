//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::helpers::*;
use crate::session::*;
use crate::{
    json, Kernel, KnowledgeContext, Origin, ReferentialPolicy, RememberRequest, Value, J, KOID,
};
pub(crate) fn tool_decide(k: &Kernel, args: &J) -> Result<J, String> {
    let koid_hex = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let koid = KOID::from_hex(koid_hex).map_err(|e| e.to_string())?;
    let decision = args
        .get("decision")
        .and_then(|v| v.as_str())
        .ok_or("missing: decision")?;
    let rationale = args.get("rationale").and_then(|v| v.as_str()).unwrap_or("");
    let confidence = args
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let subject = subject_of(args);

    // Load the target KO and record the decision as a provenance-tagged update.
    let ko = k
        .get(KnowledgeContext::from(subject.clone()), &koid)
        .map_err(|e| e.to_string())?;
    let mut props = ko.properties.clone();
    props.insert("_decision".into(), Value::Text(decision.to_string()));
    props.insert("_rationale".into(), Value::Text(rationale.to_string()));
    props.insert("_confidence".into(), Value::Float(confidence));
    props.insert("_decided_by".into(), Value::Text(subject.name.clone()));
    let r = k
        .remember(RememberRequest {
            context: subject.into(),
            koid: Some(koid),
            expected_version: Some(ko.version),
            idempotency_key: Some(format!("decide-{}-{}", koid_hex, decision)),
            metadata: ko.metadata.clone(),
            properties: props,
            semantic: None,
            relationships: ko.relationships.clone(),
            security: Some(ko.security.clone()),
            extensions: ko.extensions.clone(),
            origin: Origin::Reason,
            note: Some(format!(
                "Decision: {} (confidence: {:.2}) — {}",
                decision, confidence, rationale
            )),
            referential_policy: ReferentialPolicy::Permissive,
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "decision": decision,
        "confidence": confidence,
        "recorded": true,
    }))
}

pub(crate) fn tool_reason(k: &Kernel, args: &J) -> Result<J, String> {
    let rule_type = args
        .get("type_name")
        .and_then(|v| v.as_str())
        .ok_or("missing: type_name")?;
    let rule_props = parse_properties(args)?;
    let claims = k.reason(rule_type, rule_props).map_err(|e| e.to_string())?;
    Ok(json!({
        "claims": claims.iter().map(|c| json!({
            "type_name": c.metadata.type_name,
            "property_count": c.properties.len(),
            "origin": format!("{:?}", c.lifecycle.origin),
        })).collect::<Vec<_>>(),
        "count": claims.len(),
    }))
}

pub(crate) fn tool_infer(k: &Kernel, args: &J) -> Result<J, String> {
    let type_name = args
        .get("type_name")
        .and_then(|v| v.as_str())
        .ok_or("missing: type_name")?;
    let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let results = k
        .infer(&subject_of(args), type_name, text)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "results": results.iter().map(|s| json!({
            "koid": s.ko.koid.to_hex(),
            "score": s.score,
            "type_name": s.ko.metadata.type_name,
        })).collect::<Vec<_>>(),
        "count": results.len(),
    }))
}

pub(crate) fn tool_predict(kernel: &Kernel, args: &J) -> Result<J, String> {
    let type_name = args
        .get("type_name")
        .and_then(|v| v.as_str())
        .ok_or("missing: type_name")?;
    let props = parse_properties(args)?;
    let top_k = args.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
    let merged = kernel
        .predict(&subject_of(args), type_name, &props, top_k)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "predicted": merged.iter().map(|(key, val)| (key.clone(), value_to_json(val))).collect::<serde_json::Map<_,_>>(),
    }))
}
