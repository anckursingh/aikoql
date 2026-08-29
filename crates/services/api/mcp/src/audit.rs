//! Audit logging + tool-call detail rendering (A7 Agent Gateway).
//! Extracted from main.rs (R7 modularization). No behavior changes.

use crate::{json, Write, J};
pub(crate) fn tool_detail(name: &str, args: &J) -> String {
    let s = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "memory_search" => format!("query={}", s("query")),
        "memory_store" => format!("name={}", s("name")),
        "memory_update" => format!("name={}", s("name")),
        "memory_delete" => format!("name={}", s("name")),
        "remember" => format!("koid={}", s("koid")),
        "get" | "explain" | "trace" | "forget" | "evolve" | "verify" => {
            format!("koid={}", s("koid"))
        }
        "find_similar" => format!("query={}", s("query")),
        "compile_context" => format!("task={}", s("task")),
        "document_ingest" => format!("path={}", s("path")),
        "session_init" => format!("agent={}", s("agent_id")),
        "import" => format!("source={}", s("source")),
        "restore" => format!("backup={}", s("backup")),
        _ => String::new(),
    }
}

/// Append a JSON line to the audit log.
pub(crate) fn audit_log(db_path: &str, agent_id: &str, tool: &str, outcome: &str, detail: &str) {
    let log_path = format!("{}.audit.log", db_path);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = json!({
        "ts": ts,
        "agent": agent_id,
        "tool": tool,
        "outcome": outcome,
        "detail": if detail.len() > 200 { &detail[..200] } else { detail },
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = writeln!(f, "{}", entry);
    }
}
