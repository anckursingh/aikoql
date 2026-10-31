//! AG-03: the two prompts. `aikoql-context` compiles the session context
//! for a task; `aikoql-review-conflicts` formats the open conflicts for
//! review. Both read fresh state at get time (a prompt is a template, the
//! message is compiled on request).

use crate::resources::open_conflicts;
use crate::session::McpSession;
use crate::{json, Kernel, J};

pub(crate) fn prompts_list() -> J {
    json!({"prompts": [
        {"name": "aikoql-context",
         "description": "Compile the session context for a task: who is acting, what surface is enabled, and the open conflicts in scope.",
         "arguments": [
             {"name": "task", "description": "The task to compile context for", "required": true},
             {"name": "subject", "description": "Calling principal (default: the session agent)"}
         ]},
        {"name": "aikoql-review-conflicts",
         "description": "The open conflicts, formatted for review — for each, propose a resolution decision with a rationale.",
         "arguments": [
             {"name": "subject", "description": "Calling principal (default: the session agent)"}
         ]}
    ]})
}

fn conflicts_block(k: &Kernel, subject: &str) -> String {
    match open_conflicts(k, subject) {
        Ok(c) if c.as_array().is_some_and(|a| !a.is_empty()) => {
            format!(
                "Open conflicts (aikoql://conflicts/open):\n{}\n",
                serde_json::to_string_pretty(&c).unwrap_or_default()
            )
        }
        Ok(_) => "Open conflicts: none.\n".to_string(),
        Err(e) => format!("Open conflicts could not be read: {e}\n"),
    }
}

pub(crate) fn prompts_get(
    k: &Kernel,
    session: &McpSession,
    params: &J,
) -> Result<J, (i64, String)> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or((-32602, "missing name".to_string()))?
        .to_string();
    let args = params.get("arguments").cloned().unwrap_or(J::Null);
    let subject = args
        .get("subject")
        .and_then(|s| s.as_str())
        .unwrap_or(&session.agent_id);
    let (description, text) = match name.as_str() {
        "aikoql-context" => {
            let task = args
                .get("task")
                .and_then(|t| t.as_str())
                .ok_or((-32602, "aikoql-context requires a task".to_string()))?;
            (
                "Session context compiled for a task".to_string(),
                format!(
                    "Task: {task}\n\nSession: agent {agent}, run {run}, roles {roles:?}, \
                     tool groups {groups:?}\n{conflicts}\
                     Use this context to plan and execute the task. Read \
                     aikoql://session/{{id}}/context or aikoql://conflicts/open \
                     again if the task turns out to need more detail.",
                    agent = session.agent_id,
                    run = session.run_id.as_deref().unwrap_or("(none — session_init)"),
                    roles = session.roles,
                    groups = session.tool_groups.iter().copied().collect::<Vec<_>>(),
                    conflicts = conflicts_block(k, subject),
                ),
            )
        }
        "aikoql-review-conflicts" => (
            "Open conflicts formatted for review".to_string(),
            format!(
                "{conflicts}\
                 For each conflict: compare the two claims, decide the resolution \
                 (resolved_a_preferred, resolved_b_preferred, resolved_both_valid, \
                 resolved_replaced) and record it with resolve_conflict — a \
                 decision without a rationale is refused. If you cannot decide, \
                 say which evidence would settle it.",
                conflicts = conflicts_block(k, subject),
            ),
        ),
        _ => return Err((-32602, format!("unknown prompt {name:?}"))),
    };
    Ok(json!({
        "description": description,
        "messages": [{"role": "user", "content": {"type": "text", "text": text}}]
    }))
}
