//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::*;

use crate::helpers::*;
use crate::session::*;
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
            json!({"koid": r.koid.to_hex(), "version": r.version, "commit_ts": r.commit_ts}),
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
                "properties": ko.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
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
        _ => Ok(json!({"results": []})),
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
            "properties": ko.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
        })).collect(),
        aikoql_runtime::RowSet::Scored(scored) => scored.iter().map(|(koid, score, tn, ver)| json!({
            "koid": koid.to_hex(),
            "score": score,
            "type_name": tn,
            "version": ver
        })).collect(),
        _ => vec![],
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

pub(crate) fn tool_find_similar(k: &Kernel, args: &J) -> Result<J, String> {
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
        return match result {
            aikoql_runtime::RowSet::Scored(scored) => Ok(json!({
                "results": scored.iter().map(|(koid, score, tn, version)| json!({
                    "koid": koid.to_hex(),
                    "score": score,
                    "index_lag_ms": 0,
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

pub(crate) fn tool_trace(k: &Kernel, args: &J) -> Result<J, String> {
    let lin = k
        .trace(subject_of(args), &koid_of(args)?)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": lin.koid.to_hex(),
        "versions": lin.versions.iter().map(|v| json!({
            "version": v.version,
            "commit_ts": v.commit_ts,
            "state": v.state.to_string()
        })).collect::<Vec<_>>(),
        "events": lin.events.iter().map(ke_json).collect::<Vec<_>>()
    }))
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
        "event_refs": ex.event_refs.iter().map(|e| json!({"seq": e.seq, "commit_ts": e.commit_ts})).collect::<Vec<_>>()
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

    if !trace.events.is_empty() {
        md.push_str("### Audit Trail\n\n");
        for evt in &trace.events {
            md.push_str(&format!(
                "- `{:?}` @ seq={} commit_ts={}\n",
                evt.kind, evt.seq, evt.commit_ts
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
