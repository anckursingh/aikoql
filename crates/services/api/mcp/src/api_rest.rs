//! REST API Layer — /api/v1/* endpoints mirroring MCP tools.

use super::*;

pub fn route_v1(
    method: &str, path: &str, body: &str, kernel: &Kernel, db_path: &str,
    sessions: &Mutex<HashMap<String, crate::HttpSession>>, token: Option<String>,
) -> (String, String, String) {
    let clean_path = path.split('?').next().unwrap_or(path);

    let result: Result<J, String> = route_inner(method, clean_path, body, kernel, db_path, sessions, token.as_deref());

    match result {
        Ok(payload) => json_response(200, &json!({"data": payload})),
        Err(msg) => {
            let code = if msg.contains("missing") || msg.contains("bad") { 400 }
            else if msg.contains("not found") || msg.contains("NotFound") { 404 }
            else if msg.contains("AccessDenied") || msg.contains("login") { 401 }
            else { 500 };
            json_response(code, &json!({"error": msg}))
        }
    }
}

fn route_inner(
    method: &str, path: &str, body: &str, k: &Kernel, db_path: &str,
    sessions: &Mutex<HashMap<String, crate::HttpSession>>, token: Option<&str>,
) -> Result<J, String> {
    let need_auth = || check_auth(token, sessions);
    let args = || serde_json::from_str(body).unwrap_or(J::Null);

    match (method, path) {
        ("GET", "/api/v1/openapi.json") => openapi_spec(),
        ("GET", "/api/v1/abi-version") => tool_abi_version(k),
        ("GET", "/api/v1/metrics-info") => tool_metrics(k),
        ("GET", "/api/v1/audit") => tool_audit_report(k),
        ("GET", "/api/v1/backups") => tool_list_backups(),
        ("POST", "/api/v1/discover-ontology") => tool_discover_ontology(k),

        ("GET", p) if p.starts_with("/api/v1/schema") => schema_endpoint(k).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())),
        ("GET", p) if p.starts_with("/api/v1/graph") => graph_api(k, path).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())),

        ("GET", "/api/v1/compliance") => { need_auth()?; tool_compliance_report(k) }
        ("GET", p) if p.starts_with("/api/v1/get/") => {
            need_auth()?;
            let hex = p.strip_prefix("/api/v1/get/").unwrap_or("");
            tool_get(k, &json!({"koid": hex, "subject": "api-user"}))
        }
        ("GET", p) if p.starts_with("/api/v1/trace/") => {
            need_auth()?;
            let hex = p.strip_prefix("/api/v1/trace/").unwrap_or("");
            tool_trace(k, &json!({"koid": hex, "subject": "api-user"}))
        }

        ("POST", "/api/v1/remember") => { need_auth()?; tool_remember(k, &args()) }
        ("POST", "/api/v1/forget") => { need_auth()?; tool_forget(k, &args()) }
        ("POST", "/api/v1/evolve") => { need_auth()?; tool_evolve(k, &args()) }
        ("POST", "/api/v1/verify") => { need_auth()?; tool_verify(k, &args()) }
        ("POST", "/api/v1/find-similar") => { need_auth()?; tool_find_similar(k, &args()) }
        ("POST", "/api/v1/aikoql") => { need_auth()?; tool_aikoql(k, &args()) }
        ("POST", "/api/v1/relate") => { need_auth()?; tool_relate(k, &args()) }
        ("POST", "/api/v1/traverse") => { need_auth()?; tool_traverse(k, &args()) }
        ("POST", "/api/v1/explain") => { need_auth()?; tool_explain(k, &args()) }
        ("POST", "/api/v1/prove") => { need_auth()?; tool_prove(k, &args()) }
        ("POST", "/api/v1/deploy-program") => { need_auth()?; tool_deploy_program(k, &args()) }
        ("POST", "/api/v1/execute-program") => { need_auth()?; tool_execute_program(k, &args()) }
        ("POST", "/api/v1/list-programs") => { need_auth()?; tool_list_programs(k, &args()) }
        ("POST", "/api/v1/deploy-policy") => { need_auth()?; tool_deploy_policy(k, &args()) }
        ("POST", "/api/v1/evaluate-policies") => { need_auth()?; tool_evaluate_policies(k, &args()) }
        ("POST", "/api/v1/deploy-workflow") => { need_auth()?; tool_deploy_workflow(k, &args()) }
        ("POST", "/api/v1/deploy-trigger") => { need_auth()?; tool_deploy_trigger(k, &args()) }
        ("POST", "/api/v1/add-dependency") => { need_auth()?; tool_add_dependency(k, &args()) }
        ("POST", "/api/v1/deploy-agent") => { need_auth()?; tool_deploy_agent(k, &args()) }
        ("POST", "/api/v1/list-agents") => { need_auth()?; tool_list_agents(k, &args()) }
        ("POST", "/api/v1/deploy-connector") => { need_auth()?; tool_deploy_connector(k, &args()) }
        ("POST", "/api/v1/list-connectors") => { need_auth()?; tool_list_connectors(k, &args()) }
        ("POST", "/api/v1/execute-workflow") => { need_auth()?; tool_execute_workflow(k, &args()) }
        ("POST", "/api/v1/check-triggers") => { need_auth()?; tool_check_triggers(k) }
        ("POST", "/api/v1/reason") => { need_auth()?; tool_reason(k, &args()) }
        ("POST", "/api/v1/infer") => { need_auth()?; tool_infer(k, &args()) }
        ("POST", "/api/v1/predict") => { need_auth()?; tool_predict(k, &args()) }
        ("POST", "/api/v1/backup") => tool_backup(k, db_path),
        ("POST", "/api/v1/restore") => tool_restore(&args(), db_path),
        ("POST", "/api/v1/verify-backup") => tool_verify_backup(&args()),
        ("POST", "/api/v1/eval/recall") => { need_auth()?; tool_eval_recall(k, &args()) }
        ("POST", "/api/v1/eval/staleness") => { need_auth()?; tool_eval_staleness(k, &args()) }
        ("POST", "/api/v1/eval/contradictions") => { need_auth()?; tool_eval_contradictions(k, &args()) }

        _ => Err("Not Found".into()),
    }
}

pub fn cors_preflight() -> (String, String, String) {
    ("204 No Content".to_string(), "text/plain".to_string(), String::new())
}

pub fn cors_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
        ("Access-Control-Allow-Headers", "Content-Type, Authorization"),
    ]
}

fn json_response(code: u16, body: &J) -> (String, String, String) {
    let status = match code {
        200 => "200 OK", 400 => "400 Bad Request", 401 => "401 Unauthorized",
        404 => "404 Not Found", _ => "500 Internal Server Error",
    };
    (status.to_string(), "application/json".to_string(), body.to_string())
}

fn check_auth(token: Option<&str>, sessions: &Mutex<HashMap<String, crate::HttpSession>>) -> Result<(), String> {
    let token = token.ok_or("login required")?;
    let guard = sessions.lock().unwrap();
    guard.get(token).ok_or("invalid session")?;
    Ok(())
}

fn openapi_spec() -> Result<J, String> {
    Ok(json!({
        "openapi": "3.0.3",
        "info": {"title": "Mnemosyne API", "version": "1.0.0"},
        "servers": [{"url": "/api/v1"}],
        "paths": {
            "/remember": {"post": {"summary": "Create/update KO"}},
            "/get/{koid}": {"get": {"summary": "Fetch KO by KOID"}},
            "/find-similar": {"post": {"summary": "Hybrid search"}},
            "/aikoql": {"post": {"summary": "Execute AIKOQL"}},
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
