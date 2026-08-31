//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::helpers::*;
use crate::session::*;
use crate::{
    info_span, json, AssertionRequest, ConfidenceContext, ConflictResolution,
    ConflictResolutionRequest, ContradictionRequest, ConversationMessage, DeriveRequest, Direction,
    ExperienceRequest, ForgetMode, GraphEngineApi, InvalidationRequest, Kernel, MergeRequest,
    MergeStrategy, Metadata, ObservationRequest, RelateRequest, RelationshipRef, RememberRequest,
    SplitRequest, SummarizeConversationRequest, SupersedeRequest, TraverseQuery,
    VerificationRequest, J, KOID,
};
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

/// QA2-KNOW-006: split a merged entity in two. Side A keeps its KOID and
/// everything not moved; side B is a first-class derivation of A (DERIVED_FROM
/// edge + inherited evidence/confidence). Moving a relationship whose target
/// fails to parse is an error — a silent skip would leave the edge on A.
pub(crate) fn tool_split_entity(k: &Kernel, args: &J) -> Result<J, String> {
    let reason = args
        .get("reason")
        .and_then(|x| x.as_str())
        .ok_or("missing argument: reason")?;
    let mut req = SplitRequest {
        context: subject_of(args).into(),
        subject: koid_of(args)?,
        expected_version: args.get("expected_version").and_then(|v| v.as_u64()),
        b_properties: parse_properties(args)?,
        b_relationships: vec![],
        reason: reason.to_string(),
        idempotency_key: args
            .get("idempotency_key")
            .and_then(|v| v.as_str())
            .map(String::from),
    };
    if let Some(rels) = args.get("relationships").and_then(|r| r.as_array()) {
        for rel in rels {
            let rel_type = rel
                .get("rel_type")
                .and_then(|v| v.as_str())
                .ok_or("missing argument: relationships[].rel_type")?;
            let target = rel
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("missing argument: relationships[].target")?;
            req.b_relationships.push(RelationshipRef {
                rel_type: rel_type.into(),
                target: KOID::from_hex(target).map_err(|e| e.to_string())?,
                direction: Direction::Outbound,
            });
        }
    }
    let r = k.split(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "original_koid": r.original.0.to_hex(),
        "original_version": r.original.1,
        "new_entity_koid": r.new_entity.0.to_hex(),
        "new_entity_version": r.new_entity.1,
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
    let r = match args.get("retention_ms").and_then(|v| v.as_u64()) {
        // G13: declarative retention — the kernel stamps the expiry horizon
        // from its own clock; callers cannot forge valid_to.
        Some(ms) => k.remember_retained(req, ms),
        None => k.remember(req),
    }
    .map_err(|e| e.to_string())?;
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

/// G13 §38–39: summarize a conversation into a structured summary KO.
/// The kernel extracts verbatim only — the summary cannot invent facts —
/// and every item carries speaker/message-range/timestamp provenance.
pub(crate) fn tool_summarize_conversation(k: &Kernel, args: &J) -> Result<J, String> {
    let subject = subject_of(args);
    let conversation_id = args
        .get("conversation_id")
        .and_then(|x| x.as_str())
        .ok_or("missing argument: conversation_id")?;
    let messages = args
        .get("messages")
        .and_then(|m| m.as_array())
        .ok_or("missing argument: messages")?
        .iter()
        .map(|m| {
            let speaker = m
                .get("speaker")
                .and_then(|s| s.as_str())
                .ok_or("each message needs a speaker")?;
            let ts_ms = m
                .get("ts_ms")
                .and_then(|t| t.as_u64())
                .ok_or("each message needs ts_ms")?;
            let text = m
                .get("text")
                .and_then(|t| t.as_str())
                .ok_or("each message needs text")?;
            Ok(ConversationMessage {
                speaker: speaker.to_string(),
                timestamp_ms: ts_ms,
                text: text.to_string(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let req = SummarizeConversationRequest {
        context: subject.into(),
        conversation_id: conversation_id.to_string(),
        messages,
        evidence: parse_evidence(args)?,
        note: args.get("note").and_then(|n| n.as_str()).map(String::from),
    };
    let r = k.summarize_conversation(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "type_name": "aikoql:conversation_summary",
        "message_count": args
            .get("messages")
            .and_then(|m| m.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
    }))
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

pub(crate) fn tool_derive(k: &Kernel, args: &J) -> Result<J, String> {
    // v0.3 K3: first-class derivation through the protocol — premises are
    // validated, the derivation record + DERIVED_FROM edges are stamped by
    // the kernel (anti-CRUD-cosplay: this is not a bare remember()).
    let subject = subject_of(args);
    let type_name = args
        .get("type_name")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: type_name")?;
    let mut req = DeriveRequest::new(subject.clone(), type_name);
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
    // Review P1-9: the provenance actor is the authenticated session
    // identity — args arrive session-injected (dispatcher), so `subject` IS
    // who performed the derivation. A caller-supplied "actor" arg cannot
    // spoof provenance.
    req.actor = subject.name;
    req.model = args.get("model").and_then(|m| m.as_str()).map(String::from);
    req.reason = args
        .get("reason")
        .and_then(|r| r.as_str())
        .map(String::from);
    req.evidence = parse_evidence(args)?;
    // Review P1-7: the protocol boundary validates the confidence object —
    // out-of-range scores are rejected here, not trusted to the kernel.
    if let Some(c) = args.get("confidence").and_then(|c| c.as_object()) {
        req.confidence = Some(
            ConfidenceContext::new(
                c.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0) as f32,
                c.get("confirmations").and_then(|n| n.as_u64()).unwrap_or(0) as u32,
                c.get("last_verified").and_then(|v| v.as_u64()),
            )
            .map_err(|e| e.to_string())?,
        );
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

// ---------------------------------------------------------------------------
// v0.3 K4 — knowledge transactions over MCP. Each tool is a first-class
// kernel op (anti-CRUD-cosplay): evidence is mandatory, provenance is
// stamped, conflict resolution never silently picks a side.
// ---------------------------------------------------------------------------

pub(crate) fn tool_observe(k: &Kernel, args: &J) -> Result<J, String> {
    let type_name = args
        .get("type_name")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: type_name")?;
    let mut req = ObservationRequest::new(subject_of(args), type_name);
    req.properties = parse_properties(args)?;
    req.evidence = parse_evidence(args)?;
    req.valid_from = args.get("valid_from").and_then(|v| v.as_u64());
    req.note = args.get("note").and_then(|n| n.as_str()).map(String::from);
    let r = k.observe(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "commit_ts": r.commit_ts
    }))
}

pub(crate) fn tool_assert_knowledge(k: &Kernel, args: &J) -> Result<J, String> {
    let type_name = args
        .get("type_name")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: type_name")?;
    let mut req = AssertionRequest::new(subject_of(args), type_name);
    req.properties = parse_properties(args)?;
    req.authority = args
        .get("authority")
        .and_then(|a| a.as_str())
        .map(String::from);
    req.evidence = parse_evidence(args)?;
    req.valid_from = args.get("valid_from").and_then(|v| v.as_u64());
    req.note = args.get("note").and_then(|n| n.as_str()).map(String::from);
    let r = k.assert_knowledge(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "commit_ts": r.commit_ts
    }))
}

pub(crate) fn tool_verify_knowledge(k: &Kernel, args: &J) -> Result<J, String> {
    let mut req = VerificationRequest::new(subject_of(args), koid_of(args)?);
    req.evidence = parse_evidence(args)?;
    req.confidence = args
        .get("confidence")
        .and_then(|c| c.as_f64())
        .map(|c| c as f32);
    req.note = args.get("note").and_then(|n| n.as_str()).map(String::from);
    let r = k.verify_knowledge(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "commit_ts": r.commit_ts,
        "status": r.status.as_str(),
        "confirmations": r.confirmations,
        "last_verified": r.last_verified
    }))
}

pub(crate) fn tool_contradict(k: &Kernel, args: &J) -> Result<J, String> {
    let claim_hex = args
        .get("claim")
        .and_then(|c| c.as_str())
        .ok_or("missing argument: claim")?;
    let mut req = ContradictionRequest::new(
        subject_of(args),
        KOID::from_hex(claim_hex).map_err(|e| e.to_string())?,
    );
    if let Some(t) = args.get("counter_type").and_then(|t| t.as_str()) {
        req.counter_type = t.into();
    }
    req.counter_props = parse_properties(args)?;
    req.authority = args
        .get("authority")
        .and_then(|a| a.as_str())
        .map(String::from);
    req.evidence = parse_evidence(args)?;
    req.note = args.get("note").and_then(|n| n.as_str()).map(String::from);
    let r = k.contradict(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "counter": r.counter.to_hex(),
        "conflict": r.conflict.to_hex()
    }))
}

pub(crate) fn tool_supersede(k: &Kernel, args: &J) -> Result<J, String> {
    let old_hex = args
        .get("old")
        .and_then(|o| o.as_str())
        .ok_or("missing argument: old")?;
    // An existing successor (superseded_by) supersedes without creating a new
    // generation; type_name/properties are only needed for the create path.
    let superseded_by = match args.get("superseded_by").and_then(|s| s.as_str()) {
        Some(hex) => Some(KOID::from_hex(hex).map_err(|e| e.to_string())?),
        None => None,
    };
    let type_name = match (
        superseded_by,
        args.get("type_name").and_then(|t| t.as_str()),
    ) {
        (None, Some(t)) => t,
        (None, None) => return Err("missing argument: type_name".into()),
        (Some(_), t) => t.unwrap_or("Claim"),
    };
    let mut req = SupersedeRequest::new(
        subject_of(args),
        KOID::from_hex(old_hex).map_err(|e| e.to_string())?,
        type_name,
    );
    req.superseded_by = superseded_by;
    req.properties = parse_properties(args)?;
    req.evidence = parse_evidence(args)?;
    req.reason = args
        .get("reason")
        .and_then(|r| r.as_str())
        .map(String::from);
    req.note = args.get("note").and_then(|n| n.as_str()).map(String::from);
    let r = k.supersede(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "old": r.old.to_hex(),
        "new": r.new.to_hex(),
        "invalidated_dependents": r
            .invalidated_dependents
            .iter()
            .map(|d| d.to_hex())
            .collect::<Vec<_>>(),
        // Review P1-5: the sweep outcome is surfaced — a partial sweep is
        // visible, never silent.
        "completed": r.completed,
        "failed": r
            .failed
            .iter()
            .map(|f| json!({"koid": f.koid.to_hex(), "error": f.error}))
            .collect::<Vec<_>>()
    }))
}

pub(crate) fn tool_merge(k: &Kernel, args: &J) -> Result<J, String> {
    let type_name = args
        .get("type_name")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: type_name")?;
    let sources = args
        .get("sources")
        .and_then(|s| s.as_array())
        .ok_or("missing argument: sources")?
        .iter()
        .map(|s| {
            let hex = s.as_str().ok_or("sources must be KOID hex strings")?;
            KOID::from_hex(hex).map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<KOID>, String>>()?;
    let mut req = MergeRequest::new(subject_of(args), type_name, sources);
    req.properties = match args.get("properties").and_then(|p| p.as_object()) {
        Some(_) => Some(parse_properties(args)?),
        None => None,
    };
    req.strategy = match args
        .get("strategy")
        .and_then(|s| s.as_str())
        .unwrap_or("manual")
    {
        "manual" => MergeStrategy::Manual,
        "newest_wins" => MergeStrategy::NewestWins,
        "authority_wins" => MergeStrategy::AuthorityWins,
        other => return Err(format!("invalid merge strategy: {}", other)),
    };
    req.evidence = parse_evidence(args)?;
    req.reason = args
        .get("reason")
        .and_then(|r| r.as_str())
        .map(String::from);
    req.note = args.get("note").and_then(|n| n.as_str()).map(String::from);
    let r = k.merge(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "commit_ts": r.commit_ts
    }))
}

pub(crate) fn tool_invalidate(k: &Kernel, args: &J) -> Result<J, String> {
    let mut req = InvalidationRequest::new(subject_of(args), koid_of(args)?);
    req.evidence = parse_evidence(args)?;
    req.reason = args
        .get("reason")
        .and_then(|r| r.as_str())
        .map(String::from);
    req.note = args.get("note").and_then(|n| n.as_str()).map(String::from);
    let r = k.invalidate(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "invalidated": r.invalidated.iter().map(|k| k.to_hex()).collect::<Vec<_>>(),
        // Review P1-5: the sweep outcome is surfaced — a partial sweep is
        // visible, never silent.
        "completed": r.completed,
        "failed": r
            .failed
            .iter()
            .map(|f| json!({"koid": f.koid.to_hex(), "error": f.error}))
            .collect::<Vec<_>>()
    }))
}

pub(crate) fn tool_resolve_conflict(k: &Kernel, args: &J) -> Result<J, String> {
    let decision = args
        .get("decision")
        .and_then(|d| d.as_str())
        .ok_or("missing argument: decision")?;
    let decision = ConflictResolution::from_str(decision)
        .ok_or_else(|| format!("unknown resolution decision: {}", decision))?;
    let rationale = args
        .get("rationale")
        .and_then(|r| r.as_str())
        .ok_or("missing argument: rationale")?;
    let replacement = match args.get("replacement").and_then(|r| r.as_str()) {
        Some(hex) => Some(KOID::from_hex(hex).map_err(|e| e.to_string())?),
        None => None,
    };
    // P2-2: epoch-millis validity partition for resolved_both_valid — the
    // kernel validates it against the decision (error, never ignored).
    let split_at = args.get("split_at").and_then(|s| s.as_u64());
    let out = k
        .resolve_conflict(ConflictResolutionRequest {
            context: subject_of(args).into(),
            conflict: koid_of(args)?,
            decision,
            rationale: rationale.into(),
            replacement,
            split_at,
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "conflict": out.conflict.to_hex(),
        "decision": out.decision.as_str(),
        "effects": out
            .effects
            .iter()
            .map(|(k, st)| json!({"koid": k.to_hex(), "status": st.as_str()}))
            .collect::<Vec<_>>(),
        // Review P1-5: the replacement sweep outcome is surfaced.
        "completed": out.completed,
        "failed": out
            .failed
            .iter()
            .map(|f| json!({"koid": f.koid.to_hex(), "error": f.error}))
            .collect::<Vec<_>>()
    }))
}

pub(crate) fn tool_resolve_conflict_by_authority(k: &Kernel, args: &J) -> Result<J, String> {
    let rationale = args
        .get("rationale")
        .and_then(|r| r.as_str())
        .ok_or("missing argument: rationale")?;
    let out = k
        .resolve_conflict_by_authority(subject_of(args), koid_of(args)?, rationale.into())
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "conflict": out.conflict.to_hex(),
        "decision": out.decision.as_str(),
        "effects": out
            .effects
            .iter()
            .map(|(k, st)| json!({"koid": k.to_hex(), "status": st.as_str()}))
            .collect::<Vec<_>>(),
        // Review P1-5: the replacement sweep outcome is surfaced.
        "completed": out.completed,
        "failed": out
            .failed
            .iter()
            .map(|f| json!({"koid": f.koid.to_hex(), "error": f.error}))
            .collect::<Vec<_>>()
    }))
}

pub(crate) fn tool_get(k: &Kernel, args: &J) -> Result<J, String> {
    let ko = k
        .get(subject_of(args), &koid_of(args)?)
        .map_err(|e| e.to_string())?;
    Ok(ko_json(&ko))
}

// ---------------------------------------------------------------------------
// v0.3 K5 — Agent Experience
// ---------------------------------------------------------------------------

fn string_array(args: &J, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn tool_record_experience(k: &Kernel, args: &J) -> Result<J, String> {
    let goal = args
        .get("goal")
        .and_then(|g| g.as_str())
        .ok_or("missing argument: goal")?;
    let action = args
        .get("action")
        .and_then(|a| a.as_str())
        .ok_or("missing argument: action")?;
    let outcome = args
        .get("outcome")
        .and_then(|o| o.as_str())
        .ok_or("missing argument: outcome")?;
    let subject = subject_of(args);
    let mut req = ExperienceRequest::new(subject.clone(), goal, action, outcome);
    // Review P1-9: the provenance actor is the authenticated session
    // identity (args arrive session-injected); a caller-supplied "actor"
    // arg cannot spoof who performed the run.
    req.actor = subject.name;
    req.preconditions = string_array(args, "preconditions");
    req.causal_explanation = args
        .get("causal_explanation")
        .and_then(|c| c.as_str())
        .map(String::from);
    req.lesson = args
        .get("lesson")
        .and_then(|l| l.as_str())
        .map(String::from);
    req.reuse_conditions = string_array(args, "reuse_conditions");
    req.evidence = parse_evidence(args)?;
    req.confidence = args
        .get("confidence")
        .and_then(|c| c.as_f64())
        .map(|c| c as f32);
    req.ttl_seconds = args.get("ttl_seconds").and_then(|t| t.as_u64());
    req.shared_with = string_array(args, "shared_with");
    req.note = args.get("note").and_then(|n| n.as_str()).map(String::from);
    let r = k.record_experience(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "commit_ts": r.commit_ts
    }))
}

pub(crate) fn tool_find_experiences(k: &Kernel, args: &J) -> Result<J, String> {
    let task = args
        .get("task")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: task")?;
    let limit = args
        .get("limit")
        .and_then(|l| l.as_u64())
        .unwrap_or(5)
        .min(50) as usize;
    let matches = k
        .match_experiences(subject_of(args), task, limit)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "task": task,
        "matches": matches
            .iter()
            .map(|(ko, score)| json!({
                "koid": ko.koid.to_hex(),
                "score": score,
                "actor": ko.properties.get("actor").map(value_to_json),
                "goal": ko.properties.get("goal").map(value_to_json),
                "action": ko.properties.get("action").map(value_to_json),
                "outcome": ko.properties.get("outcome").map(value_to_json),
                "lesson": ko.properties.get("lesson").map(value_to_json),
                "confidence": ko.confidence_context().map(|c| c.score)
            }))
            .collect::<Vec<_>>()
    }))
}
