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
    /// MEM-AGENT: the agent this token is (`agent=NAME` in the role list);
    /// `tcp-agent` when the token names none.
    pub(crate) agent: String,
}

/// The agent a TCP token is when it names none.
pub(crate) const DEFAULT_TCP_AGENT: &str = "tcp-agent";

/// Token table from repeated `--tcp-token TOKEN[:TENANT[:ROLE1,ROLE2[,agent=NAME]]]`
/// flags. Every token must carry at least one role (fail-closed). An
/// `agent=NAME` entry names the token's agent and is not a role; it rides
/// the role list because roles may contain `:` (`scope:read`).
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
            let entries: Vec<&str> = parts
                .next()
                .map(|r| r.split(',').filter(|s| !s.is_empty()).collect())
                // justified: absence of a roles section → empty list
                .unwrap_or_default();
            let (agents, roles): (Vec<&str>, Vec<&str>) =
                entries.into_iter().partition(|e| e.starts_with("agent="));
            let roles: Vec<String> = roles.into_iter().map(String::from).collect();
            let agent = match agents.as_slice() {
                [] => DEFAULT_TCP_AGENT.to_string(),
                [one] => {
                    let name = &one["agent=".len()..];
                    let valid = !name.is_empty()
                        && name.len() <= 64
                        && name
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch));
                    if !valid {
                        return Err(format!(
                            "'{spec}' has a bad agent name (agent=NAME: 1-64 of A-Z a-z 0-9 . _ -)"
                        ));
                    }
                    name.to_string()
                }
                _ => return Err(format!("'{spec}' names more than one agent")),
            };
            if roles.is_empty() {
                return Err(format!(
                    "'{spec}' assigns no roles — at least one is required (TOKEN[:TENANT[:ROLE1,ROLE2]])"
                ));
            }
            if by_token
                .insert(
                    token,
                    TcpIdentity {
                        tenant,
                        roles,
                        agent,
                    },
                )
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
    /// AG-01: tool groups this session enabled through tools_more (core profile)
    pub(crate) tool_groups: std::collections::BTreeSet<&'static str>,
}

impl Default for McpSession {
    fn default() -> Self {
        McpSession {
            agent_id: "mcp-agent".into(),
            run_id: None,
            tenant: None,
            roles: vec![],
            trust_mode: TrustMode::Stdio,
            tool_groups: Default::default(),
        }
    }
}
pub(crate) fn tool_session_init(args: &J, session: &mut McpSession) -> Result<J, String> {
    reject_empty_tenant(args)?;
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

/// P2c (P3-009 LOW): a tenant that is present but empty is an invisible
/// namespace — rows land where no scoped reader can ever reach them.
/// Reject at the boundary: session init and the write entry points
/// (remember covers tool_batch ops, which resolve through tool_remember).
pub(crate) fn reject_empty_tenant(args: &J) -> Result<(), String> {
    if let Some(t) = args.get("tenant") {
        match t.as_str() {
            Some(s) if !s.is_empty() => {}
            _ => return Err("invalid tenant: must be a non-empty string when provided".into()),
        }
    }
    Ok(())
}

/// Fill identity into one args object and recurse into the `operations`
/// array of tool_batch, so every batch op carries the session identity
/// (F2: ops without a subject landed as mcp-agent and the submitting
/// session then hit ACCESS_DENIED on its own KOs).
fn fill_identity(a: &mut J, session: &McpSession, forced: bool) {
    if forced {
        a["subject"] = json!(session.agent_id);
        // MEM-AGENT: an agent-scoped call (agent_memory) acts as the token's
        // agent; naming another agent cannot reach that agent's memory
        if a.get("agent_id").is_some() {
            a["agent_id"] = json!(session.agent_id);
        }
        a["roles"] = json!(session.roles);
        if let Some(t) = &session.tenant {
            a["tenant"] = json!(t);
        } else {
            // Token without tenant: strip any client-supplied tenant entirely.
            a.as_object_mut().map(|o| o.remove("tenant"));
        }
    } else {
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
    }
    // AG-11: the session's run_id scopes session memories (client input in
    // both modes — it narrows what the caller sees, never widens it)
    if let Some(run) = &session.run_id {
        if a.get("run_id").is_none() {
            a["run_id"] = json!(run);
        }
    }
    if let Some(ops) = a.get_mut("operations").and_then(|o| o.as_array_mut()) {
        for op in ops {
            fill_identity(op, session, forced);
        }
    }
}

/// Inject session identity into args if not overridden per-call (MRFC-0040).
pub(crate) fn inject_session(args: &J, session: &McpSession) -> J {
    let mut a = args.clone();
    fill_identity(&mut a, session, false);
    a
}

/// PRR-2: TCP variant — session identity OVERRIDES per-call args, so a client
/// cannot smuggle roles:["admin"] or another tenant through tools/call.
pub(crate) fn inject_session_forced(args: &J, session: &McpSession) -> J {
    let mut a = args.clone();
    fill_identity(&mut a, session, true);
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

// ---------------------------------------------------------------------------
// Enterprise OIDC (Task 5): the login-flow registry + the shared user store.
// ---------------------------------------------------------------------------

use crate::users::UserStore;
use base64::Engine as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What the IdP callback must resume: the MCP client's authorize request
/// the redirect interrupted (the password form's POST equivalent).
pub(crate) struct PendingAuthorize {
    pub(crate) client_id: String,
    pub(crate) redirect_uri: String,
    pub(crate) code_challenge: String,
    /// the MCP client's state, echoed back with the code
    pub(crate) state: Option<String>,
    pub(crate) resource: String,
    pub(crate) scope: String,
}

/// One in-flight SSO login: the PKCE verifier, plus what to resume.
pub(crate) struct OidcFlow {
    pub(crate) verifier: String,
    pub(crate) resume: Option<PendingAuthorize>,
    created: Instant,
}

/// Flow states by state token — in memory, like the OAuth codes.
pub(crate) struct OidcFlows {
    flows: Mutex<HashMap<String, OidcFlow>>,
}

/// A flow state is good for ten minutes (the OAuth codes' TTL).
const FLOW_TTL: Duration = Duration::from_secs(600);

impl OidcFlows {
    /// Register a flow → its state token (fresh 256-bit randomness).
    pub(crate) fn begin(&self, verifier: String, resume: Option<PendingAuthorize>) -> String {
        let mut raw = [0u8; 32];
        // getrandom fails only without an OS entropy source — unrecoverable
        getrandom::getrandom(&mut raw).expect("OS randomness");
        let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        // justified: Mutex poison is unrecoverable
        let mut flows = self.flows.lock().unwrap();
        flows.retain(|_, f| f.created.elapsed() < FLOW_TTL);
        flows.insert(
            state.clone(),
            OidcFlow {
                verifier,
                resume,
                created: Instant::now(),
            },
        );
        state
    }

    /// Take (and invalidate) a flow by state — single use.
    pub(crate) fn take(&self, state: &str) -> Option<OidcFlow> {
        // justified: Mutex poison is unrecoverable
        self.flows
            .lock()
            .unwrap()
            .remove(state)
            .filter(|f| f.created.elapsed() < FLOW_TTL)
    }
}

/// The process-wide flow registry (one serve per process).
pub(crate) fn oidc_flows() -> &'static OidcFlows {
    static FLOWS: OnceLock<OidcFlows> = OnceLock::new();
    FLOWS.get_or_init(|| OidcFlows {
        flows: Mutex::new(HashMap::new()),
    })
}

/// The process-wide user store: the serve path opens it once (main.rs owns
/// the startup open/seed/refuse); None in community mode.
static USERS: OnceLock<Option<Arc<Mutex<UserStore>>>> = OnceLock::new();

/// Open the serving handle on the store main.rs seeded at `{db_path}/users.db`.
/// Called by serve_metrics — the shared path of every HTTP surface.
pub(crate) fn open_users(db_path: &str) -> Option<Arc<Mutex<UserStore>>> {
    if !crate::license::LICENSE_STATE
        .get()
        .is_some_and(|s| s.features.multi_user)
    {
        return None;
    }
    USERS.get_or_init(|| {
        let path = std::path::PathBuf::from(format!("{db_path}/users.db"));
        match UserStore::open(&path) {
            Ok(s) => Some(Arc::new(Mutex::new(s))),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "user store unavailable — enterprise logins degrade");
                None
            }
        }
    })
    .clone()
}

/// Publish the store handle main.rs opened and seeded at startup (final
/// review I-1): the process-wide handle every gate reads. main.rs exits on
/// a startup open/seed failure, so a published handle is always a working
/// store — and the gates hold on every serve surface, MCP over TCP with or
/// without a metrics listener. serve_metrics' later open_users is a no-op.
pub(crate) fn set_users(store: Arc<Mutex<UserStore>>) {
    USERS.get_or_init(|| Some(store));
}

/// The shared store, once the serve path has opened it.
pub(crate) fn users() -> Option<Arc<Mutex<UserStore>>> {
    USERS.get().cloned().flatten()
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
    fn tcp_token_agent_entry_is_not_a_role() {
        // MEM-AGENT: `agent=NAME` names the agent; it is never a role
        let t = TcpAuthTable::parse(&specs(&["tok:acme:editor,agent=alice"])).unwrap();
        assert_eq!(t.lookup("tok").unwrap().roles, vec!["editor".to_string()]);
        assert_eq!(t.lookup("tok").unwrap().agent, "alice");
        // a role with a colon is still one role
        let t = TcpAuthTable::parse(&specs(&["r:acme:scope:read"])).unwrap();
        assert_eq!(t.lookup("r").unwrap().roles, vec!["scope:read".to_string()]);
        assert_eq!(t.lookup("r").unwrap().agent, DEFAULT_TCP_AGENT);
        // fail-closed: an agent but no role, an empty or repeated agent, a bad name
        for bad in [
            "tok:acme:agent=alice",
            "tok:acme:editor,agent=",
            "tok:acme:editor,agent=a,agent=b",
            "tok:acme:editor,agent=a b",
        ] {
            assert!(TcpAuthTable::parse(&specs(&[bad])).is_err(), "{bad}");
        }
    }

    #[test]
    fn forced_injection_overrides_identity() {
        let mut s = McpSession {
            trust_mode: TrustMode::Tcp,
            tool_groups: Default::default(),
            agent_id: "tcp-agent".into(),
            tenant: Some("acme".into()),
            roles: vec!["viewer".into()],
            ..Default::default()
        };
        let args = json!({"subject": "mallory", "roles": ["admin"], "tenant": "other", "x": 1,
                          "agent_id": "mallory"});
        let f = inject_session_forced(&args, &s);
        assert_eq!(f["subject"], "tcp-agent");
        // MEM-AGENT: an agent-scoped call cannot name another agent
        assert_eq!(f["agent_id"], "tcp-agent");
        assert!(inject_session_forced(&json!({}), &s)
            .get("agent_id")
            .is_none());
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
            tool_groups: Default::default(),
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
