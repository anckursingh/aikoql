//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::helpers::*;
use crate::session::*;
use crate::{
    json, CardinalityConstraint, CheckConstraint, CheckExpression, ConstraintTiming,
    EnforcementMode, Kernel, KnowledgeContext, Origin, ReferentialPolicy, RememberRequest, Schema,
    SchemaProperty, TemporalConstraint, UniqueConstraint, UniquenessScope, Value,
    ViolationSeverity, J, KOID,
};

fn parse_mode(v: Option<&str>) -> Result<EnforcementMode, String> {
    match v.unwrap_or("Enforced") {
        "Enforced" => Ok(EnforcementMode::Enforced),
        "Validated" => Ok(EnforcementMode::Validated),
        "Advisory" => Ok(EnforcementMode::Advisory),
        "Disabled" => Ok(EnforcementMode::Disabled),
        other => Err(format!("invalid mode: {}", other)),
    }
}

fn parse_severity(v: Option<&str>) -> Result<ViolationSeverity, String> {
    match v.unwrap_or("Error") {
        "Error" => Ok(ViolationSeverity::Error),
        "Warning" => Ok(ViolationSeverity::Warning),
        "Info" => Ok(ViolationSeverity::Info),
        other => Err(format!("invalid severity: {}", other)),
    }
}

fn parse_scope(v: Option<&str>) -> Result<UniquenessScope, String> {
    match v.unwrap_or("Type") {
        "Type" => Ok(UniquenessScope::Type),
        "Tenant" => Ok(UniquenessScope::Tenant),
        "Global" => Ok(UniquenessScope::Global),
        other => Err(format!("invalid scope: {}", other)),
    }
}

fn parse_timing(v: Option<&str>) -> Result<ConstraintTiming, String> {
    match v.unwrap_or("Immediate") {
        "Immediate" => Ok(ConstraintTiming::Immediate),
        "Deferred" => Ok(ConstraintTiming::Deferred),
        other => Err(format!("invalid timing: {}", other)),
    }
}
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

/// P3-M5 M5a: diagnostics surface for the constraint engine — the violation
/// event ring (capped at 256, oldest evicted) plus evaluation counters.
pub(crate) fn tool_constraint_diagnostics(k: &Kernel, _args: &J) -> Result<J, String> {
    let events: Vec<J> = k
        .violation_events()
        .into_iter()
        .map(|v| {
            json!({
                "constraint": v.constraint_name,
                "message": v.message,
                "severity": format!("{:?}", v.severity),
                "mode": format!("{:?}", v.mode),
                "timestamp": v.timestamp,
                "koid": v.koid.map(|kid| kid.to_hex()),
            })
        })
        .collect();
    let stats = k.constraint_stats();
    Ok(json!({
        "events": events,
        "stats": {
            "evaluated": stats.evaluated,
            "skipped_disabled": stats.skipped_disabled,
            "skipped_unaffected": stats.skipped_unaffected,
        },
    }))
}

/// P3-M5 follow-up: register a constraint-bearing schema (MRFC-0060 §7/§30/§31).
/// Check predicates are strings parsed by `CheckExpression::parse`. Mode/severity
/// default to Enforced/Error; timing to Immediate; scope to Type.
pub(crate) fn tool_register_schema(k: &Kernel, args: &J) -> Result<J, String> {
    let type_name = args
        .get("type_name")
        .and_then(|v| v.as_str())
        .ok_or("missing: type_name")?;
    let schema_version = args
        .get("schema_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;
    let mut schema = Schema::new(type_name, schema_version);
    if let Some(props) = args.get("properties").and_then(|v| v.as_array()) {
        for p in props {
            let name = p
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("property missing name")?;
            let value_type = p
                .get("value_type")
                .and_then(|v| v.as_str())
                .ok_or("property missing value_type")?;
            schema.properties.push(SchemaProperty {
                name: name.to_string(),
                value_type: value_type.to_string(),
                required: p.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
                nullable: p.get("nullable").and_then(|v| v.as_bool()).unwrap_or(true),
                provenance_required: p
                    .get("provenance_required")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                domain_constraints: Vec::new(),
            });
        }
    }
    if let Some(us) = args.get("uniques").and_then(|v| v.as_array()) {
        for u in us {
            let mut properties = Vec::new();
            if let Some(ps) = u.get("properties").and_then(|v| v.as_array()) {
                for p in ps {
                    properties.push(
                        p.as_str()
                            .ok_or("unique property must be a string")?
                            .to_string(),
                    );
                }
            }
            schema.unique_constraints.push(UniqueConstraint {
                properties,
                scope: parse_scope(u.get("scope").and_then(|v| v.as_str()))?,
                timing: parse_timing(u.get("timing").and_then(|v| v.as_str()))?,
                mode: parse_mode(u.get("mode").and_then(|v| v.as_str()))?,
                severity: parse_severity(u.get("severity").and_then(|v| v.as_str()))?,
            });
        }
    }
    if let Some(cs) = args.get("checks").and_then(|v| v.as_array()) {
        for c in cs {
            let name = c
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("check missing name")?;
            let expr = c
                .get("expr")
                .and_then(|v| v.as_str())
                .ok_or("check missing expr")?;
            schema.check_constraints.push(CheckConstraint {
                name: name.to_string(),
                predicate: CheckExpression::parse(expr)
                    .map_err(|e| format!("check '{}': {}", name, e))?,
                timing: parse_timing(c.get("timing").and_then(|v| v.as_str()))?,
                mode: parse_mode(c.get("mode").and_then(|v| v.as_str()))?,
                severity: parse_severity(c.get("severity").and_then(|v| v.as_str()))?,
            });
        }
    }
    if let Some(cs) = args.get("cardinality").and_then(|v| v.as_array()) {
        for c in cs {
            let name = c
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("cardinality missing name")?;
            let relationship_type = c
                .get("relationship_type")
                .and_then(|v| v.as_str())
                .ok_or("cardinality missing relationship_type")?;
            schema.cardinality_constraints.push(CardinalityConstraint {
                name: name.to_string(),
                relationship_type: relationship_type.to_string(),
                min_outbound: c.get("min").and_then(|v| v.as_u64()).map(|n| n as u32),
                max_outbound: c.get("max").and_then(|v| v.as_u64()).map(|n| n as u32),
                mode: parse_mode(c.get("mode").and_then(|v| v.as_str()))?,
                severity: parse_severity(c.get("severity").and_then(|v| v.as_str()))?,
            });
        }
    }
    if let Some(ts) = args.get("temporal").and_then(|v| v.as_array()) {
        for t in ts {
            let name = t
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("temporal missing name")?;
            let start_property = t
                .get("start")
                .and_then(|v| v.as_str())
                .ok_or("temporal missing start")?
                .to_string();
            let end_property = t
                .get("end")
                .and_then(|v| v.as_str())
                .ok_or("temporal missing end")?
                .to_string();
            schema.temporal_constraints.push(TemporalConstraint {
                name: name.to_string(),
                start_property,
                end_property,
                mode: parse_mode(t.get("mode").and_then(|v| v.as_str()))?,
                severity: parse_severity(t.get("severity").and_then(|v| v.as_str()))?,
            });
        }
    }
    k.register_schema(schema).map_err(|e| e.to_string())?;
    Ok(json!({ "type_name": type_name, "registered": true }))
}
