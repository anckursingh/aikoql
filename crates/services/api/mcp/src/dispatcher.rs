//! Extracted verbatim from server.rs (PRR-7). No behavior changes.

use crate::audit::*;
use crate::helpers::*;
use crate::session::*;
use crate::tools::*;
use crate::{
    error, info_span, json, warn, Arc, EventFilter, EventKind, HashSet, Kernel, Mutex, Write, J,
    KOID, PROTOCOL_VERSION,
};

use crate::protocol::*;
use crate::tool_registry::*;

pub(crate) fn handle_message(
    k: &Kernel,
    sub_ids: &mut HashSet<String>,
    stdout: &Arc<Mutex<impl Write + Send + 'static>>,
    rate_limit: &Mutex<crate::rate_limiter::RateLimiter>,
    db_path: &Arc<String>,
    session: &mut McpSession,
    msg: J,
) {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    // PRR-4 + R5 (review round 3): ONE rate limiter, shared process-wide
    // (authz.rs used to keep a second, hidden 120/min limiter — R9 deletes
    // it). The key is the principal, so parallel connections from one
    // principal share one budget; stdio (the OS user IS the principal) gets
    // one key.
    if method == "tools/call" {
        let params = msg.get("params").cloned().unwrap_or(J::Null);
        let name = params
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let key = match session.trust_mode {
            TrustMode::Tcp => format!(
                "{}:{}",
                session.agent_id,
                session.tenant.as_deref().unwrap_or("")
            ),
            TrustMode::Stdio => "_stdio".to_string(),
        };
        let mut limiter = rate_limit.lock().unwrap(); // justified: Mutex poison is unrecoverable
        if let Err(max) = limiter.check(&key) {
            drop(limiter);
            warn!(limit = %max, %key, "rate limit exceeded");
            // R5: keep the denied call on the audit trail (previously logged
            // by the tool_registry limiter).
            audit_log(
                db_path,
                &session.agent_id,
                &name,
                "denied:rate",
                &format!("rate limit exceeded (max {max} calls/min)"),
            );
            let mut out = stdout.lock().unwrap(); // justified: Mutex poison is unrecoverable
            if let Some(id) = id {
                write_frame(
                    &mut *out,
                    err_frame(
                        &id,
                        -32000,
                        &format!("rate limit exceeded (max {max} calls/min)"),
                    ),
                );
            }
            return;
        }
    }
    let mut out = stdout.lock().unwrap(); // justified: Mutex poison is unrecoverable
    match method {
        "initialize" => {
            if let Some(id) = id {
                write_frame(
                    &mut *out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": PROTOCOL_VERSION,
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "aikoql-mcp", "version": env!("CARGO_PKG_VERSION")}
                        }
                    }),
                );
            }
        }
        "ping" => {
            if let Some(id) = id {
                write_frame(&mut *out, json!({"jsonrpc":"2.0","id":id,"result":{}}));
            }
        }
        "aikoql/stream" => {
            drop(out); // release lock during query execution
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            let params = inject_for_session(&params, session); // R9/PRR-2: session identity
            let query = params.get("query").and_then(|q| q.as_str()).unwrap_or("");
            let subject = params
                .get("subject")
                .and_then(|s| s.as_str())
                .unwrap_or("stream-user");
            let caller = subject_of(&params);
            let result =
                execute_stream_query(k, query, subject, &caller.roles, caller.tenant.as_deref());
            let mut out = stdout.lock().unwrap(); // justified: Mutex poison is unrecoverable
            match result {
                Ok((chunks, stream_id)) => {
                    let total = chunks.len();
                    // Send first chunk as the JSON-RPC response.
                    if let Some(id) = id.clone() {
                        let first = if total > 0 { &chunks[0] } else { &json!([]) };
                        write_frame(
                            &mut *out,
                            json!({
                                "jsonrpc":"2.0","id":id,"result":{
                                    "stream_id": stream_id,
                                    "chunk": 0,
                                    "total_chunks": total,
                                    "results": first
                                }
                            }),
                        );
                    }
                    // Stream remaining chunks as notification frames from a background thread.
                    if total > 1 {
                        let out_arc = stdout.clone();
                        let sid = stream_id.clone();
                        let remaining: Vec<J> = chunks.into_iter().skip(1).collect();
                        std::thread::spawn(move || {
                            let n = remaining.len();
                            for (i, chunk) in remaining.into_iter().enumerate() {
                                let chunk_idx = i + 1;
                                let done = chunk_idx == n;
                                let mut w = out_arc.lock().unwrap(); // justified: Mutex poison is unrecoverable
                                write_frame(
                                    &mut *w,
                                    json!({
                                        "jsonrpc":"2.0",
                                        "method":"notifications/notify",
                                        "params": {
                                            "stream_id": sid,
                                            "chunk": chunk_idx,
                                            "done": done,
                                            "results": chunk
                                        }
                                    }),
                                );
                            }
                        });
                    }
                }
                Err(e) => {
                    if let Some(id) = id {
                        write_frame(&mut *out, err_frame(&id, -32603, &e));
                    }
                }
            }
        }
        "session/init" => {
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            // PRR-2: TCP identity is server-assigned; the method path sees raw
            // params (no forced injection), so reject any client-supplied
            // identity field outright.
            if session.trust_mode == TrustMode::Tcp
                && (params.get("tenant").is_some()
                    || params.get("roles").is_some()
                    || params.get("agent_id").is_some())
            {
                if let Some(id) = id {
                    write_frame(
                        &mut *out,
                        err_frame(&id, -32602,
                            "TCP identity is server-assigned by --tcp-token; agent_id/tenant/roles cannot be set per session"),
                    );
                }
            } else {
                let frame = match tool_session_init(&params, session) {
                    Ok(resp) => json!({"jsonrpc":"2.0","id":id.clone(),"result":resp}),
                    Err(e) => err_frame(id.as_ref().unwrap_or(&J::Null), -32602, &e),
                };
                if id.is_some() {
                    write_frame(&mut *out, frame);
                }
            }
        }
        "tools/list" => {
            if let Some(id) = id {
                write_frame(
                    &mut *out,
                    json!({"jsonrpc":"2.0","id":id,"result":tools_list()}),
                );
            }
        }
        "tools/call" => {
            drop(out); // release lock before tool execution
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            let name = params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args = params.get("arguments").cloned().unwrap_or(J::Null);
            let args = inject_for_session(&args, session);
            let span = info_span!("tool_call", tool = %name);
            let result = span.in_scope(|| call_tool(k, &name, &args, db_path.as_ref(), session));
            if result.is_err() {
                error!(tool = %name, "tool call failed");
            }
            // Notifications are streamed immediately by background threads;
            // no drain needed.
            let mut out = stdout.lock().unwrap(); // justified: Mutex poison is unrecoverable
            if let Some(id) = id {
                let frame = match result {
                    Ok(res) => json!({"jsonrpc":"2.0","id":id,"result":res}),
                    Err((code, message)) => err_frame(&id, code, &message),
                };
                write_frame(&mut *out, frame);
            }
        }
        "notifications/subscribe" => {
            drop(out); // release lock before subscription setup
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            let result = notification_subscribe(k, sub_ids, stdout, &params);
            let mut out = stdout.lock().unwrap(); // justified: Mutex poison is unrecoverable
            if let Some(id) = id {
                let frame = match result {
                    Ok(res) => json!({"jsonrpc":"2.0","id":id,"result":res}),
                    Err((code, message)) => err_frame(&id, code, &message),
                };
                write_frame(&mut *out, frame);
            }
        }
        "notifications/unsubscribe" => {
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            let result = notification_unsubscribe(k, sub_ids, &params);
            if let Some(id) = id {
                let frame = match result {
                    Ok(res) => json!({"jsonrpc":"2.0","id":id,"result":res}),
                    Err((code, message)) => err_frame(&id, code, &message),
                };
                write_frame(&mut *out, frame);
            }
        }
        "notifications/ack" => {
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            let result = notification_ack(k, &params);
            if let Some(id) = id {
                let frame = match result {
                    Ok(res) => json!({"jsonrpc":"2.0","id":id,"result":res}),
                    Err((code, message)) => err_frame(&id, code, &message),
                };
                write_frame(&mut *out, frame);
            }
        }
        m if m.starts_with("notifications/") => {}
        _ => {
            if let Some(id) = id {
                write_frame(
                    &mut *out,
                    err_frame(&id, -32601, &format!("method not found: {}", method)),
                );
            }
        }
    }
}
pub(crate) fn notification_subscribe(
    k: &Kernel,
    sub_ids: &mut HashSet<String>,
    stdout: &Arc<Mutex<impl Write + Send + 'static>>,
    params: &J,
) -> ToolResult {
    let id = params
        .get("id")
        .and_then(|x| x.as_str())
        .ok_or((-32602, "missing subscription id".to_string()))?
        .to_string();
    let filter = parse_event_filter(params)?;
    let rx = k
        .subscribe(id.clone(), filter)
        .map_err(|e| (-32603, e.to_string()))?;
    // Replay missed events before the subscription becomes live.
    let replayed = k.replay(&id).map_err(|e| (-32603, e.to_string()))?;
    {
        let mut out = stdout.lock().unwrap(); // justified: Mutex poison is unrecoverable
        for ke in &replayed {
            write_frame(
                &mut *out,
                json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/notify",
                    "params": {"id": id.clone(), "event": ke_json(ke)}
                }),
            );
        }
    }
    // Spawn a background thread that streams notifications immediately.
    let out = stdout.clone();
    let id_clone = id.clone();
    std::thread::spawn(move || {
        for ke in rx {
            let mut out = out.lock().unwrap(); // justified: Mutex poison is unrecoverable
            write_frame(
                &mut *out,
                json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/notify",
                    "params": {"id": id_clone.clone(), "event": ke_json(&ke)}
                }),
            );
        }
    });
    sub_ids.insert(id);
    Ok(json!({"subscribed": true, "replayed": replayed.len()}))
}
pub(crate) fn notification_unsubscribe(
    k: &Kernel,
    sub_ids: &mut HashSet<String>,
    params: &J,
) -> ToolResult {
    let id = params
        .get("id")
        .and_then(|x| x.as_str())
        .ok_or((-32602, "missing subscription id".to_string()))?
        .to_string();
    k.unsubscribe(&id).map_err(|e| (-32603, e.to_string()))?;
    sub_ids.remove(&id);
    Ok(json!({}))
}
pub(crate) fn notification_ack(k: &Kernel, params: &J) -> ToolResult {
    let id = params
        .get("id")
        .and_then(|x| x.as_str())
        .ok_or((-32602, "missing subscription id".to_string()))?;
    let seq = params
        .get("seq")
        .and_then(|x| x.as_u64())
        .ok_or((-32602, "missing seq".to_string()))?;
    k.ack(id, seq).map_err(|e| (-32603, e.to_string()))?;
    Ok(json!({}))
}
pub(crate) fn parse_event_filter(args: &J) -> Result<EventFilter, (i64, String)> {
    let koid = args
        .get("koid")
        .and_then(|x| x.as_str())
        .map(KOID::from_hex)
        .transpose()
        .map_err(|e| (-32602, format!("invalid koid: {}", e)))?;
    let kinds = args.get("kinds").and_then(|x| x.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|x| x.as_str())
            .filter_map(parse_event_kind)
            .collect::<Vec<_>>()
    });
    Ok(EventFilter { koid, kinds })
}
pub(crate) fn parse_event_kind(s: &str) -> Option<EventKind> {
    match s {
        "created" => Some(EventKind::Created),
        "updated" => Some(EventKind::Updated),
        "forgotten" => Some(EventKind::Forgotten),
        "lifecycle_changed" => Some(EventKind::LifecycleChanged),
        "claim_asserted" => Some(EventKind::ClaimAsserted),
        "audit" => Some(EventKind::Audit),
        _ => None,
    }
}
