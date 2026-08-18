//! Per-connection MCP session identity (MRFC-0040, R9 tenant scoping).
//! Extracted from main.rs (R7 modularization). No behavior changes.

use crate::*;

/// Per-connection MCP session identity (MRFC-0040).
#[derive(Clone, Debug)]
pub(crate) struct McpSession {
    pub(crate) agent_id: String,
    pub(crate) run_id: Option<String>,
    pub(crate) tenant: Option<String>,
    pub(crate) roles: Vec<String>,
}

impl Default for McpSession {
    fn default() -> Self {
        McpSession {
            agent_id: "mcp-agent".into(),
            run_id: None,
            tenant: None,
            roles: vec![],
        }
    }
}
pub(crate) fn tool_session_init(args: &J, session: &mut McpSession) -> Result<J, String> {
    // Store session identity for subsequent tool calls (MRFC-0040).
    session.agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("mcp-agent")
        .into();
    session.run_id = args
        .get("run_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    session.tenant = args
        .get("tenant")
        .and_then(|v| v.as_str())
        .map(String::from);
    session.roles = args
        .get("roles")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        // justified: absent roles param → empty role list
        .unwrap_or_default();
    Ok(json!({
        "session": {
            "agent_id": session.agent_id,
            "run_id": session.run_id,
            "tenant": session.tenant,
            "roles": session.roles,
        },
        "established": true,
        "note": "Session identity established. Subsequent tool calls in this connection inherit this context."
    }))
}

/// Inject session identity into args if not overridden per-call (MRFC-0040).
pub(crate) fn inject_session(args: &J, session: &McpSession) -> J {
    let mut a = args.clone();
    if a.get("subject").is_none() {
        a["subject"] = json!(session.agent_id);
    }
    if a.get("roles").is_none() && !session.roles.is_empty() {
        a["roles"] = json!(session.roles);
    }
    // R9: session tenant scope → every kernel call built from these args.
    if a.get("tenant").is_none() && session.tenant.is_some() {
        a["tenant"] = json!(session.tenant);
    }
    a
}

pub(crate) fn subject_of(args: &J) -> Subject {
    let name = args
        .get("subject")
        .and_then(|s| s.as_str())
        .unwrap_or("mcp-agent");
    let roles: Vec<String> = args
        .get("roles")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        // justified: absent roles param → empty role list
        .unwrap_or_default();
    // R9: tenant scope from the session-injected args (None = unscoped).
    let tenant = args
        .get("tenant")
        .and_then(|t| t.as_str())
        .map(String::from);
    Subject {
        name: name.into(),
        roles,
        tenant,
    }
}
