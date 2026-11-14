//! RBAC capability checks (A7 Agent Gateway).
//! R9 (review round 3): the rate limiter that lived here was a duplicate of
//! rate_limiter.rs with a hardcoded 120/min — deleted. The dispatcher's
//! shared, principal-keyed limiter is the single gate (R5).

use crate::session::TrustMode;
/// AG-34 — the role a read-scoped OAuth token (`mcp:read`) carries.
pub(crate) const READ_SCOPE_ROLE: &str = "scope:read";

/// AG-34 — what an `mcp:read` token may call: an allowlist, so a tool is a
/// write until classified here (fail-closed). `aikoql` is checked further:
/// only a MATCH statement reads.
const READ_TOOLS: &[&str] = &[
    "abi_version",
    "aikoql",
    "audit_report",
    "compile_context",
    "compliance_report",
    "constraint_diagnostics",
    "document_list",
    "document_objects",
    "document_status",
    "eval_contradictions",
    "eval_recall",
    "eval_staleness",
    "evidence_pack",
    "explain",
    "explain_component",
    "explain_decision",
    "filter_secrets",
    "find_conflicts",
    "find_experiences",
    "find_similar",
    "find_stale",
    "get",
    "get_by_idem",
    "health",
    "job_status",
    "list_agents",
    "list_backups",
    "list_benchmarks",
    "list_connectors",
    "list_programs",
    "list_reports",
    "list_views",
    "memory_search",
    "metrics",
    "program_cache_stats",
    "provenance",
    "recall",
    "session_init",
    "storage_stats",
    "trace",
    "trace_requirement",
    "traverse",
];

/// AG-34 — a read-scoped token reaches read tools only; through `aikoql`,
/// only a statement that parses to MATCH.
pub(crate) fn check_scope(
    roles: &[String],
    tool: &str,
    args: &serde_json::Value,
) -> Result<(), (i64, String)> {
    let read_scoped = roles.iter().any(|r| r == READ_SCOPE_ROLE);
    // Task 6 / MEM-VIEWER: the viewer role is read-only through `aikoql`
    // too (only a MATCH reads) — in every mode: a read-only memory token
    // handed to an agent must not write, licence or not.
    let viewer = roles.iter().any(|r| r == "viewer");
    if !read_scoped && !viewer {
        return Ok(());
    }
    let denied = |what: &str| {
        if read_scoped {
            Err((
                -32001,
                format!("the token's scope is mcp:read — {what} (sign in again with mcp:write)"),
            ))
        } else {
            Err((-32001, format!("the viewer role is read-only — {what}")))
        }
    };
    if !READ_TOOLS.contains(&tool) {
        return denied(&format!("tool '{tool}' writes"));
    }
    if tool == "aikoql" {
        let q = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
        if !matches!(
            aikoql_compiler::parser::parse(q),
            Ok(aikoql_compiler::parser::ast::Statement::Match(_))
        ) {
            return denied("only a MATCH statement reads");
        }
    }
    Ok(())
}

/// Capability grants: which roles can call which tools.
/// Review P1-10: the empty-roles passthrough is trust-mode-aware — on stdio
/// the OS process boundary IS the trust boundary (single-user local mode,
/// role-less calls stay unrestricted). On TCP identity is server-assigned
/// from a verified token; a role-less TCP session is denied here AND at the
/// dispatch gate (defense in depth), never silently unrestricted.
pub(crate) fn check_capability(
    trust: TrustMode,
    roles: &[String],
    tool: &str,
) -> Result<(), (i64, String)> {
    if (trust == TrustMode::Stdio && roles.is_empty()) || roles.contains(&"admin".to_string()) {
        return Ok(()); // admin has full access
    }
    // Review P1-10: a role-less TCP session is fail-closed for EVERY tool,
    // not just the restricted ones (the dispatch gate is the primary
    // defense — this is belt+braces so no code path can silently treat an
    // unauthenticated network caller as unrestricted).
    if trust == TrustMode::Tcp && roles.is_empty() {
        return Err((-32001, "untrusted TCP session: no roles assigned".into()));
    }

    // Sensitive tools require specific roles. Epistemic state changes need
    // separation of duties (review P1-5): verifying, invalidating, and
    // resolving conflicts are distinct capabilities.
    let restricted: &[(&str, &[&str])] = &[
        ("backup", &["operator"]),
        ("restore", &["operator"]),
        ("deploy_program", &["developer"]),
        ("deploy_policy", &["developer"]),
        ("deploy_workflow", &["developer"]),
        ("deploy_agent", &["developer"]),
        ("deploy_connector", &["developer"]),
        ("execute_program", &["operator"]),
        ("deploy_benchmark", &["developer"]),
        ("audit_report", &["auditor"]),
        ("compliance_report", &["auditor"]),
        ("evidence_pack", &["auditor"]),
        ("verify_knowledge", &["verifier"]),
        ("storage_stats", &["operator"]),
        ("storage_compact", &["operator"]),
        ("storage_checkpoint", &["operator"]),
        ("invalidate", &["operator"]),
        ("resolve_conflict", &["arbiter"]),
        ("resolve_conflict_by_authority", &["arbiter"]),
    ];

    for (restricted_tool, allowed_roles) in restricted {
        if tool == *restricted_tool {
            if !allowed_roles.iter().any(|r| roles.contains(&r.to_string())) {
                return Err((
                    -32001,
                    format!(
                        "capability denied: tool '{}' requires one of {:?}",
                        tool, allowed_roles
                    ),
                ));
            }
            return Ok(());
        }
    }

    // Task 6 / MEM-VIEWER: the viewer role reaches read tools only — the
    // same READ_TOOLS allowlist an mcp:read token carries (fail-closed), in
    // every mode (community mode used to leave the role uninterpreted).
    if roles.iter().any(|r| r == "viewer") && !READ_TOOLS.contains(&tool) {
        return Err((
            -32001,
            format!("capability denied: viewer role is read-only — tool '{tool}' writes"),
        ));
    }
    Ok(())
}

/// Task 6: a REST gate refusal — the HTTP status and message ride here.
pub(crate) enum ApiError {
    Forbidden(&'static str),
}

impl ApiError {
    pub(crate) fn status(&self) -> u16 {
        match self {
            ApiError::Forbidden(_) => 403,
        }
    }
    pub(crate) fn message(&self) -> &'static str {
        match self {
            ApiError::Forbidden(m) => m,
        }
    }
}

/// Task 6: the REST write gate. `admin` always passes; `admin_only`
/// requires admin; otherwise any non-viewer passes (a viewer may read but
/// never write). MEM-VIEWER: in community mode (no user store) the viewer
/// half holds too — a viewer never writes, licence or not — while
/// admin-only routes keep their v0.4.0 community behavior.
pub(crate) fn check_rest(roles: &[String], admin_only: bool) -> Result<(), ApiError> {
    if roles.iter().any(|r| r == "admin") {
        return Ok(());
    }
    if crate::session::users().is_none() {
        return if roles.iter().any(|r| r == "viewer") {
            Err(ApiError::Forbidden("viewer role is read-only"))
        } else {
            Ok(())
        };
    }
    if admin_only {
        return Err(ApiError::Forbidden("admin role required"));
    }
    if roles.iter().any(|r| r == "viewer") {
        return Err(ApiError::Forbidden("viewer role is read-only"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{check_capability, check_scope, TrustMode};

    fn roles(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| s.to_string()).collect()
    }

    /// MEM-VIEWER follow-up: compile_context only reads (its package cache
    /// is in-process), so a viewer and an mcp:read token may call it.
    #[test]
    fn compile_context_is_a_read() {
        for r in ["viewer", super::READ_SCOPE_ROLE] {
            assert!(
                check_scope(&roles(&[r]), "compile_context", &serde_json::json!({})).is_ok(),
                "{r} must reach compile_context"
            );
        }
    }

    #[test]
    fn capability_separation_of_duties() {
        // Unauthenticated stdio / admin sessions are unrestricted (review
        // P1-10: the passthrough is stdio-local).
        for r in [vec![], roles(&["admin"])] {
            for tool in ["verify_knowledge", "invalidate", "resolve_conflict"] {
                assert!(
                    check_capability(TrustMode::Stdio, &r, tool).is_ok(),
                    "{tool} for {r:?}"
                );
            }
        }
        // TCP with no roles is fail-closed at the capability gate too
        // (the dispatch gate is the primary defense — this is belt+braces).
        for tool in [
            "verify_knowledge",
            "invalidate",
            "resolve_conflict",
            "aikoql",
        ] {
            assert!(
                check_capability(TrustMode::Tcp, &[], tool).is_err(),
                "{tool}"
            );
        }
        // A read-only analyst cannot verify, invalidate, or resolve.
        let analyst = roles(&["analyst"]);
        for tool in ["verify_knowledge", "invalidate", "resolve_conflict"] {
            assert!(
                check_capability(TrustMode::Stdio, &analyst, tool).is_err(),
                "{tool}"
            );
        }
        // Each duty requires its own role — no cross-capability grants.
        assert!(
            check_capability(TrustMode::Stdio, &roles(&["verifier"]), "verify_knowledge").is_ok()
        );
        assert!(check_capability(TrustMode::Stdio, &roles(&["verifier"]), "invalidate").is_err());
        assert!(check_capability(TrustMode::Stdio, &roles(&["operator"]), "invalidate").is_ok());
        assert!(
            check_capability(TrustMode::Stdio, &roles(&["operator"]), "resolve_conflict").is_err()
        );
        assert!(
            check_capability(TrustMode::Stdio, &roles(&["arbiter"]), "resolve_conflict").is_ok()
        );
        assert!(check_capability(
            TrustMode::Stdio,
            &roles(&["arbiter"]),
            "resolve_conflict_by_authority"
        )
        .is_ok());
        assert!(
            check_capability(TrustMode::Stdio, &roles(&["arbiter"]), "verify_knowledge").is_err()
        );
        // Non-epistemic tools are unaffected.
        assert!(check_capability(TrustMode::Stdio, &analyst, "aikoql").is_ok());
        assert!(check_capability(TrustMode::Stdio, &analyst, "remember").is_ok());
    }
}
