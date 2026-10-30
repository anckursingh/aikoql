//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::helpers::*;
use crate::session::*;
use crate::{
    json, ExtensionMap, FuseMode, IndexStatusKind, IrOp, IrPlan, KError, Kernel, Metadata,
    Ordering, Origin, PropertyMap, ReferentialPolicy, RememberRequest, SimilarityQuery, Value, J,
    STREAM_ID,
};
use std::time::Duration;
pub(crate) fn tool_aikoql(k: &Kernel, args: &J) -> Result<J, String> {
    let source = args
        .get("query")
        .and_then(|q| q.as_str())
        .ok_or("missing argument: query")?;
    let subject = args
        .get("subject")
        .and_then(|s| s.as_str())
        .unwrap_or("query-user");
    // R9: full caller identity (roles + tenant) from the session-injected args.
    let caller = subject_of(args);
    let stmt = aikoql_compiler::parser::parse(source).map_err(|e| e.to_string())?;

    // CREATE/UPDATE/DELETE are executed directly, not via IR.
    if let aikoql_compiler::parser::ast::Statement::Create(create) = &stmt {
        let mut props = PropertyMap::new();
        for (k, v) in &create.properties {
            props.insert(k.clone(), compiler_expr_to_value(v));
        }
        let r = k
            .remember(RememberRequest {
                context: caller.clone().into(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: create.entity.clone(),
                    // R9: tenant-scoped sessions create within their tenant.
                    tenant: caller.tenant.clone(),
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                security: None,
                extensions: ExtensionMap::new(),
                origin: Origin::Human,
                note: None,
                referential_policy: ReferentialPolicy::default(),
            })
            .map_err(|e| e.to_string())?;
        return Ok(
            json!({"koid": r.koid.to_hex(), "version": r.version, "commit_ts": commit_ts_millis(r.commit_ts)}),
        );
    }

    // R9: compile scoped — the Scan carries the caller's roles + tenant.
    let raw = aikoql_compiler::parser::compile_scoped(
        source,
        subject,
        &caller.roles,
        caller.tenant.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    let plan = aikoql_compiler::planner::Planner::optimize(&raw);
    let result = aikoql_runtime::Interpreter::execute(k, &plan).map_err(|e| e.to_string())?;
    match result {
        aikoql_runtime::RowSet::Objects(kos) => Ok(json!({
            "results": kos.iter().map(|ko| json!({
                "koid": ko.koid.to_hex(),
                "type_name": ko.metadata.type_name,
                "version": ko.version,
                // T-69 (POC T-67 follow-up): every row carries its tenant
                // (null for a tenantless row) so one unscoped call assembles
                // tenant-prefixed results without per-tenant scoped calls.
                "tenant": ko.metadata.tenant,
                "properties": ko.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>(),
                // v0.3 K1: epistemic metadata must survive the QL query
                // boundary too — no silent drops.
                "extensions": ko.extensions.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
            })).collect::<Vec<_>>()
        })),
        aikoql_runtime::RowSet::Scored(scored) => Ok(json!({
            "results": scored.iter().map(|(koid, score, tn, ver)| json!({
                "koid": koid.to_hex(),
                "score": score,
                "type_name": tn,
                "version": ver
            })).collect::<Vec<_>>()
        })),
        aikoql_runtime::RowSet::Traversal(hits) => Ok(json!({
            "results": hits.iter().map(|(koid, rt, depth)| json!({
                "koid": koid.to_hex(),
                "rel_type": rt,
                "depth": depth
            })).collect::<Vec<_>>()
        })),
        // T-32: group rows carry properties only (no KO identity) — the tool
        // must surface them, not empty out.
        aikoql_runtime::RowSet::Grouped(groups) => Ok(json!({
            "results": groups.iter().map(|g| json!({
                "properties": g.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
            })).collect::<Vec<_>>()
        })),
        // T-32: one row per join pair — left object plus the right match
        // (or null on an unmatched LEFT row).
        aikoql_runtime::RowSet::Joined(pairs) => Ok(json!({
            "results": pairs.iter().map(|(l, r)| json!({
                "koid": l.koid.to_hex(),
                "type_name": l.metadata.type_name,
                "version": l.version,
                "properties": l.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>(),
                "joined": r.as_ref().map(|ro| json!({
                    "koid": ro.koid.to_hex(),
                    "type_name": ro.metadata.type_name,
                    "version": ro.version,
                    "properties": ro.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
                }))
            })).collect::<Vec<_>>()
        })),
    }
}

/// Execute an aikoql query and chunk the results for streaming (MRFC-0040 #5).
/// Returns (chunks, stream_id). Chunk 0 is sent as the JSON-RPC response;
/// remaining chunks are sent as notification frames.
pub(crate) fn execute_stream_query(
    k: &Kernel,
    query: &str,
    subject: &str,
    roles: &[String],
    tenant: Option<&str>,
) -> Result<(Vec<J>, String), String> {
    // R9: compile scoped — the Scan carries the caller's roles + tenant.
    let raw = aikoql_compiler::parser::compile_scoped(query, subject, roles, tenant)
        .map_err(|e| e.to_string())?;
    let plan = aikoql_compiler::planner::Planner::optimize(&raw);
    let result = aikoql_runtime::Interpreter::execute(k, &plan).map_err(|e| e.to_string())?;

    let rows: Vec<J> = match result {
        aikoql_runtime::RowSet::Objects(kos) => kos.iter().map(|ko| json!({
            "koid": ko.koid.to_hex(),
            "type_name": ko.metadata.type_name,
            "version": ko.version,
            // T-69: the row's tenant — null for a tenantless row (see the
            // non-stream serializer).
            "tenant": ko.metadata.tenant,
            "properties": ko.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>(),
            // v0.3 K1: epistemic metadata must survive the QL query
            // boundary too — no silent drops.
            "extensions": ko.extensions.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
        })).collect(),
        aikoql_runtime::RowSet::Scored(scored) => scored.iter().map(|(koid, score, tn, ver)| json!({
            "koid": koid.to_hex(),
            "score": score,
            "type_name": tn,
            "version": ver
        })).collect(),
        aikoql_runtime::RowSet::Traversal(hits) => hits.iter().map(|(koid, rt, depth)| json!({
            "koid": koid.to_hex(),
            "rel_type": rt,
            "depth": depth
        })).collect(),
        // T-32 (DI-006 re-verification): group rows carry properties only
        // (no KO identity) — the tool must surface them, not empty out.
        aikoql_runtime::RowSet::Grouped(groups) => groups.iter().map(|g| json!({
            "properties": g.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
        })).collect(),
        // T-32: one row per join pair — left object plus the right match
        // (or null on an unmatched LEFT row).
        aikoql_runtime::RowSet::Joined(pairs) => pairs.iter().map(|(l, r)| json!({
            "koid": l.koid.to_hex(),
            "type_name": l.metadata.type_name,
            "version": l.version,
            "properties": l.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>(),
            "joined": r.as_ref().map(|ro| json!({
                "koid": ro.koid.to_hex(),
                "type_name": ro.metadata.type_name,
                "version": ro.version,
                "properties": ro.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
            }))
        })).collect(),
    };

    const CHUNK_SIZE: usize = 100;
    let chunks: Vec<J> = rows.chunks(CHUNK_SIZE).map(|c| json!(c)).collect();
    let stream_id = format!("stream-{}", STREAM_ID.fetch_add(1, Ordering::Relaxed));
    Ok((chunks, stream_id))
}

pub(crate) fn compiler_expr_to_value(e: &aikoql_compiler::parser::ast::Expr) -> Value {
    match e {
        aikoql_compiler::parser::ast::Expr::String(s) => Value::Text(s.clone()),
        aikoql_compiler::parser::ast::Expr::Number(n) => Value::Float(*n),
        aikoql_compiler::parser::ast::Expr::Bool(b) => Value::Bool(*b),
        aikoql_compiler::parser::ast::Expr::Null => Value::Null,
    }
}

/// Wait up to `ms` for the index maintainer to drain (a broken index is
/// skipped: answers still come, and lag is surfaced per hit).
pub(crate) fn wait_fresh(k: &Kernel, ms: u64) {
    if let Some(m) = k.index_maintainer() {
        let healthy = m
            .status(k)
            .map(|s| s.status != IndexStatusKind::Error)
            .unwrap_or(true);
        if healthy {
            let _ = m.wait_caught_up(k, Duration::from_millis(ms));
        }
    }
}

pub(crate) fn tool_find_similar(k: &Kernel, args: &J) -> Result<J, String> {
    // P5-M27 (IDX-P1-02): freshness is the client's choice. The default is
    // query-now — index_lag_ms is reported per hit — and an explicit
    // wait_for_freshness_ms waits here, once, ahead of both paths.
    if let Some(ms) = args.get("wait_for_freshness_ms").and_then(|v| v.as_u64()) {
        wait_fresh(k, ms);
    }
    // IR path: when type_name is explicit, compile to IR and execute via runtime.
    if let Some(type_name) = args.get("type_name").and_then(|t| t.as_str()) {
        let subject = args
            .get("subject")
            .and_then(|s| s.as_str())
            .unwrap_or("mcp-agent");
        let k_req = args.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let text = args.get("text").and_then(|t| t.as_str());
        let vector = parse_vector(args)?;
        let model = args.get("embedding_model").and_then(|t| t.as_str());

        let mut ops = vec![IrOp::Scan {
            type_name: type_name.into(),
            subject: subject.into(),
            roles: args
                .get("roles")
                .and_then(|r| r.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                // justified: absent roles param → empty role list
                .unwrap_or_default(),
            tenant: args
                .get("tenant")
                .and_then(|t| t.as_str())
                .map(String::from),
        }];
        if let Some(ref v) = vector {
            ops.push(IrOp::AnnSearch {
                vector: v.clone(),
                query_text: None,
                embedding_model: model.map(String::from),
                k: k_req,
            });
        }
        if let Some(t) = text {
            ops.push(IrOp::TextSearch {
                query: t.into(),
                k: k_req,
                scoring: None,
            });
        }
        if vector.is_some() && text.is_some() {
            let mode = match args.get("fusion").and_then(|f| f.as_str()).unwrap_or("rrf") {
                "vector" => FuseMode::VectorOnly,
                "text" => FuseMode::TextOnly,
                "weighted" => FuseMode::Weighted { wv: 0.5, wt: 0.5 },
                _ => FuseMode::Rrf { k0: 60 },
            };
            ops.push(IrOp::Fuse { mode });
        }
        let raw = IrPlan::new(ops).with_description(format!("find_similar type={}", type_name));
        let plan = aikoql_compiler::planner::Planner::optimize(&raw);
        let result = aikoql_runtime::Interpreter::execute(k, &plan).map_err(|e| e.to_string())?;
        // P5-M27 (IDX-P1-02): report the real maintainer lag, not a hardcoded 0.
        let lag_ms = k
            .index_maintainer()
            .map(|m| m.lag(k).unwrap_or(0))
            .unwrap_or(0);
        return match result {
            aikoql_runtime::RowSet::Scored(scored) => Ok(json!({
                "results": scored.iter().map(|(koid, score, tn, version)| json!({
                    "koid": koid.to_hex(),
                    "score": score,
                    "index_lag_ms": lag_ms,
                    "type_name": tn,
                    "version": version
                })).collect::<Vec<_>>()
            })),
            _ => Err("find_similar did not produce scored results".into()),
        };
    }

    // Fallback: no type_name — use kernel's find_similar for cross-type search.
    let fusion = parse_fusion(args);
    let vector = parse_vector(args)?;
    let res = k
        .find_similar(SimilarityQuery {
            context: subject_of(args).into(),
            filter: None,
            text: args.get("text").and_then(|t| t.as_str()).map(String::from),
            vector,
            embedding_model: args
                .get("embedding_model")
                .and_then(|t| t.as_str())
                .map(String::from),
            k: args.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize,
            fusion,
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "results": res.iter().map(|s| json!({
            "koid": s.ko.koid.to_hex(),
            "score": s.score,
            "index_lag_ms": s.index_lag_ms,
            "type_name": s.ko.metadata.type_name,
            "version": s.ko.version
        })).collect::<Vec<_>>()
    }))
}

/// The query's embedding, or why there is none. The model fails fast while
/// background enrichment holds it (T-44); a deliberate retrieval waits a
/// little before falling back to text-only — and says so.
pub(crate) fn embed_query(k: &Kernel, query: &str) -> (Option<Vec<f32>>, Option<String>) {
    let started = std::time::Instant::now();
    loop {
        match k.embed_text(query, None) {
            Ok(v) => return (Some(v), None),
            Err(KError::Retryable(_)) if started.elapsed() < Duration::from_millis(1500) => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return (None, Some(format!("text-only: {e}"))),
        }
    }
}

/// AG-02: retrieval as one call — hybrid search (text, plus the query's
/// embedding when a model is loaded) over current truth, each item with
/// its status, authority, a provenance one-liner and the open conflicts
/// touching it, inside a token budget.
pub(crate) fn tool_recall(k: &Kernel, args: &J) -> Result<J, String> {
    let query = args
        .get("query")
        .and_then(J::as_str)
        .filter(|q| !q.trim().is_empty())
        .ok_or("missing argument: query")?;
    let k_req = args.get("k").and_then(J::as_u64).unwrap_or(10).clamp(1, 50) as usize;
    let max_tokens = args
        .get("max_tokens")
        .and_then(J::as_u64)
        .unwrap_or(2000)
        .max(100) as usize;
    // an agent recalls right after it remembers: wait briefly by default
    wait_fresh(
        k,
        args.get("wait_for_freshness_ms")
            .and_then(J::as_u64)
            .unwrap_or(1000),
    );
    let subject = subject_of(args);
    let (vector, semantic_note) = embed_query(k, query);
    let semantic = vector.is_some();
    let hits = k
        .find_similar(SimilarityQuery {
            context: subject.clone().into(),
            filter: None,
            text: Some(query.to_string()),
            vector,
            embedding_model: None,
            // headroom for the system records and statuses filtered below
            k: k_req * 2,
            // semantic-led by default: on natural-language questions the
            // lexical leg mostly matches stopwords (6 no-shared-word
            // questions: vector 5/6 top-1, rrf 3/6, text 2/6); an explicit
            // fusion (e.g. rrf for an exact identifier) wins
            fusion: match (args.get("fusion"), semantic) {
                (Some(_), _) => parse_fusion(args),
                (None, true) => crate::Fusion::VectorOnly,
                (None, false) => crate::Fusion::TextOnly,
            },
        })
        .map_err(|e| e.to_string())?;
    // shortcut: open conflicts by a scan of the conflict records (no claim →
    // conflict index), add one when conflicts number in the thousands
    let open: Vec<(String, String, String)> = k
        .scan_by_type(&subject, "aikoql:conflict")
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(
            |c| matches!(c.extensions.get("resolution"), Some(Value::Text(r)) if r == "unresolved"),
        )
        .map(|c| {
            let side = |n: &str| match c.properties.get(n) {
                Some(Value::Text(t)) => t.clone(),
                _ => String::new(),
            };
            (c.koid.to_hex(), side("claim_a"), side("claim_b"))
        })
        .collect();
    let budget = max_tokens * 4; // ~4 bytes per token
    let (mut items, mut used, mut truncated) = (Vec::new(), 2usize, false);
    for hit in hits {
        let ko = &hit.ko;
        let status = ko.epistemic_status();
        // a zero score is filler from the fused list, not a match
        if hit.score <= 0.0
            || !crate::tools::memory::in_this_session(ko, args)
            || ko.metadata.type_name.starts_with("aikoql:")
            || matches!(status, crate::EpistemicStatus::Superseded)
        {
            continue;
        }
        if items.len() == k_req {
            truncated = true;
            break;
        }
        let koid = ko.koid.to_hex();
        let why = match (ko.derivation(), ko.evidence().first()) {
            (Some(d), _) => format!(
                "{} from {} source(s){}",
                d.operation,
                d.sources.len(),
                d.reason.map(|r| format!(": {r}")).unwrap_or_default()
            ),
            (None, Some(e)) => format!(
                "{} v{}, evidence {}",
                status.as_str(),
                ko.version,
                e.source_artifact
            ),
            (None, None) => format!("{} v{}, no recorded evidence", status.as_str(), ko.version),
        };
        let mut snippet: String = ko
            .properties
            .iter()
            .map(|(name, v)| match v {
                Value::Text(t) => format!("{name}: {t}"),
                other => format!("{name}: {}", value_to_json(other)),
            })
            .collect::<Vec<_>>()
            .join("; ");
        if snippet.chars().count() > 300 {
            snippet = snippet.chars().take(300).collect::<String>() + "…";
        }
        let item = json!({
            "koid": koid,
            "type_name": ko.metadata.type_name,
            "version": ko.version,
            "score": hit.score,
            "status": status.as_str(),
            "authority": ko.authority().map(|a| a.as_str()),
            "snippet": snippet,
            "why": why,
            "conflicts": open.iter().filter(|(_, a, b)| *a == koid || *b == koid).map(|(c, _, _)| c).collect::<Vec<_>>(),
            "related": ko.relationships.iter().take(5).map(|r| json!({"rel_type": r.rel_type, "target": r.target.to_hex()})).collect::<Vec<_>>(),
        });
        let size = item.to_string().len() + 1;
        if used + size > budget {
            truncated = true;
            break;
        }
        used += size;
        items.push(item);
    }
    let mut out =
        json!({ "query": query, "semantic": semantic, "items": items, "truncated": truncated });
    if let Some(note) = semantic_note {
        out["semantic_note"] = json!(note);
    }
    Ok(out)
}

pub(crate) fn tool_trace(k: &Kernel, args: &J) -> Result<J, String> {
    let koid = koid_of(args)?;
    let subject = subject_of(args);
    let lin = k.trace(subject.clone(), &koid).map_err(|e| e.to_string())?;
    // v0.3 K3: lineage must answer WHY / FROM WHAT / DERIVED HOW / BY WHOM /
    // WHEN / WITH WHICH EVIDENCE — a bare source pointer is insufficient
    // (reviewer H4). The derivation record + confidence context live in the
    // head's extensions; sources are resolved one level deep for readability.
    let ko = k.get(subject.clone(), &koid).map_err(|e| e.to_string())?;
    let derivation = ko.derivation().map(|d| {
        json!({
            "operation": d.operation,
            "actor": d.actor,
            "model": d.model,
            "timestamp": d.timestamp,
            "reason": d.reason,
            "sources": d.sources.iter().map(|s| {
                // Review P2-7: distinguish WHY a source cannot be resolved —
                // deleted/never-existed vs exists-but-the-caller-cannot-read.
                let (type_name, status) = match k.get(subject.clone(), s) {
                    Ok(src) => (Some(src.metadata.type_name), "ok"),
                    Err(KError::NotFound(_)) => (None, "not_found"),
                    Err(KError::AccessDenied { .. }) => (None, "not_visible"),
                    Err(_) => (None, "error"),
                };
                json!({
                    "koid": s.to_hex(),
                    "type_name": type_name,
                    "status": status,
                })
            }).collect::<Vec<_>>()
        })
    });
    Ok(json!({
        "koid": lin.koid.to_hex(),
        "versions": lin.versions.iter().map(|v| json!({
            "version": v.version,
            "commit_ts": commit_ts_millis(v.commit_ts),
            "state": v.state.to_string(),
            // POC-4 S-6: the asserted event-time start, when the version
            // carried one — the timeline's event-time column.
            "valid_from": v.valid_from
        })).collect::<Vec<_>>(),
        "events": lin.events.iter().map(ke_json).collect::<Vec<_>>(),
        "derivation": derivation,
        "confidence": ko.confidence_context().map(|c| json!({
            "score": c.score,
            "confirmations": c.confirmations,
            "last_verified": c.last_verified
        })),
        // v0.3 K4: withdrawn support — when stamped, the trace answers
        // INVALIDATED WHEN / BY WHOM / WHY.
        "invalidation": ko.invalidation().map(|i| json!({
            "at": i.at,
            "actor": i.actor,
            "reason": i.reason
        })),
        // Review P2-6: trace is an epistemic-critical read — malformed
        // evidence is an error here, not a silent drop (QL rows keep the
        // lenient decode for display).
        "evidence": ko.strict_evidence().map_err(|e| e.to_string())?
            .iter().map(|e| json!({
            "source_artifact": e.source_artifact,
            "location": e.location,
            "revision": e.revision,
            "method": e.method.as_str(),
            "confidence": e.confidence
        })).collect::<Vec<_>>()
    }))
}

/// One canonical evidence record, as explain and provenance show it.
fn evidence_record_json(e: &aikoql_kernel::knowledge::evidence::Evidence) -> J {
    json!({
        "source_artifact": e.source_artifact,
        "method": e.method.as_str(),
        "location": e.location,
        "revision": e.revision,
        "confidence": e.confidence,
    })
}

pub(crate) fn tool_explain(k: &Kernel, args: &J) -> Result<J, String> {
    let ex = k
        .explain(
            subject_of(args),
            &koid_of(args)?,
            args.get("version").and_then(|v| v.as_u64()),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": ex.koid.to_hex(),
        "version": ex.version,
        "origin": format!("{:?}", ex.origin),
        "source": ex.source,
        "confidence": ex.confidence,
        "verified": ex.verified,
        "evidence": ex.evidence.iter().map(|(t, id)| json!({"rel_type": t, "target": id.to_hex()})).collect::<Vec<_>>(),
        "evidence_records": ex.records.iter().map(evidence_record_json).collect::<Vec<_>>(),
        "event_refs": ex.event_refs.iter().map(|e| json!({"seq": e.seq, "commit_ts": commit_ts_millis(e.commit_ts)})).collect::<Vec<_>>()
    }))
}

pub(crate) fn tool_provenance(k: &Kernel, args: &J) -> Result<J, String> {
    let koid = koid_of(args)?;
    let ex = k
        .explain(subject_of(args), &koid, None)
        .map_err(|e| e.to_string())?;
    let trace = k
        .trace(subject_of(args), &koid)
        .map_err(|e| e.to_string())?;

    let mut md = format!("## Provenance for `{}`\n\n", koid.to_hex());
    md.push_str(&format!(
        "- **Version:** {}\n- **Origin:** {:?}\n- **Source:** {}\n- **Confidence:** {:.2}\n- **Verified:** {}\n\n",
        ex.version,
        ex.origin,
        ex.source.as_deref().unwrap_or("unknown"),
        ex.confidence.unwrap_or(0.0),
        ex.verified,
    ));

    if !ex.evidence.is_empty() {
        md.push_str("### Evidence Chain\n\n");
        for (i, (rel_type, target)) in ex.evidence.iter().enumerate() {
            md.push_str(&format!(
                "{}. `{}` → `{}`\n",
                i + 1,
                rel_type,
                target.to_hex()
            ));
        }
        md.push('\n');
    }

    if !ex.records.is_empty() {
        md.push_str("### Evidence Records\n\n");
        for (i, e) in ex.records.iter().enumerate() {
            let at = e
                .location
                .as_deref()
                .map(|l| format!(", {l}"))
                .unwrap_or_default();
            md.push_str(&format!(
                "{}. `{}` ({}{at}, confidence {:.2})\n",
                i + 1,
                e.source_artifact,
                e.method.as_str(),
                e.confidence
            ));
        }
        md.push('\n');
    }

    if !trace.events.is_empty() {
        md.push_str("### Audit Trail\n\n");
        for evt in &trace.events {
            md.push_str(&format!(
                "- `{:?}` @ seq={} commit_ts={}\n",
                evt.kind,
                evt.seq,
                commit_ts_millis(evt.commit_ts)
            ));
        }
    }

    Ok(json!({"koid": koid.to_hex(), "provenance": md}))
}

pub(crate) fn tool_prove(k: &Kernel, args: &J) -> Result<J, String> {
    let p = k
        .prove(subject_of(args), &koid_of(args)?)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "claim": p.claim.to_hex(),
        "events": p.events,
        "chain_valid": p.chain_valid,
        "head_audit_hash": p.head_audit_hash
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aikoql_kernel::index::{IndexMaintainerApi, TextIndex, VectorIndex};
    use aikoql_kernel::{BruteForceVectorIndex, ManualClock, MemoryEngine, TokenTextIndex};
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// A healthy-but-lagging maintainer whose wait_caught_up reproduces the
    /// barrier (the real impl polls lag for up to the bound) and records
    /// every bound the tool asks for.
    struct RecordingMaintainer {
        lag: AtomicU64,
        waits: Mutex<Vec<Duration>>,
        vectors: Arc<dyn VectorIndex>,
        text: Arc<dyn TextIndex>,
    }
    impl IndexMaintainerApi for RecordingMaintainer {
        fn lag(&self, _k: &Kernel) -> aikoql_kernel::KResult<u64> {
            Ok(self.lag.load(AtomicOrdering::Relaxed))
        }
        fn vectors(&self) -> &Arc<dyn VectorIndex> {
            &self.vectors
        }
        fn text(&self) -> &Arc<dyn TextIndex> {
            &self.text
        }
        fn wait_caught_up(&self, _k: &Kernel, timeout: Duration) -> aikoql_kernel::KResult<()> {
            // justified: Mutex poison is unrecoverable
            self.waits.lock().unwrap().push(timeout);
            std::thread::sleep(timeout);
            Ok(())
        }
    }

    /// T-32 (DI-006 re-verification): the interpreter computes GROUP BY
    /// correctly, but the tool layer dropped RowSet::Grouped to an empty
    /// result — both tool paths must surface group rows.
    #[test]
    fn aikoql_group_by_surfaces_grouped_rows() {
        let k = Kernel::open(
            Arc::new(MemoryEngine::new()),
            Arc::new(ManualClock::new(10_000)),
            0xBEEF,
        )
        .unwrap();
        for (name, dept) in [("a", "eng"), ("b", "eng"), ("c", "sales")] {
            let mut props = PropertyMap::new();
            props.insert("name".into(), Value::Text(name.into()));
            props.insert("dept".into(), Value::Text(dept.into()));
            k.remember(RememberRequest {
                context: subject_of(&json!({"subject": "alice"})).into(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: "Scratch".into(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                security: None,
                extensions: ExtensionMap::new(),
                origin: Origin::Human,
                note: None,
                referential_policy: ReferentialPolicy::default(),
            })
            .unwrap();
        }

        let q = "MATCH Scratch GROUP BY dept, COUNT(*) RETURN *";
        // Streaming tool path (MRFC-0040 #5).
        let (chunks, _sid) = execute_stream_query(&k, q, "alice", &[], None).unwrap();
        let rows: Vec<J> = chunks
            .into_iter()
            .flat_map(|c| c.as_array().cloned().unwrap_or_default())
            .collect();
        assert_eq!(rows.len(), 2, "two dept groups, not an empty result");

        // Non-stream tool path.
        let res = tool_aikoql(&k, &json!({"query": q, "subject": "alice"})).unwrap();
        assert_eq!(
            res["results"].as_array().map(|a| a.len()),
            Some(2),
            "non-stream tool surfaces groups too"
        );
    }

    /// P5-M27 (IDX-P1-02) — RED: the default find_similar blocks ~2s behind
    /// a lagging maintainer; the explicit opt-in must be the client's
    /// bound. Fails on the elapsed-time assertion with the current code.
    #[test]
    fn find_similar_default_has_no_hidden_catch_up_barrier() {
        let k = Kernel::open(
            Arc::new(MemoryEngine::new()),
            Arc::new(ManualClock::new(10_000)),
            0xBEEF,
        )
        .unwrap();
        let m = Arc::new(RecordingMaintainer {
            lag: AtomicU64::new(1), // healthy, but lagging
            waits: Mutex::new(Vec::new()),
            vectors: Arc::new(BruteForceVectorIndex::new()),
            text: Arc::new(TokenTextIndex::default()),
        });
        k.attach_indexes(m.clone());

        let t0 = Instant::now();
        let res = tool_find_similar(&k, &json!({"text": "cat"})).unwrap();
        let elapsed = t0.elapsed();
        assert!(
            elapsed < Duration::from_millis(1500),
            "the default path must not hide a 2s catch-up barrier (took {elapsed:?})"
        );
        assert!(res["results"].is_array());
        // justified: Mutex poison is unrecoverable
        assert!(
            m.waits.lock().unwrap().is_empty(),
            "no wait unless the client asked for one"
        );

        // The explicit policy: wait_for_freshness_ms is the client's bound.
        let _ =
            tool_find_similar(&k, &json!({"text": "cat", "wait_for_freshness_ms": 500})).unwrap();
        // justified: Mutex poison is unrecoverable
        let waits = m.waits.lock().unwrap().clone();
        assert_eq!(waits, vec![Duration::from_millis(500)]);
    }
}
