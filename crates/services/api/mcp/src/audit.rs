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
        "get_by_idem" => format!("key={}", s("key")),
        "find_similar" => format!("query={}", s("query")),
        "compile_context" => format!("task={}", s("task")),
        "document_ingest" => format!("path={}", s("path")),
        "session_init" => format!("agent={}", s("agent_id")),
        "import" => format!("source={}", s("source")),
        "restore" => format!("backup={}", s("backup")),
        _ => String::new(),
    }
}

/// Open-append-close per call costs ~300ms on Windows once the log grows
/// (AV rescans the file on every open) — dogfood-measured at 15MB/146K
/// lines. Keep one handle per process instead; the path is fixed per
/// server instance (one db dir), reopened only if it ever changes.
static AUDIT: std::sync::OnceLock<std::sync::Mutex<Option<(String, std::fs::File)>>> =
    std::sync::OnceLock::new();

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
    let mut guard = AUDIT
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap(); // justified: Mutex poison is unrecoverable
    if guard.as_ref().map(|(p, _)| p != &log_path).unwrap_or(true) {
        *guard = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok()
            .map(|f| (log_path.clone(), f));
    }
    if let Some((_, f)) = guard.as_mut() {
        let _ = writeln!(f, "{}", entry);
    }
}
