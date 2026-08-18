//! RBAC capability checks + rate limiting (A7 Agent Gateway).
//! Extracted from main.rs (R7 modularization). No behavior changes.

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
/// Empty allowed list = unrestricted (admin/superuser).
pub(crate) fn check_capability(roles: &[String], tool: &str) -> Result<(), (i64, String)> {
    if roles.is_empty() || roles.contains(&"admin".to_string()) {
        return Ok(()); // admin has full access
    }

    // Sensitive tools require specific roles
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
