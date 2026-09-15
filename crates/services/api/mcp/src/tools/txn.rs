//! P5-M11 (ND-11) — the P5-M10-deferred MCP transaction tools: begin →
//! stage → commit / rollback over tools/call with a CONNECTION-SCOPED
//! handle registry (a handle opened on one connection is invisible and
//! unusable on another — the sv003 isolation principle applied to txn ids).
//!
//! The kernel contract (docs/transaction-contract.md) does the heavy
//! lifting: SNAPSHOT isolation, stage-pinned expected_version, idempotent
//! retry via the recorded outcome row. This module is protocol plumbing:
//! JSON → RememberRequest, handle registry, response shapes.

use crate::audit::{audit_log, tool_detail};
use crate::helpers::*;
use crate::protocol::ToolResult;
use crate::session::*;
use crate::{json, HashMap, Kernel, Metadata, Mutex, RememberRequest, Transaction, J};

/// Connection-scoped open transactions, keyed by client txn id.
pub(crate) type TxnRegistry = Mutex<HashMap<String, Transaction>>;

/// Dispatch a txn_* tool call and shape the response. The response carries
/// the protocol pins (txn_id/snapshot_ts/results/deduped/…) at the TOP level
/// of the result frame alongside the standard content/isError wrapper —
/// sv011 pins that shape.
pub(crate) fn txn_dispatch(
    k: &Kernel,
    name: &str,
    args: &J,
    db_path: &str,
    session: &McpSession,
    txns: &TxnRegistry,
) -> ToolResult {
    let r: Result<J, String> = match name {
        "txn_begin" => txn_begin(k, args, session, txns),
        "txn_stage" => txn_stage(k, args, session, txns),
        "txn_commit" => txn_commit(k, args, session, txns),
        "txn_rollback" => txn_rollback(k, args, session, txns),
        other => return Err((-32602, format!("unknown tool: {other}"))),
    };
    match r {
        Ok(mut out) => {
            audit_log(
                db_path,
                &session.agent_id,
                name,
                "ok",
                &tool_detail(name, args),
            );
            out["content"] = json!([{"type": "text", "text": out.to_string()}]);
            out["isError"] = json!(false);
            Ok(out)
        }
        Err(e) => {
            audit_log(db_path, &session.agent_id, name, "error", &e);
            let body = json!({
                "ok": false,
                "error": {
                    "code": "INTERNAL",
                    "message": e,
                    "retryable": false,
                    "suggestion": "An unexpected error occurred. Report this if it persists"
                }
            });
            Ok(json!({
                "content": [{"type": "text", "text": body.to_string()}],
                "isError": true
            }))
        }
    }
}

fn txn_id_of(args: &J) -> Result<String, String> {
    args.get("txn_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| "missing argument: txn_id".to_string())
}

fn txn_begin(k: &Kernel, args: &J, _session: &McpSession, txns: &TxnRegistry) -> Result<J, String> {
    let txn_id = txn_id_of(args)?;
    let mut reg = txns.lock().unwrap(); // justified: Mutex poison is unrecoverable
    if reg.contains_key(&txn_id) {
        return Err(format!(
            "transaction '{txn_id}' is already open on this connection"
        ));
    }
    // The session-injected args carry the verified principal; staged writes
    // must carry the same subject (the kernel rejects a mismatch).
    let subject = subject_of(args);
    let t = k
        .begin_transaction(subject, txn_id.clone())
        .map_err(|e| e.to_string())?;
    let snapshot_ts = t.snapshot_ts();
    reg.insert(txn_id.clone(), t);
    Ok(json!({"txn_id": txn_id, "snapshot_ts": snapshot_ts}))
}

fn txn_stage(
    _k: &Kernel,
    args: &J,
    _session: &McpSession,
    txns: &TxnRegistry,
) -> Result<J, String> {
    let txn_id = txn_id_of(args)?;
    let op = args.get("op").ok_or("missing argument: op")?;
    let mut reg = txns.lock().unwrap(); // justified: Mutex poison is unrecoverable
    let t = reg
        .get_mut(&txn_id)
        .ok_or_else(|| format!("no open transaction '{txn_id}' on this connection"))?;
    let subject = subject_of(args);
    let req = op_to_request(op, subject)?;
    t.stage(req).map_err(|e| e.to_string())?;
    Ok(json!({"staged": true, "txn_id": txn_id}))
}

fn txn_commit(
    _k: &Kernel,
    args: &J,
    _session: &McpSession,
    txns: &TxnRegistry,
) -> Result<J, String> {
    let txn_id = txn_id_of(args)?;
    let t = {
        let mut reg = txns.lock().unwrap(); // justified: Mutex poison is unrecoverable
        reg.remove(&txn_id)
            .ok_or_else(|| format!("no open transaction '{txn_id}' on this connection"))?
    };
    let (results, deduped) = t.commit().map_err(|e| e.to_string())?;
    Ok(json!({
        "txn_id": txn_id,
        "deduped": deduped,
        "results": results.iter().map(|r| json!({
            "koid": r.koid.to_hex(),
            "version": r.version,
            "commit_ts": r.commit_ts
        })).collect::<Vec<_>>()
    }))
}

fn txn_rollback(
    _k: &Kernel,
    args: &J,
    _session: &McpSession,
    txns: &TxnRegistry,
) -> Result<J, String> {
    let txn_id = txn_id_of(args)?;
    let t = {
        let mut reg = txns.lock().unwrap(); // justified: Mutex poison is unrecoverable
        reg.remove(&txn_id)
            .ok_or_else(|| format!("no open transaction '{txn_id}' on this connection"))?
    };
    t.rollback();
    Ok(json!({"rolled_back": true, "txn_id": txn_id}))
}

/// One staged op: {"action": "create", "type_name", "properties"} or
/// {"action": "update", "koid", "properties"}.
fn op_to_request(op: &J, subject: crate::Subject) -> Result<RememberRequest, String> {
    let action = op
        .get("action")
        .and_then(|a| a.as_str())
        .ok_or("op.action is required (create|update)")?;
    let type_name = op
        .get("type_name")
        .and_then(|t| t.as_str())
        .ok_or("op.type_name is required")?;
    let metadata = Metadata {
        type_name: type_name.into(),
        tenant: subject.tenant.clone(),
        schema_version: 1,
        tags: vec![],
    };
    let mut req = match action {
        "create" => RememberRequest::create(subject, metadata),
        "update" => {
            let hex = op
                .get("koid")
                .and_then(|x| x.as_str())
                .ok_or("op.koid is required for update")?;
            let koid = crate::KOID::from_hex(hex).map_err(|e| e.to_string())?;
            RememberRequest::update(subject, koid, metadata)
        }
        other => return Err(format!("op.action '{other}' unknown — use create|update")),
    };
    // Reuse the helpers' property parser on a synthetic args shape.
    let args = json!({"properties": op.get("properties").cloned().unwrap_or(J::Null)});
    req.properties = parse_properties(&args)?;
    Ok(req)
}
