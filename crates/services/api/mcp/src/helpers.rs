//! JSON↔kernel conversion helpers shared by every tool module.
//! Extracted from main.rs (R7 modularization). No behavior changes.

use crate::*;
pub(crate) fn koid_of(args: &J) -> Result<KOID, String> {
    let hex = args
        .get("koid")
        .and_then(|s| s.as_str())
        .ok_or("missing argument: koid")?;
    KOID::from_hex(hex).map_err(|e| e.to_string())
}

pub(crate) fn json_to_value(j: &J) -> Result<Value, String> {
    Ok(match j {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                return Err("unsupported number".into());
            }
        }
        J::String(s) => Value::Text(s.clone()),
        J::Array(xs) => Value::List(
            xs.iter()
                .map(json_to_value)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        J::Object(m) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in m {
                out.insert(k.clone(), json_to_value(v)?);
            }
            Value::Map(out)
        }
    })
}

pub(crate) fn value_to_json(v: &Value) -> J {
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(i) => json!(i),
        Value::Float(f) => json!(f),
        Value::Text(s) => J::String(s.clone()),
        Value::Bytes(b) => json!(format!("{} bytes", b.len())),
        Value::List(xs) => J::Array(xs.iter().map(value_to_json).collect()),
        Value::Map(m) => {
            let mut out = serde_json::Map::new();
            for (k, v) in m {
                out.insert(k.clone(), value_to_json(v));
            }
            J::Object(out)
        }
    }
}

pub(crate) fn parse_properties(args: &J) -> Result<PropertyMap, String> {
    let mut out = PropertyMap::new();
    if let Some(J::Object(m)) = args.get("properties") {
        for (k, v) in m {
            out.insert(k.clone(), json_to_value(v)?);
        }
    }
    Ok(out)
}

/// v0.3 K1: `extensions` argument — epistemic state, authority, scope, and
/// canonical evidence can be declared at the protocol boundary.
pub(crate) fn parse_extensions(args: &J) -> Result<ExtensionMap, String> {
    let mut out = ExtensionMap::new();
    if let Some(J::Object(m)) = args.get("extensions") {
        for (k, v) in m {
            out.insert(k.clone(), json_to_value(v)?);
        }
    }
    Ok(out)
}

pub(crate) fn parse_semantic(args: &J) -> Result<Option<SemanticBlock>, String> {
    let Some(s) = args.get("semantic") else {
        return Ok(None);
    };
    let embedding = match s.get("embedding") {
        Some(J::Array(xs)) => Some(
            xs.iter()
                .map(|x| {
                    x.as_f64()
                        .map(|f| f as f32)
                        .ok_or("embedding must be numbers")
                })
                .collect::<Result<Vec<f32>, _>>()?,
        ),
        _ => None,
    };
    Ok(Some(SemanticBlock {
        embedding_model: s
            .get("embedding_model")
            .and_then(|x| x.as_str())
            .map(String::from),
        embedding,
        confidence: s
            .get("confidence")
            .and_then(|x| x.as_f64())
            .map(|f| f as f32),
        source: s.get("source").and_then(|x| x.as_str()).map(String::from),
        summary: s.get("summary").and_then(|x| x.as_str()).map(String::from),
    }))
}

pub(crate) fn parse_origin(args: &J) -> Origin {
    match args.get("origin").and_then(|o| o.as_str()) {
        Some("system") => Origin::System,
        Some("reason") => Origin::Reason,
        Some("semantic_enrichment") => Origin::SemanticEnrichment,
        Some(other) => Origin::Agent(other.into()),
        None => Origin::Agent("mcp-agent".into()),
    }
}

pub(crate) fn parse_state(args: &J) -> Result<LifecycleState, String> {
    match args.get("to").and_then(|s| s.as_str()).unwrap_or("") {
        "draft" => Ok(LifecycleState::Draft),
        "active" => Ok(LifecycleState::Active),
        "verified" => Ok(LifecycleState::Verified),
        "archived" => Ok(LifecycleState::Archived),
        "deleted" => Ok(LifecycleState::Deleted),
        other => Err(format!("invalid lifecycle state: {}", other)),
    }
}

pub(crate) fn parse_action(args: &J) -> Result<Action, String> {
    match args.get("action").and_then(|s| s.as_str()).unwrap_or("") {
        "read" => Ok(Action::Read),
        "write" => Ok(Action::Write),
        "evolve" => Ok(Action::Evolve),
        "delete" => Ok(Action::Delete),
        "admin" => Ok(Action::Admin),
        other => Err(format!("invalid action: {}", other)),
    }
}

/// Parse the canonical evidence array (K1/K3/K4 tools share this shape):
/// [{source_artifact, method, location?, revision?, confidence?}]
pub(crate) fn parse_evidence(args: &J) -> Result<Vec<Evidence>, String> {
    let mut out = Vec::new();
    if let Some(evs) = args.get("evidence").and_then(|e| e.as_array()) {
        for ev in evs {
            let source_artifact = ev
                .get("source_artifact")
                .and_then(|s| s.as_str())
                .ok_or("evidence entries need source_artifact")?;
            let method = ev
                .get("method")
                .and_then(|m| m.as_str())
                .ok_or("evidence entries need method")?;
            let method = EvidenceMethod::from_str(method)
                .ok_or_else(|| format!("unknown evidence method: {}", method))?;
            let mut e = Evidence::new(source_artifact, method);
            if let Some(l) = ev.get("location").and_then(|l| l.as_str()) {
                e = e.with_location(l);
            }
            if let Some(r) = ev.get("revision").and_then(|r| r.as_str()) {
                e = e.with_revision(r);
            }
            if let Some(c) = ev.get("confidence").and_then(|c| c.as_f64()) {
                e = e.with_confidence(c as f32);
            }
            out.push(e);
        }
    }
    Ok(out)
}

pub(crate) fn parse_fusion(args: &J) -> Fusion {
    match args.get("fusion").and_then(|f| f.as_str()).unwrap_or("rrf") {
        "vector" => Fusion::VectorOnly,
        "text" => Fusion::TextOnly,
        "weighted" => {
            let wv = args.get("wv").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            let wt = args.get("wt").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            Fusion::Weighted { wv, wt }
        }
        _ => Fusion::Rrf { k0: 60 },
    }
}

pub(crate) fn parse_vector(args: &J) -> Result<Option<Vec<f32>>, String> {
    match args.get("vector") {
        Some(J::Array(xs)) => Ok(Some(
            xs.iter()
                .map(|x| x.as_f64().map(|f| f as f32).ok_or("vector must be numbers"))
                .collect::<Result<Vec<f32>, _>>()?,
        )),
        _ => Ok(None),
    }
}

pub(crate) fn ko_json(ko: &KnowledgeObject) -> J {
    let mut props = serde_json::Map::new();
    for (k, v) in &ko.properties {
        props.insert(k.clone(), value_to_json(v));
    }
    // v0.3 K1: extensions (epistemic state, history, evidence, authority,
    // scope, trust) must survive to the query boundary — dropping them here
    // is a silent epistemic metadata drop.
    let mut ext = serde_json::Map::new();
    for (k, v) in &ko.extensions {
        ext.insert(k.clone(), value_to_json(v));
    }
    json!({
        "koid": ko.koid.to_hex(),
        "version": ko.version,
        "commit_ts": ko.commit_ts,
        "type_name": ko.metadata.type_name,
        "state": ko.lifecycle.state.to_string(),
        "properties": J::Object(props),
        "extensions": J::Object(ext),
        "semantic": ko.semantic.as_ref().map(|s| json!({
            "embedding_model": s.embedding_model,
            "confidence": s.confidence,
            "source": s.source,
            "summary": s.summary,
            "embedding_dims": s.embedding.as_ref().map(|e| e.len())
        })),
        "relationships": ko.relationships.iter().map(|r| json!({
            "rel_type": r.rel_type,
            "target": r.target.to_hex(),
            "direction": if r.direction == Direction::Outbound { "outbound" } else { "inbound" }
        })).collect::<Vec<_>>(),
        "event_refs": ko.event_refs.len()
    })
}

pub(crate) fn ke_json(ke: &KnowledgeEvent) -> J {
    json!({
        "seq": ke.seq,
        "koid": ke.koid.to_hex(),
        "version": ke.version,
        "kind": format!("{:?}", ke.kind),
        "actor": ke.actor,
        "commit_ts": ke.commit_ts,
        "note": ke.note
    })
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------
