//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::*;

use crate::helpers::*;
use crate::session::*;
pub(crate) fn tool_relate(k: &Kernel, args: &J) -> Result<J, String> {
    let from = args
        .get("from")
        .and_then(|x| x.as_str())
        .ok_or("missing argument: from")?;
    let to = args
        .get("to")
        .and_then(|x| x.as_str())
        .ok_or("missing argument: to")?;
    let rel_type = args
        .get("rel_type")
        .and_then(|x| x.as_str())
        .ok_or("missing argument: rel_type")?;
    let req = RelateRequest::new(
        subject_of(args),
        KOID::from_hex(from).map_err(|e| e.to_string())?,
        KOID::from_hex(to).map_err(|e| e.to_string())?,
        rel_type,
    );
    let r = k.relate(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "commit_ts": r.commit_ts
    }))
}

pub(crate) fn tool_traverse(k: &Kernel, args: &J) -> Result<J, String> {
    let mut q = TraverseQuery::new(subject_of(args), koid_of(args)?);
    if let Some(rt) = args.get("rel_type").and_then(|x| x.as_str()) {
        q.rel_type = Some(rt.into());
    }
    if let Some(d) = args.get("depth").and_then(|x| x.as_u64()) {
        q.depth = d as usize;
    }
    let hits = k.traverse(q).map_err(|e| e.to_string())?;
    Ok(json!({
        "hits": hits.iter().map(|h| json!({
            "koid": h.koid.to_hex(),
            "depth": h.depth,
            "rel_type": h.rel_type,
            "direction": if h.direction == Direction::Outbound { "outbound" } else { "inbound" }
        })).collect::<Vec<_>>()
    }))
}

pub(crate) fn tool_remember(k: &Kernel, args: &J) -> Result<J, String> {
    let _span = info_span!(
        "remember",
        type_name = args
            .get("type_name")
            .and_then(|t| t.as_str())
            .unwrap_or("?")
    )
    .entered();
    let subject = subject_of(args);
    let type_name = args
        .get("type_name")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: type_name")?;
    let metadata = Metadata {
        type_name: type_name.into(),
        tenant: args
            .get("tenant")
            .and_then(|t| t.as_str())
            .map(String::from),
        schema_version: args
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as u32,
        tags: args
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            // justified: absent tags param → empty tag list
            .unwrap_or_default(),
    };
    let mut req = match args.get("koid").and_then(|x| x.as_str()) {
        Some(hex) => {
            let id = KOID::from_hex(hex).map_err(|e| e.to_string())?;
            RememberRequest::update(subject, id, metadata)
        }
        None => RememberRequest::create(subject, metadata),
    };
    req.properties = parse_properties(args)?;
    req.semantic = parse_semantic(args)?;
    // v0.3 K1: extensions may be declared at the protocol boundary
    // (epistemic status, authority, scope, canonical evidence).
    req.extensions = parse_extensions(args)?;
    // Parse optional relationships array.
    if let Some(rels) = args.get("relationships").and_then(|r| r.as_array()) {
        for rel in rels {
            if let (Some(rt), Some(target_hex)) = (
                rel.get("rel_type").and_then(|v| v.as_str()),
                rel.get("target").and_then(|v| v.as_str()),
            ) {
                if let Ok(target) = KOID::from_hex(target_hex) {
                    req.relationships.push(RelationshipRef {
                        rel_type: rt.into(),
                        target,
                        direction: aikoql_kernel::knowledge::kom::Direction::Outbound,
                    });
                }
            }
        }
    }
    req.origin = parse_origin(args);
    req.note = args.get("note").and_then(|n| n.as_str()).map(String::from);
    req.expected_version = args.get("expected_version").and_then(|v| v.as_u64());
    req.idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .map(String::from);
    let embed_requested = args.get("embed").and_then(|v| v.as_bool()).unwrap_or(false);
    let r = k.remember(req).map_err(|e| e.to_string())?;
    let mut resp = json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "commit_ts": r.commit_ts
    });
    if embed_requested {
        resp["embed"] = json!({
            "requested": true,
            "status": "pending",
            "note": "SemanticEngine will enrich this KO asynchronously. Use get() to check for embeddings."
        });
    }
    Ok(resp)
}

pub(crate) fn tool_forget(k: &Kernel, args: &J) -> Result<J, String> {
    let mode = match args
        .get("mode")
        .and_then(|m| m.as_str())
        .unwrap_or("tombstone")
    {
        "tombstone" => ForgetMode::Tombstone,
        "erase" => ForgetMode::Erase,
        other => return Err(format!("invalid forget mode: {}", other)),
    };
    let f = k
        .forget(
            subject_of(args),
            &koid_of(args)?,
            mode,
            args.get("expected_version").and_then(|v| v.as_u64()),
            args.get("note").and_then(|n| n.as_str()).map(String::from),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({"koid": f.koid.to_hex(), "version": f.version, "commit_ts": f.commit_ts}))
}

pub(crate) fn tool_evolve(k: &Kernel, args: &J) -> Result<J, String> {
    let e = k
        .evolve(
            subject_of(args),
            &koid_of(args)?,
            parse_state(args)?,
            parse_origin(args),
            args.get("expected_version").and_then(|v| v.as_u64()),
            args.get("note").and_then(|n| n.as_str()).map(String::from),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": e.koid.to_hex(),
        "version": e.version,
        "commit_ts": e.commit_ts,
        "state": e.state.to_string()
    }))
}

/// v0.3 K1: epistemic status transitions over MCP — the protocol surface for
/// "how do we know this is true" (distinct from `evolve`'s lifecycle axis).
pub(crate) fn tool_transition_epistemic(k: &Kernel, args: &J) -> Result<J, String> {
    let to = args
        .get("to")
        .and_then(|x| x.as_str())
        .ok_or("missing argument: to")?;
    let to =
        EpistemicStatus::from_str(to).ok_or_else(|| format!("unknown epistemic status: {}", to))?;
    // v0.3 K2: naming the successor wires the SUPERSEDES edge + ends validity.
    let superseded_by = match args.get("superseded_by").and_then(|s| s.as_str()) {
        Some(hex) => Some(KOID::from_hex(hex).map_err(|e| e.to_string())?),
        None => None,
    };
    let r = k
        .transition_epistemic(
            subject_of(args),
            &koid_of(args)?,
            to,
            parse_origin(args),
            superseded_by,
            args.get("expected_version").and_then(|v| v.as_u64()),
            args.get("reason")
                .and_then(|r| r.as_str())
                .map(String::from),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "commit_ts": r.commit_ts,
        "from": r.from.as_str(),
        "to": r.to.as_str()
    }))
}

pub(crate) fn tool_derive(k: &Kernel, args: &J) -> Result<J, String> {
    // v0.3 K3: first-class derivation through the protocol — premises are
    // validated, the derivation record + DERIVED_FROM edges are stamped by
    // the kernel (anti-CRUD-cosplay: this is not a bare remember()).
    let subject = subject_of(args);
    let type_name = args
        .get("type_name")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: type_name")?;
    let mut req = DeriveRequest::new(subject, type_name);
    req.properties = parse_properties(args)?;
    if let Some(srcs) = args.get("sources").and_then(|s| s.as_array()) {
        for s in srcs {
            let hex = s.as_str().ok_or("sources must be KOID hex strings")?;
            req.sources
                .push(KOID::from_hex(hex).map_err(|e| e.to_string())?);
        }
    }
    req.operation = args
        .get("operation")
        .and_then(|o| o.as_str())
        .unwrap_or("derivation")
        .into();
    if let Some(actor) = args.get("actor").and_then(|a| a.as_str()) {
        req.actor = actor.into();
    }
    req.model = args.get("model").and_then(|m| m.as_str()).map(String::from);
    req.reason = args
        .get("reason")
        .and_then(|r| r.as_str())
        .map(String::from);
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
            req.evidence.push(e);
        }
    }
    if let Some(c) = args.get("confidence").and_then(|c| c.as_object()) {
        req.confidence = Some(ConfidenceContext {
            score: c.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0) as f32,
            confirmations: c.get("confirmations").and_then(|n| n.as_u64()).unwrap_or(0) as u32,
            last_verified: c.get("last_verified").and_then(|v| v.as_u64()),
        });
    }
    let r = k.derive(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "commit_ts": r.commit_ts
    }))
}

pub(crate) fn tool_verify(k: &Kernel, args: &J) -> Result<J, String> {
    k.verify(subject_of(args), &koid_of(args)?, parse_action(args)?)
        .map_err(|e| e.to_string())?;
    Ok(json!({"allowed": true}))
}

pub(crate) fn tool_get(k: &Kernel, args: &J) -> Result<J, String> {
    let ko = k
        .get(subject_of(args), &koid_of(args)?)
        .map_err(|e| e.to_string())?;
    Ok(ko_json(&ko))
}
