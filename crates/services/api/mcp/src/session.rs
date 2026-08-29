//! Per-connection MCP session identity (MRFC-0040, R9 tenant scoping).
//! Extracted from main.rs (R7 modularization). PRR-2 added the TCP trust
//! mode + token table.

use crate::{json, HashMap, Subject, J};
/// Where a session's identity comes from (PRR-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrustMode {
    /// stdio: the OS process boundary is the trust boundary — caller-supplied
    /// identity is accepted (single-user local mode, unchanged behavior).
    Stdio,
    /// TCP: identity is server-assigned from a verified `--tcp-token`.
    Tcp,
}

/// One token → identity mapping.
#[derive(Clone, Debug)]
pub(crate) struct TcpIdentity {
    pub(crate) tenant: Option<String>,
    pub(crate) roles: Vec<String>,
}

/// Token table from repeated `--tcp-token TOKEN[:TENANT[:ROLE1,ROLE2]]` flags.
/// Every token must carry at least one role (fail-closed).
pub(crate) struct TcpAuthTable {
    by_token: HashMap<String, TcpIdentity>,
}

impl TcpAuthTable {
    pub(crate) fn parse(specs: &[String]) -> Result<Self, String> {
        let mut by_token = HashMap::new();
        for spec in specs {
            let mut parts = spec.splitn(3, ':');
            let token = parts.next().unwrap_or_default().to_string();
            if token.is_empty() {
                return Err(format!(
                    "'{spec}' has an empty token (expected TOKEN[:TENANT[:ROLE1,ROLE2]])"
                ));
            }
            let tenant = parts
                .next()
                .map(|t| {
                    if t.is_empty() {
                        None
                    } else {
                        Some(t.to_string())
                    }
                })
                .unwrap_or(None);
            let roles: Vec<String> = parts
                .next()
                .map(|r| {
                    r.split(',')
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect()
                })
                // justified: absence of a roles section → empty list
                .unwrap_or_default();
            if roles.is_empty() {
                return Err(format!(
                    "'{spec}' assigns no roles — at least one is required (TOKEN[:TENANT[:ROLE1,ROLE2]])"
                ));
            }
            if by_token
                .insert(token, TcpIdentity { tenant, roles })
                .is_some()
            {
                return Err(format!("'{spec}' repeats a token already defined"));
            }
        }
        Ok(Self { by_token })
    }

    pub(crate) fn lookup(&self, token: &str) -> Option<&TcpIdentity> {
        self.by_token.get(token)
    }
}

/// Per-connection MCP session identity (MRFC-0040).
#[derive(Clone, Debug)]
pub(crate) struct McpSession {
    pub(crate) agent_id: String,
    pub(crate) run_id: Option<String>,
    pub(crate) tenant: Option<String>,
    pub(crate) roles: Vec<String>,
    /// PRR-2: identity source — stdio trusts the caller, TCP trusts the token.
    pub(crate) trust_mode: TrustMode,
}

impl Default for McpSession {
    fn default() -> Self {
        McpSession {
            agent_id: "mcp-agent".into(),
            run_id: None,
            tenant: None,
            roles: vec![],
            trust_mode: TrustMode::Stdio,
        }
    }
}
pub(crate) fn tool_session_init(args: &J, session: &mut McpSession) -> Result<J, String> {
    if session.trust_mode == TrustMode::Tcp {
        // PRR-2: TCP identity is server-assigned from --tcp-token. On the
        // tools/call path tenant/roles arrive already forced to the session's
        // own values (harmless no-op); agent_id is never neutralized, so
        // reject it. Only run_id is per-session client input.
        if args.get("agent_id").is_some() {
            return Err(
                "TCP identity is server-assigned by --tcp-token; agent_id cannot be set per session"
                    .into(),
            );
        }
        session.run_id = args
            .get("run_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        return Ok(json!({
            "session": {
                "agent_id": session.agent_id,
                "run_id": session.run_id,
                "tenant": session.tenant,
                "roles": session.roles,
            },
            "established": true,
            "note": "TCP identity is server-assigned by --tcp-token; only run_id is per-session."
        }));
    }
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

/// PRR-2: TCP variant — session identity OVERRIDES per-call args, so a client
/// cannot smuggle roles:["admin"] or another tenant through tools/call.
pub(crate) fn inject_session_forced(args: &J, session: &McpSession) -> J {
    let mut a = args.clone();
    a["subject"] = json!(session.agent_id);
    a["roles"] = json!(session.roles);
    if let Some(t) = &session.tenant {
        a["tenant"] = json!(t);
    } else {
        // Token without tenant: strip any client-supplied tenant entirely.
        a.as_object_mut().map(|o| o.remove("tenant"));
    }
    a
}

/// Dispatch on trust mode: stdio fills-if-absent, TCP forces.
pub(crate) fn inject_for_session(args: &J, session: &McpSession) -> J {
    match session.trust_mode {
        TrustMode::Stdio => inject_session(args, session),
        TrustMode::Tcp => inject_session_forced(args, session),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn specs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tcp_token_parse_valid_specs() {
        let t =
            TcpAuthTable::parse(&specs(&["s3cret:acme:admin,viewer", "tok2::operator"])).unwrap();
        let a = t.lookup("s3cret").unwrap();
        assert_eq!(a.tenant.as_deref(), Some("acme"));
        assert_eq!(a.roles, vec!["admin".to_string(), "viewer".to_string()]);
        let b = t.lookup("tok2").unwrap();
        assert_eq!(b.tenant, None);
        assert_eq!(b.roles, vec!["operator".to_string()]);
        assert!(t.lookup("nope").is_none());
        // Empty role segments are filtered, not an error.
        assert!(TcpAuthTable::parse(&specs(&["tok:a:admin,,"])).is_ok());
    }

    #[test]
    fn tcp_token_parse_rejects_bad_specs() {
        // No roles section → fail-closed.
        assert!(TcpAuthTable::parse(&specs(&["tok:acme"])).is_err());
        // Empty roles section → fail-closed.
        assert!(TcpAuthTable::parse(&specs(&["tok:acme:"])).is_err());
        // Empty token.
        assert!(TcpAuthTable::parse(&specs(&[":acme:admin"])).is_err());
        // Duplicate token.
        assert!(TcpAuthTable::parse(&specs(&["tok:a:admin", "tok:b:viewer"])).is_err());
    }

    #[test]
    fn forced_injection_overrides_identity() {
        let mut s = McpSession {
            trust_mode: TrustMode::Tcp,
            agent_id: "tcp-agent".into(),
            tenant: Some("acme".into()),
            roles: vec!["viewer".into()],
            ..Default::default()
        };
        let args = json!({"subject": "mallory", "roles": ["admin"], "tenant": "other", "x": 1});
        let f = inject_session_forced(&args, &s);
        assert_eq!(f["subject"], "tcp-agent");
        assert_eq!(f["roles"], json!(["viewer"]));
        assert_eq!(f["tenant"], "acme");
        assert_eq!(f["x"], 1);
        // Tenant-less token strips client tenant.
        s.tenant = None;
        let f = inject_session_forced(&args, &s);
        assert!(f.get("tenant").is_none());
        // Stdio keeps fill-if-absent behavior.
        s.trust_mode = TrustMode::Stdio;
        let f = inject_for_session(&args, &s);
        assert_eq!(f["subject"], "mallory");
    }

    #[test]
    fn tcp_session_init_rejects_client_agent_id() {
        let mut s = McpSession {
            trust_mode: TrustMode::Tcp,
            ..Default::default()
        };
        // The auth gate sets this on successful token verify (see
        // handle_tcp_client); mirror that flow here.
        s.agent_id = "tcp-agent".into();
        let err = tool_session_init(&json!({"agent_id": "mallory"}), &mut s);
        assert!(err.is_err());
        // run_id-only init succeeds and keeps server identity.
        let ok = tool_session_init(&json!({"run_id": "r7"}), &mut s).unwrap();
        assert_eq!(s.agent_id, "tcp-agent".to_string());
        assert_eq!(s.run_id.as_deref(), Some("r7"));
        assert_eq!(ok["session"]["agent_id"], "tcp-agent");
    }
}
