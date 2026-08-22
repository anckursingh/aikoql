//! REST API Layer — /api/v1/* endpoints mirroring MCP tools.

use super::*;
use crate::tools::*;

pub fn route_v1(
    method: &str,
    path: &str,
    body: &str,
    kernel: &Kernel,
    db_path: &str,
    sessions: &Mutex<HashMap<String, crate::HttpSession>>,
    token: Option<String>,
    rate_limit: &Mutex<crate::rate_limiter::RateLimiter>,
) -> (String, String, String) {
    let clean_path = path.split('?').next().unwrap_or(path);

    // PRR-4: [rate_limit] config applies to the REST surface — 60s window,
    // keyed per token (anonymous callers share one bucket).
    if let Err(max) = rate_limit
        .lock()
        .unwrap() // justified: Mutex poison is unrecoverable
        .check(token.as_deref().unwrap_or("anon"))
    {
        return json_response(
            429,
            &json!({"error": format!("rate limit exceeded (max {max} calls/min)")}),
        );
    }

    let result: Result<J, String> = route_inner(
        method,
        clean_path,
        body,
        kernel,
        db_path,
        sessions,
        token.as_deref(),
    );

    match result {
        Ok(payload) => json_response(200, &json!({"data": payload})),
        Err(msg) => {
            let code = if msg.contains("missing") || msg.contains("bad") {
                400
            } else if msg.contains("not found") || msg.contains("NotFound") {
                404
            } else if msg.contains("AccessDenied") || msg.contains("login") {
                401
            } else {
                500
            };
            json_response(code, &json!({"error": msg}))
        }
    }
}

fn route_inner(
    method: &str,
    path: &str,
    body: &str,
    k: &Kernel,
    db_path: &str,
    sessions: &Mutex<HashMap<String, crate::HttpSession>>,
    token: Option<&str>,
) -> Result<J, String> {
    let need_auth = || check_auth(token, sessions);
    let args = || {
        with_principal(
            serde_json::from_str(body).unwrap_or(J::Null),
            token,
            sessions,
        )
    };

    match (method, path) {
        ("GET", "/api/v1/openapi.json") => openapi_spec(),
        ("GET", "/api/v1/abi-version") => tool_abi_version(k),
        ("GET", "/api/v1/metrics-info") => tool_metrics(k),
        ("GET", "/api/v1/audit") => tool_audit_report(k),
        ("GET", "/api/v1/backups") => tool_list_backups(),
        ("POST", "/api/v1/discover-ontology") => tool_discover_ontology(k),

        ("GET", p) if p.starts_with("/api/v1/schema") => {
            schema_endpoint(k).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        }
        ("GET", p) if p.starts_with("/api/v1/graph") => {
            graph_api(k, path).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        }

        ("GET", "/api/v1/compliance") => {
            need_auth()?;
            tool_compliance_report(k)
        }
        ("GET", "/api/v1/list-programs") => {
            need_auth()?;
            tool_list_programs(k, &args())
        }
        ("GET", "/api/v1/list-agents") => {
            need_auth()?;
            tool_list_agents(k, &args())
        }
        ("GET", "/api/v1/list-connectors") => {
            need_auth()?;
            tool_list_connectors(k, &args())
        }
        ("GET", "/api/v1/list-views") => {
            need_auth()?;
            tool_list_views(k, &args())
        }
        ("GET", "/api/v1/list-reports") => {
            need_auth()?;
            tool_list_reports(k, &args())
        }
        ("GET", "/api/v1/list-benchmarks") => {
            need_auth()?;
            tool_list_benchmarks(k, &args())
        }
        ("GET", "/api/v1/prove") => {
            need_auth()?;
            tool_prove(k, &args())
        }
        ("GET", p) if p.starts_with("/api/v1/explain/") => {
            need_auth()?;
            let hex = p.strip_prefix("/api/v1/explain/").unwrap_or("");
            tool_explain(k, &with_principal(json!({"koid": hex}), token, sessions))
        }
        ("GET", p) if p.starts_with("/api/v1/get/") => {
            need_auth()?;
            let hex = p.strip_prefix("/api/v1/get/").unwrap_or("");
            tool_get(k, &with_principal(json!({"koid": hex}), token, sessions))
        }
        ("GET", p) if p.starts_with("/api/v1/trace/") => {
            need_auth()?;
            let hex = p.strip_prefix("/api/v1/trace/").unwrap_or("");
            tool_trace(k, &with_principal(json!({"koid": hex}), token, sessions))
        }

        ("POST", "/api/v1/remember") => {
            need_auth()?;
            tool_remember(k, &args())
        }
        ("POST", "/api/v1/forget") => {
            need_auth()?;
            tool_forget(k, &args())
        }
        ("POST", "/api/v1/evolve") => {
            need_auth()?;
            tool_evolve(k, &args())
        }
        ("POST", "/api/v1/verify") => {
            need_auth()?;
            tool_verify(k, &args())
        }
        ("POST", "/api/v1/find-similar") => {
            need_auth()?;
            tool_find_similar(k, &args())
        }
        ("POST", "/api/v1/aikoql") => {
            need_auth()?;
            tool_aikoql(k, &args())
        }
        ("POST", "/api/v1/relate") => {
            need_auth()?;
            tool_relate(k, &args())
        }
        ("POST", "/api/v1/traverse") => {
            need_auth()?;
            tool_traverse(k, &args())
        }
        ("POST", "/api/v1/explain") => {
            need_auth()?;
            tool_explain(k, &args())
        }
        ("POST", "/api/v1/prove") => {
            need_auth()?;
            tool_prove(k, &args())
        }
        ("POST", "/api/v1/deploy-program") => {
            need_auth()?;
            tool_deploy_program(k, &args())
        }
        ("POST", "/api/v1/execute-program") => {
            need_auth()?;
            tool_execute_program(k, &args())
        }
        ("POST", "/api/v1/list-programs") => {
            need_auth()?;
            tool_list_programs(k, &args())
        }
        ("POST", "/api/v1/deploy-policy") => {
            need_auth()?;
            tool_deploy_policy(k, &args())
        }
        ("POST", "/api/v1/evaluate-policies") => {
            need_auth()?;
            tool_evaluate_policies(k, &args())
        }
        ("POST", "/api/v1/deploy-workflow") => {
            need_auth()?;
            tool_deploy_workflow(k, &args())
        }
        ("POST", "/api/v1/deploy-trigger") => {
            need_auth()?;
            tool_deploy_trigger(k, &args())
        }
        ("POST", "/api/v1/add-dependency") => {
            need_auth()?;
            tool_add_dependency(k, &args())
        }
        ("POST", "/api/v1/deploy-agent") => {
            need_auth()?;
            tool_deploy_agent(k, &args())
        }
        ("POST", "/api/v1/list-agents") => {
            need_auth()?;
            tool_list_agents(k, &args())
        }
        ("POST", "/api/v1/execute-agent") => {
            need_auth()?;
            tool_execute_agent(k, &args())
        }
        ("POST", "/api/v1/deploy-connector") => {
            need_auth()?;
            tool_deploy_connector(k, &args())
        }
        ("POST", "/api/v1/list-connectors") => {
            need_auth()?;
            tool_list_connectors(k, &args())
        }
        ("POST", "/api/v1/deploy-view") => {
            need_auth()?;
            tool_deploy_view(k, &args())
        }
        ("POST", "/api/v1/list-views") => {
            need_auth()?;
            tool_list_views(k, &args())
        }
        ("POST", "/api/v1/deploy-report") => {
            need_auth()?;
            tool_deploy_report(k, &args())
        }
        ("POST", "/api/v1/list-reports") => {
            need_auth()?;
            tool_list_reports(k, &args())
        }
        ("POST", "/api/v1/deploy-benchmark") => {
            need_auth()?;
            tool_deploy_benchmark(k, &args())
        }
        ("POST", "/api/v1/list-benchmarks") => {
            need_auth()?;
            tool_list_benchmarks(k, &args())
        }
        ("POST", "/api/v1/execute-workflow") => {
            need_auth()?;
            tool_execute_workflow(k, &args())
        }
        ("POST", "/api/v1/check-triggers") => {
            need_auth()?;
            tool_check_triggers(k)
        }
        ("POST", "/api/v1/reason") => {
            need_auth()?;
            tool_reason(k, &args())
        }
        ("POST", "/api/v1/infer") => {
            need_auth()?;
            tool_infer(k, &args())
        }
        ("POST", "/api/v1/predict") => {
            need_auth()?;
            tool_predict(k, &args())
        }
        ("POST", "/api/v1/documents") => {
            need_auth()?;
            tool_document_ingest(k, &args(), db_path)
        }
        ("GET", "/api/v1/list-documents") => {
            need_auth()?;
            tool_document_list(k, &args())
        }
        ("GET", p) if p.starts_with("/api/v1/documents/") && p.ends_with("/status") => {
            need_auth()?;
            let koid_hex = p
                .strip_prefix("/api/v1/documents/")
                .unwrap_or("")
                .strip_suffix("/status")
                .unwrap_or("");
            tool_document_status(
                k,
                &with_principal(json!({"koid": koid_hex}), token, sessions),
            )
        }
        ("POST", "/api/v1/documents/compile") => {
            need_auth()?;
            tool_document_compile(k, &args(), db_path)
        }
        ("POST", "/api/v1/backup") => tool_backup(k, db_path),
        ("POST", "/api/v1/restore") => tool_restore(&args(), db_path),
        ("POST", "/api/v1/verify-backup") => tool_verify_backup(&args()),
        ("POST", "/api/v1/eval/recall") => {
            need_auth()?;
            tool_eval_recall(k, &args())
        }
        ("POST", "/api/v1/eval/staleness") => {
            need_auth()?;
            tool_eval_staleness(k, &args())
        }
        ("POST", "/api/v1/eval/contradictions") => {
            need_auth()?;
            tool_eval_contradictions(k, &args())
        }

        // Agent Knowledge Interface (MRFC-0070)
        ("POST", "/api/v1/agent/compile-context") => {
            need_auth()?;
            tool_compile_context(k, &args(), db_path)
        }
        ("POST", "/api/v1/agent/reconcile") => {
            need_auth()?;
            tool_reconcile(k, &args(), db_path)
        }
        ("POST", "/api/v1/agent/connector-bridge") => {
            need_auth()?;
            tool_connector_bridge(k, &args())
        }
        ("POST", "/api/v1/agent/filter-secrets") => {
            need_auth()?;
            tool_filter_secrets(k, &args(), db_path)
        }
        ("POST", "/api/v1/agent/explain-component") => {
            need_auth()?;
            tool_explain_component(k, &args(), db_path)
        }
        ("POST", "/api/v1/agent/explain-decision") => {
            need_auth()?;
            tool_explain_decision(k, &args(), db_path)
        }
        ("POST", "/api/v1/agent/trace-requirement") => {
            need_auth()?;
            tool_trace_requirement(k, &args(), db_path)
        }
        ("POST", "/api/v1/agent/find-conflicts") => {
            need_auth()?;
            tool_find_conflicts(k, &args(), db_path)
        }
        ("POST", "/api/v1/agent/find-stale") => {
            need_auth()?;
            tool_find_stale(k, &args(), db_path)
        }
        ("POST", "/api/v1/agent/validate-change") => {
            need_auth()?;
            tool_validate_change(k, &args(), db_path)
        }
        ("POST", "/api/v1/agent/propose-update") => {
            need_auth()?;
            tool_propose_update(k, &args(), db_path)
        }

        // Memory tools (MRFC-0070)
        ("POST", "/api/v1/agent/memory-search") => {
            need_auth()?;
            tool_memory_search(&args())
        }
        ("POST", "/api/v1/agent/memory-store") => {
            need_auth()?;
            tool_memory_store(&args())
        }
        ("POST", "/api/v1/agent/memory-update") => {
            need_auth()?;
            tool_memory_update(&args())
        }
        ("POST", "/api/v1/agent/memory-delete") => {
            need_auth()?;
            tool_memory_delete(&args())
        }

        _ => Err("Not Found".into()),
    }
}

pub fn cors_preflight() -> (String, String, String) {
    (
        "204 No Content".to_string(),
        "text/plain".to_string(),
        String::new(),
    )
}

pub fn cors_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
        (
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization",
        ),
    ]
}

fn json_response(code: u16, body: &J) -> (String, String, String) {
    let status = match code {
        200 => "200 OK",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        404 => "404 Not Found",
        429 => "429 Too Many Requests",
        _ => "500 Internal Server Error",
    };
    (
        status.to_string(),
        "application/json".to_string(),
        body.to_string(),
    )
}

fn check_auth(
    token: Option<&str>,
    sessions: &Mutex<HashMap<String, crate::HttpSession>>,
) -> Result<(), String> {
    let token = token.ok_or("login required")?;
    let guard = sessions.lock().unwrap(); // justified: Mutex poison is unrecoverable
    guard.get(token).ok_or("invalid session")?;
    Ok(())
}

/// Overwrite `subject`/`roles` in tool args with the authenticated Bearer
/// session's principal. REST tools must act as the token's identity, never a
/// body-declared subject — the body is untrusted input and could claim any
/// subject/roles otherwise (documents/compile was ACCESS_DENIED with an admin
/// token because tools defaulted to "mcp-agent" from the body).
fn with_principal(
    mut a: J,
    token: Option<&str>,
    sessions: &Mutex<HashMap<String, crate::HttpSession>>,
) -> J {
    if let Some(tok) = token {
        let sessions = sessions.lock().unwrap(); // justified: Mutex poison is unrecoverable
        if let Some(s) = sessions.get(tok) {
            let obj = match a.as_object_mut() {
                Some(o) => o,
                None => {
                    a = json!({});
                    a.as_object_mut().expect("fresh json object") // justified: literal json! object — as_object_mut cannot fail
                }
            };
            obj.insert("subject".into(), json!(s.username.clone()));
            obj.insert("roles".into(), json!(s.roles.clone()));
        }
    }
    a
}

fn openapi_spec() -> Result<J, String> {
    Ok(json!({
        "openapi": "3.0.3",
        "info": {"title": "Aikoql API", "version": "1.0.0"},
        "servers": [{"url": "/api/v1"}],
        "paths": {
            "/remember": {"post": {"summary": "Create/update KO"}},
            "/get/{koid}": {"get": {"summary": "Fetch KO by KOID"}},
            "/find-similar": {"post": {"summary": "Hybrid search"}},
            "/aikoql": {"post": {"summary": "Execute aikoql"}},
            "/relate": {"post": {"summary": "Create relationship"}},
            "/traverse": {"post": {"summary": "Walk graph"}},
            "/reason": {"post": {"summary": "Class B: reason"}},
            "/infer": {"post": {"summary": "Class B: infer"}},
            "/predict": {"post": {"summary": "Class B: predict"}},
            "/backup": {"post": {"summary": "Create backup"}},
            "/restore": {"post": {"summary": "PITR restore"}},
            "/schema": {"get": {"summary": "Schema discovery"}},
            "/graph": {"get": {"summary": "Graph data"}},
            "/openapi.json": {"get": {"summary": "This spec"}},
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_principal_token_overrides_body() {
        let sessions = Mutex::new(HashMap::new());
        sessions.lock().unwrap().insert(
            "tok".into(),
            crate::HttpSession {
                username: "admin".into(),
                roles: vec!["admin".into()],
                created: std::time::Instant::now(),
            },
        );
        // Valid token → body-declared subject/roles are overwritten.
        let a = with_principal(
            json!({"koid": "x", "subject": "hacker", "roles": ["hacker"]}),
            Some("tok"),
            &sessions,
        );
        assert_eq!(a["subject"], "admin");
        assert_eq!(a["roles"], json!(["admin"]));
        // No token → body untouched.
        let a = with_principal(json!({"subject": "hacker"}), None, &sessions);
        assert_eq!(a["subject"], "hacker");
        // Unknown token → body untouched.
        let a = with_principal(json!({"subject": "hacker"}), Some("bogus"), &sessions);
        assert_eq!(a["subject"], "hacker");
    }
}
