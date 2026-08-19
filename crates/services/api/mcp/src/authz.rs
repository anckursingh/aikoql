//! RBAC capability checks + rate limiting (A7 Agent Gateway).
//! Extracted from main.rs (R7 modularization). No behavior changes.

use crate::session::TrustMode;
use crate::*;
/// Simple per-agent rate limiter: max calls per minute.
/// Uses a sliding window — resets after 60s.
pub(crate) fn check_rate(
    agent_id: &str,
    roles: &[String],
    max_per_minute: u32,
) -> Result<(), (i64, String)> {
    // ponytail: unauthenticated session (no roles) = unrestricted
    if roles.is_empty() || roles.contains(&"admin".to_string()) {
        return Ok(());
    }
    let mut store = RATE_STORE.lock().unwrap(); // justified: Mutex poison is unrecoverable
    let now = Instant::now();
    let window = Duration::from_secs(60);

    let should_reset = match store.get(agent_id) {
        Some((start, _)) => now.duration_since(*start) > window,
        None => true,
    };

    if should_reset {
        store.insert(agent_id.to_string(), (now, 1));
    } else {
        let count = store.get(agent_id).map(|(_, c)| *c).unwrap_or(0);
        if count >= max_per_minute {
            return Err((
                -32002,
                format!("rate limit exceeded: {} calls/min", max_per_minute),
            ));
        }
        // justified: entry present — should_reset=false only when store.get returned Some above
        let start = store.get(agent_id).unwrap().0;
        store.insert(agent_id.to_string(), (start, count + 1));
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
        ("verify_knowledge", &["verifier"]),
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
    Ok(())
}

use std::sync::LazyLock;

pub(crate) static RATE_STORE: LazyLock<Mutex<HashMap<String, (Instant, u32)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
mod tests {
    use super::{check_capability, TrustMode};

    fn roles(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| s.to_string()).collect()
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
