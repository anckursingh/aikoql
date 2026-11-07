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
    admin: Option<&dyn aikoql_storage_v2::engine::StorageAdminApi>,
) -> (String, String, String) {
    let clean_path = path.split('?').next().unwrap_or(path);

    // S-4 (POC-4): GET routes carry tool args in the query string (?koid=…).
    // Fold them into a JSON body so every tool reads one args shape — the
    // studio prove call is the first caller. The auth `token` pair stays out
    // of tool args (it is consumed by extract_token).
    let folded_body = if method == "GET" {
        let mut map = serde_json::Map::new();
        if let Some(qs) = path.split_once('?').map(|(_, q)| q) {
            for pair in qs.split('&') {
                if let Some((key, val)) = pair.split_once('=') {
                    if key != "token" {
                        map.insert(key.to_string(), json!(crate::http::url_decode(val)));
                    }
                }
            }
        }
        serde_json::to_string(&json!(map)).unwrap_or_default()
    } else {
        body.to_string()
    };

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

    // Enterprise (Task 6): REST writes go through the role gate — a viewer
    // session may read but never write. The gate resolves the session's
    // LIVE roles (the user store is authoritative); in community mode it
    // passes, so v0.4.0 behavior is unchanged. Only the write set is
    // gated: read POSTs (aikoql MATCH, find-similar, traverse, list-*
    // twins, ...) stay reachable for viewers.
    if method == "POST" && (is_write_route(clean_path, &folded_body) || is_admin_route(clean_path))
    {
        if let Some(roles) = crate::http::live_roles(token.as_deref(), sessions) {
            if let Err(e) = crate::authz::check_rest(&roles, is_admin_route(clean_path)) {
                return json_response(e.status(), &json!({"error": e.message()}));
            }
        }
    }

    let result: Result<J, String> = route_inner(
        method,
        clean_path,
        &folded_body,
        kernel,
        db_path,
        sessions,
        token.as_deref(),
        admin,
    );

    match result {
        Ok(payload) => json_response(200, &json!({"data": payload})),
        Err(msg) => {
            // 409: a CAS on expected_version lost the race, or a document's
            // batches are still running / were cut short by a restart
            let code = if msg.contains("VERSION_CONFLICT")
                || msg.starts_with(crate::tools::ingestion::STILL_PROCESSING)
                || msg.starts_with(crate::tools::ingestion::INTERRUPTED)
            {
                409
            } else if msg.contains("missing")
                || msg.contains("bad")
                || msg.contains("unsupported mime type")
                || aikoql_ingestion::is_limit_error(&msg)
                || msg.starts_with("invalid ontology")
                || msg.starts_with("expected_version required")
            {
                400
            } else if msg.contains("not found") || msg.contains("NotFound") {
                404
            } else if msg.contains("AccessDenied")
                || msg.contains("login")
                || msg == "invalid session"
            {
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
    admin: Option<&dyn aikoql_storage_v2::engine::StorageAdminApi>,
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
        // P3-M1 (auth005): the allowlist — spec, ABI, metrics-info. health,
        // metrics, and login live in the root-level handler.
        ("GET", "/api/v1/openapi.json") => openapi_spec(),
        ("GET", "/api/v1/abi-version") => tool_abi_version(k),
        ("GET", "/api/v1/metrics-info") => tool_metrics(k),
        ("GET", "/api/v1/audit") => {
            need_auth()?;
            tool_audit_report(k)
        }
        ("GET", "/api/v1/backups") => {
            need_auth()?;
            tool_list_backups(db_path)
        }
        ("GET", "/api/v1/ontology") => {
            need_auth()?;
            tool_get_ontology(k, &args())
        }
        ("POST", "/api/v1/ontology") => {
            need_auth()?;
            tool_save_ontology(k, &args())
        }
        ("POST", "/api/v1/discover-ontology") => {
            need_auth()?;
            tool_discover_ontology(k, &args())
        }

        ("GET", p) if p.starts_with("/api/v1/schema") => {
            need_auth()?;
            schema_endpoint(k).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        }
        ("GET", p) if p.starts_with("/api/v1/graph") => {
            need_auth()?;
            // P3-M1 (auth006): graph executes as the session principal.
            let subj = crate::http::validate_token(token, sessions).ok_or("login required")?;
            graph_api(k, path, &subj)
                .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        }

        ("GET", "/api/v1/compliance") => {
            need_auth()?;
            tool_compliance_report(k)
        }
        ("GET", p) if p.starts_with("/api/v1/evidence/") => {
            need_auth()?;
            tool_evidence_pack(
                k,
                &json!({"framework": p.trim_start_matches("/api/v1/evidence/")}),
            )
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
            let args = args();
            let r = tool_remember(k, &args)?;
            // S-8 (POC-4): the studio's manual Save Ontology flow — a
            // remembered Ontology KO republishes the compile-time registry.
            if args.get("type_name").and_then(|t| t.as_str()) == Some("Ontology") {
                crate::http::refresh_ontologies(k);
            }
            Ok(r)
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
        ("POST", "/api/v1/job-status") => {
            need_auth()?;
            tool_job_status(k, &args())
        }
        ("POST", "/api/v1/approve-job") => {
            need_auth()?;
            tool_approve_job(k, &args())
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
                db_path,
            )
        }
        ("POST", "/api/v1/documents/compile") => {
            need_auth()?;
            tool_document_compile(k, &args(), db_path)
        }
        ("POST", "/api/v1/backup") => {
            need_auth()?;
            tool_backup(k, db_path, admin)
        }
        ("POST", "/api/v1/restore") => {
            need_auth()?;
            tool_restore(&args(), admin, db_path)
        }
        ("POST", "/api/v1/verify-backup") => {
            need_auth()?;
            tool_verify_backup(&args(), db_path)
        }
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
            tool_memory_search(k, &args())
        }
        ("POST", "/api/v1/agent/memory-store") => {
            need_auth()?;
            tool_memory_store(k, &args())
        }
        ("POST", "/api/v1/agent/memory-update") => {
            need_auth()?;
            tool_memory_update(k, &args())
        }
        ("POST", "/api/v1/agent/memory-delete") => {
            need_auth()?;
            tool_memory_delete(k, &args())
        }

        _ => Err("Not Found".into()),
    }
}

// ---------------------------------------------------------------------------
// Enterprise (Task 8): the admin API — /api/license plus the user routes.
// Root-level paths (outside /api/v1, which stays the untouched v0.4.0
// surface) and outside route_v1's rate limiter, like the rest of the
// root surface. The T6 admin-only gate applies directly here: these routes
// are never in route_v1's POST-only gate.
// ---------------------------------------------------------------------------

/// The admin API: GET /api/license (any authenticated session) and
/// GET/POST /api/users plus PUT /api/users/{sub}/roles (admin only).
pub fn route_admin(
    method: &str,
    route: &str,
    body: &str,
    sessions: &Mutex<HashMap<String, crate::HttpSession>>,
    token: Option<&str>,
) -> (String, String, String) {
    let err = |code: u16, msg: String| json_response(code, &json!({"error": msg}));
    // Every route here needs an authenticated session — /api/license
    // included (the license state is not public).
    if let Err(msg) = check_auth(token, sessions) {
        return err(401, msg);
    }
    match (method, route) {
        // any authenticated session: the process license, or community
        ("GET", "/api/license") => json_response(200, &license_payload()),
        ("GET", "/api/users") => {
            if let Err(r) = admin_only(token, sessions) {
                return r;
            }
            let rows = match with_store(|s| s.list_users()) {
                Ok(rows) => rows,
                Err(r) => return r,
            };
            let users: Vec<J> = rows
                .iter()
                .map(|u| {
                    json!({
                        "id": u.id,
                        "sub": u.sub,
                        "name": u.name,
                        "roles": u.roles,
                        "created_at": u.created_at,
                    })
                })
                .collect();
            json_response(200, &json!({"users": users}))
        }
        ("POST", "/api/users") => {
            if let Err(r) = admin_only(token, sessions) {
                return r;
            }
            let j: J = match serde_json::from_str(body) {
                Ok(j) => j,
                Err(e) => return err(400, format!("bad JSON: {e}")),
            };
            let sub = match j
                .get("sub")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                Some(s) => s,
                None => return err(400, "missing sub".into()),
            };
            // Final review I-4: the bootstrap admin row is reserved — an
            // upsert here would overwrite it and brick the next restart's
            // seed INSERT.
            if sub == crate::users::ADMIN_SUB {
                return err(400, format!("reserved sub '{sub}'"));
            }
            let name = j.get("name").and_then(|v| v.as_str());
            let mut roles: Vec<String> = roles_of_json(&j).unwrap_or_default();
            // Final review I-3: v1 roles are {admin, viewer} — anything
            // else would pass check_rest as "not viewer" and grant writes.
            if let Err(r) = validate_roles(&roles) {
                return r;
            }
            // Final review N-1: an empty set (absent, [], or non-strings
            // only) must not fail open — it normalizes to viewer, the
            // stripped state (same as an unknown claim).
            if roles.is_empty() {
                roles = vec!["viewer".to_string()];
            }
            match with_store(|s| s.upsert_oidc_user(sub, name, &roles)) {
                Ok(()) => json_response(200, &json!({"data": {"sub": sub, "roles": roles}})),
                Err(r) => r,
            }
        }
        ("PUT", p) if p.starts_with("/api/users/") && p.ends_with("/roles") => {
            if let Err(r) = admin_only(token, sessions) {
                return r;
            }
            let sub = p
                .trim_start_matches("/api/users/")
                .trim_end_matches("/roles");
            let j: J = match serde_json::from_str(body) {
                Ok(j) => j,
                Err(e) => return err(400, format!("bad JSON: {e}")),
            };
            let mut roles: Vec<String> = match roles_of_json(&j) {
                Some(r) => r,
                None => return err(400, "missing roles".into()),
            };
            // Final review I-3: v1 roles are {admin, viewer} — anything
            // else would pass check_rest as "not viewer" and grant writes.
            if let Err(r) = validate_roles(&roles) {
                return r;
            }
            // Final review N-1: an empty set must not fail open — it
            // normalizes to viewer, the stripped state.
            if roles.is_empty() {
                roles = vec!["viewer".to_string()];
            }
            match with_store(|s| {
                // the row must exist — a bare UPDATE would silently no-op
                if s.roles_of(sub)?.is_none() {
                    return Ok(None);
                }
                s.set_roles(sub, &roles)?;
                Ok(Some(()))
            }) {
                Ok(Some(())) => json_response(200, &json!({"data": {"sub": sub, "roles": roles}})),
                Ok(None) => json_response(404, &json!({"error": "user not found"})),
                Err(r) => r,
            }
        }
        _ => json_response(404, &json!({"error": "Not Found"})),
    }
}

/// Task 8: the wire `roles` array of a user-route body — strings only.
/// None when the field is absent or not an array of strings.
fn roles_of_json(j: &J) -> Option<Vec<String>> {
    j.get("roles").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    })
}

/// Final review I-3: the admin API stores only v1 roles. An unknown value
/// would pass `check_rest` as "not viewer" (a write grant), so the
/// boundary refuses it with a 400.
fn validate_roles(roles: &[String]) -> Result<(), (String, String, String)> {
    for r in roles {
        if r != "admin" && r != "viewer" {
            return Err(json_response(
                400,
                &json!({"error": format!("invalid role '{r}' (admin or viewer)")}),
            ));
        }
    }
    Ok(())
}

/// Task 8's admin-only gate: the session's LIVE store roles (a revocation
/// takes effect on the next call), admin required — the same check_rest
/// the T6 REST write gate uses. With no user store it refuses: only the
/// flag-gated enterprise surfaces (route_audit_export) reach here
/// store-less; community route_v1 calls check_rest directly.
fn admin_only(
    token: Option<&str>,
    sessions: &Mutex<HashMap<String, crate::HttpSession>>,
) -> Result<(), (String, String, String)> {
    if crate::session::users().is_none() {
        return Err(json_response(403, &json!({"error": "admin role required"})));
    }
    let roles = crate::http::live_roles(token, sessions).unwrap_or_default();
    crate::authz::check_rest(&roles, true)
        .map_err(|e| json_response(e.status(), &json!({"error": e.message()})))
}

/// Run `f` against the shared user store. The "user store unavailable"
/// error covers community mode (no multi-user store is ever opened there).
fn with_store<T>(
    f: impl FnOnce(&crate::users::UserStore) -> rusqlite::Result<T>,
) -> Result<T, (String, String, String)> {
    let store = crate::session::users()
        .ok_or_else(|| json_response(500, &json!({"error": "user store unavailable"})))?;
    let guard = store.lock().unwrap(); // justified: Mutex poison is unrecoverable
    f(&guard).map_err(|e| json_response(500, &json!({"error": format!("user store: {e}")})))
}

/// Task 9: GET /api/audit-export — the retention-trimmed audit log as a
/// JSONL attachment. Admin-only (the same T6/T8 gate); http.rs dispatches
/// here only under the audit-export license flag, so unlicensed servers
/// keep the v0.4.0 plain 404.
pub fn route_audit_export(
    sessions: &Mutex<HashMap<String, crate::HttpSession>>,
    token: Option<&str>,
    db_path: &str,
) -> (String, String, Vec<(String, String)>, String) {
    if let Err(msg) = check_auth(token, sessions) {
        let (s, ct, b) = json_response(401, &json!({"error": msg}));
        return (s, ct, Vec::new(), b);
    }
    if let Err(r) = admin_only(token, sessions) {
        let (s, ct, b) = r;
        return (s, ct, Vec::new(), b);
    }
    match crate::admin::audit_jsonl(db_path) {
        Ok(bytes) => (
            "200 OK".to_string(),
            "application/x-ndjson".to_string(),
            vec![(
                "Content-Disposition".to_string(),
                "attachment; filename=\"aikoql-audit.jsonl\"".to_string(),
            )],
            bytes,
        ),
        Err(e) => {
            let (s, ct, b) = json_response(500, &json!({"error": e}));
            (s, ct, Vec::new(), b)
        }
    }
}

/// Task 8: the /api/license payload — the license fields on the wire.
/// Community (no LICENSE_STATE) reports tier "community" with empty
/// org/features and no expiry.
fn license_payload() -> J {
    let Some(state) = crate::license::LICENSE_STATE.get() else {
        return json!({"tier": "community", "org": "", "features": [], "expires": null});
    };
    let features = crate::license::feature_flags(&state.features);
    let expires = state
        .expires
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    json!({"tier": "enterprise", "org": state.org, "features": features, "expires": expires})
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
        ("Access-Control-Allow-Methods", "GET, POST, PUT, OPTIONS"),
        (
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization",
        ),
    ]
}

/// Task 6: the mutating POST routes the role gate closes for viewers —
/// every POST in route_inner that writes. Fail-closed: the set must stay
/// complete. Read POSTs (aikoql MATCH, find-similar, traverse, explain,
/// prove, reason/infer/predict, evaluate-policies, job-status, list-*
/// twins, eval/*, agent/* reports) stay reachable.
fn is_write_route(path: &str, body: &str) -> bool {
    // aikoql is query-aware: a MATCH reads, every other statement writes.
    if path == "/api/v1/aikoql" {
        let q: J = serde_json::from_str(body).unwrap_or(J::Null);
        let query = q.get("query").and_then(|q| q.as_str()).unwrap_or("");
        return !matches!(
            aikoql_compiler::parser::parse(query),
            Ok(aikoql_compiler::parser::ast::Statement::Match(_))
        );
    }
    matches!(
        path,
        "/api/v1/discover-ontology"
            | "/api/v1/remember"
            | "/api/v1/forget"
            | "/api/v1/evolve"
            | "/api/v1/verify"
            | "/api/v1/relate"
            | "/api/v1/deploy-program"
            | "/api/v1/execute-program"
            | "/api/v1/deploy-policy"
            | "/api/v1/deploy-workflow"
            | "/api/v1/deploy-trigger"
            | "/api/v1/add-dependency"
            | "/api/v1/deploy-agent"
            | "/api/v1/execute-agent"
            | "/api/v1/deploy-connector"
            | "/api/v1/deploy-view"
            | "/api/v1/deploy-report"
            | "/api/v1/deploy-benchmark"
            | "/api/v1/execute-workflow"
            | "/api/v1/check-triggers"
            | "/api/v1/approve-job"
            | "/api/v1/documents"
            | "/api/v1/documents/compile"
            | "/api/v1/agent/memory-store"
            | "/api/v1/agent/memory-update"
            | "/api/v1/agent/memory-delete"
    )
}

/// Task 6: the REST routes the role gate keeps admin-only — the
/// storage-admin operations (tools/admin.rs).
fn is_admin_route(path: &str) -> bool {
    matches!(
        path,
        "/api/v1/backup" | "/api/v1/restore" | "/api/v1/verify-backup"
    )
}

fn json_response(code: u16, body: &J) -> (String, String, String) {
    let status = match code {
        200 => "200 OK",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        403 => "403 Forbidden",
        404 => "404 Not Found",
        409 => "409 Conflict",
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

    // ---- Studio hardening M3: one active ontology, saved in place with CAS ----

    fn m3_kernel(tag: &str) -> (crate::Kernel, std::path::PathBuf) {
        let db = std::env::temp_dir().join(format!("mnemo-m3-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&db);
        let k = crate::Kernel::open(
            std::sync::Arc::new(
                aikoql_storage_v2::AikoqlStorageEngineV2::open(&db).expect("open store"),
            ),
            std::sync::Arc::new(crate::SystemClock),
            0,
        )
        .expect("open kernel");
        (k, db)
    }

    fn m3_call(
        k: &crate::Kernel,
        db: &std::path::Path,
        method: &str,
        path: &str,
        body: serde_json::Value,
    ) -> (String, serde_json::Value) {
        let sessions = Mutex::new(HashMap::new());
        sessions.lock().unwrap().insert(
            "tok".into(),
            crate::HttpSession {
                username: "admin".into(),
                roles: vec!["admin".into()],
                created: std::time::Instant::now(),
                ttl_secs: 86_400,
            },
        );
        let rate = Mutex::new(crate::rate_limiter::RateLimiter::new(false, 1));
        let (status, _ct, out) = route_v1(
            method,
            path,
            &body.to_string(),
            k,
            &db.to_string_lossy(),
            &sessions,
            Some("tok".into()),
            &rate,
            None,
        );
        (
            status,
            serde_json::from_str(&out).unwrap_or(serde_json::json!({"raw": out})),
        )
    }

    fn m3_def(ns: &str, class: &str) -> serde_json::Value {
        serde_json::json!({
            "namespace": ns, "version": "1.0",
            "classes": [{"name": class}], "relationships": [], "mappings": [],
        })
    }

    fn m3_ontology_kos(k: &crate::Kernel) -> usize {
        k.scan_by_type(
            &crate::Subject::with_roles("admin", &["admin"]),
            crate::ONTOLOGY_TYPE,
        )
        .expect("scan")
        .len()
    }

    fn m3_cleanup(k: crate::Kernel, db: std::path::PathBuf) {
        drop(k);
        let _ = std::fs::remove_dir_all(&db);
        let _ = std::fs::remove_file(format!("{}.audit.log", db.to_string_lossy()));
    }

    #[test]
    fn m3_the_active_ontology_endpoint_agrees_with_the_compile_registry() {
        let _serial = crate::http::ontology_serial();
        let (k, db) = m3_kernel("active");
        for (ns, class, tags) in [
            ("manual-ns", "ManualClass", vec![]),
            ("auto-ns", "AutoClass", vec!["auto-discovered"]), // newer, but manual wins
        ] {
            crate::tools::tool_remember(
                &k,
                &serde_json::json!({
                    "subject": "admin", "roles": ["admin"], "type_name": "Ontology",
                    "tags": tags, "properties": m3_def(ns, class),
                }),
            )
            .expect("remember ontology");
        }
        let (status, body) = m3_call(&k, &db, "GET", "/api/v1/ontology", serde_json::json!({}));
        assert_eq!(status, "200 OK", "{body}");
        assert_eq!(body["data"]["source"], "manual", "{body}");
        assert_eq!(
            body["data"]["properties"]["namespace"], "manual-ns",
            "{body}"
        );
        let reg = crate::http::load_ontology_registry(&k);
        assert!(
            reg.resolve_class("ManualClass").is_some() && reg.resolve_class("AutoClass").is_none()
        );
        m3_cleanup(k, db);
    }

    #[test]
    fn m6_the_ontology_endpoint_scopes_to_the_requested_tenant() {
        let _serial = crate::http::ontology_serial();
        let (k, db) = m3_kernel("tenant-scope");
        for (ns, class, tenant) in [("g", "GlobalClass", None), ("t", "AcmeClass", Some("acme"))] {
            let mut a = serde_json::json!({
                "subject": "admin", "roles": ["admin"], "type_name": "Ontology",
                "properties": m3_def(ns, class),
            });
            if let Some(t) = tenant {
                a["tenant"] = serde_json::json!(t);
            }
            crate::tools::tool_remember(&k, &a).expect("remember ontology");
        }
        let ns_of = |path: &str| {
            let (status, body) = m3_call(&k, &db, "GET", path, serde_json::json!({}));
            assert_eq!(status, "200 OK", "{body}");
            body["data"]["properties"]["namespace"].clone()
        };
        assert_eq!(ns_of("/api/v1/ontology"), "g");
        assert_eq!(ns_of("/api/v1/ontology?tenant=acme"), "t");
        let (_, none) = m3_call(
            &k,
            &db,
            "GET",
            "/api/v1/ontology?tenant=other",
            serde_json::json!({}),
        );
        assert!(none["data"]["active"].is_null(), "{none}");
        m3_cleanup(k, db);
    }

    #[test]
    fn m3_saving_updates_the_one_ontology_in_place() {
        let _serial = crate::http::ontology_serial();
        let (k, db) = m3_kernel("save");
        let (s1, one) = m3_call(
            &k,
            &db,
            "POST",
            "/api/v1/ontology",
            serde_json::json!({"definition": m3_def("ns", "First")}),
        );
        assert_eq!(s1, "200 OK", "{one}");
        let v1 = one["data"]["version"].as_u64().expect("version");
        let (s2, two) = m3_call(
            &k,
            &db,
            "POST",
            "/api/v1/ontology",
            serde_json::json!({"definition": m3_def("ns", "Second"), "expected_version": v1}),
        );
        assert_eq!(s2, "200 OK", "{two}");
        assert_eq!(
            one["data"]["koid"], two["data"]["koid"],
            "saved a second KO"
        );
        assert!(two["data"]["version"].as_u64() > Some(v1));
        assert_eq!(
            m3_ontology_kos(&k),
            1,
            "one logical ontology, many versions"
        );
        let reg = crate::http::load_ontology_registry(&k);
        assert!(
            reg.resolve_class("Second").is_some(),
            "registry not republished"
        );
        m3_cleanup(k, db);
    }

    #[test]
    fn m7_saving_with_a_tenant_writes_that_tenants_ontology_only() {
        let _serial = crate::http::ontology_serial();
        let (k, db) = m3_kernel("tenant-save");
        let save = |def: serde_json::Value, extra: serde_json::Value| {
            let mut body = serde_json::json!({ "definition": def });
            for (key, v) in extra.as_object().expect("object") {
                body[key] = v.clone();
            }
            m3_call(&k, &db, "POST", "/api/v1/ontology", body)
        };
        let (s, g) = save(m3_def("g", "GlobalClass"), serde_json::json!({}));
        assert_eq!(s, "200 OK", "{g}");
        let (s, t1) = save(
            m3_def("t", "AcmeOne"),
            serde_json::json!({"tenant": "acme"}),
        );
        assert_eq!(s, "200 OK", "{t1}");
        assert_ne!(
            g["data"]["koid"], t1["data"]["koid"],
            "tenant save hit the global KO"
        );
        // the tenant's own revision is checked, not the global one
        let v = t1["data"]["version"].clone();
        let (s, t2) = save(
            m3_def("t", "AcmeTwo"),
            serde_json::json!({"tenant": "acme", "expected_version": v}),
        );
        assert_eq!(s, "200 OK", "{t2}");
        assert_eq!(t1["data"]["koid"], t2["data"]["koid"]);
        assert_eq!(m3_ontology_kos(&k), 2, "one global + one tenant ontology");
        let ns = |p: &str| {
            m3_call(&k, &db, "GET", p, serde_json::json!({})).1["data"]["properties"]["classes"][0]
                ["name"]
                .clone()
        };
        assert_eq!(ns("/api/v1/ontology"), "GlobalClass");
        assert_eq!(ns("/api/v1/ontology?tenant=acme"), "AcmeTwo");
        m3_cleanup(k, db);
    }

    #[test]
    fn m8_an_image_document_compiles_and_an_unsupported_type_is_a_client_error() {
        let (k, db) = m3_kernel("compile-types");
        let compile = |filename: &str, mime: &str, b64: &str| {
            let (s, ingested) = m3_call(
                &k,
                &db,
                "POST",
                "/api/v1/documents",
                serde_json::json!({"filename": filename, "mime_type": mime, "content_base64": b64}),
            );
            assert_eq!(s, "200 OK", "{ingested}");
            m3_call(
                &k,
                &db,
                "POST",
                "/api/v1/documents/compile",
                serde_json::json!({ "koid": ingested["data"]["koid"] }),
            )
        };
        // a real 1x1 PNG: supported — compiles even with no OCR tool installed
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let (status, body) = compile("shot.png", "image/png", png);
        assert_eq!(status, "200 OK", "{body}");
        // a type no extractor reads is bad input, not a server fault
        let (status, body) = compile("a.zip", "application/zip", "UEsDBA==");
        assert_eq!(status, "400 Bad Request", "{body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap_or("")
                .contains("unsupported mime type"),
            "{body}"
        );
        m3_cleanup(k, db);
    }

    #[test]
    fn m8_an_image_reports_its_ocr_outcome_through_ingest_and_compile() {
        use base64::Engine;
        let (k, db) = m3_kernel("image-ocr");
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let ingest = |filename: &str, mime: &str, content: String| {
            let (s, body) = m3_call(
                &k,
                &db,
                "POST",
                "/api/v1/documents",
                serde_json::json!({"filename": filename, "mime_type": mime, "content_base64": content}),
            );
            assert_eq!(s, "200 OK", "{body}");
            body["data"].clone()
        };
        let compile = |koid: &serde_json::Value| {
            m3_call(
                &k,
                &db,
                "POST",
                "/api/v1/documents/compile",
                serde_json::json!({ "koid": koid }),
            )
        };
        let has_tesseract = aikoql_ingestion::tool_available("tesseract");
        // a browser that sends no type, or octet-stream: the extension decides
        let fixtures = [
            (
                "scan.png",
                "",
                &include_bytes!("../../../../ingestion/tests/fixtures/ocr-text.png")[..],
                "image/png",
            ),
            (
                "scan.jpg",
                "application/octet-stream",
                &include_bytes!("../../../../ingestion/tests/fixtures/ocr-text.jpg")[..],
                "image/jpeg",
            ),
        ];
        for (name, sent, bytes, want) in fixtures {
            let d = ingest(name, sent, b64(bytes));
            assert_eq!(d["page_count"], 1, "{name}: {d}");
            let (s, c) = compile(&d["koid"]);
            assert_eq!(s, "200 OK", "{name}: {c}");
            let c = &c["data"];
            assert_eq!(c["mime_type"], want, "{name}");
            assert_eq!(c["extraction_status"], d["status"], "{name}: {c}");
            if has_tesseract {
                assert_eq!(d["status"], "ocr_complete", "{name}: {d}");
                assert!(d["char_count"].as_i64().unwrap() > 0, "{name}: {d}");
                assert!(c["total_chars"].as_i64().unwrap() > 0, "{name}");
                assert!(
                    c.to_string().contains("Utrecht"),
                    "{name}: OCR text compiled"
                );
            } else {
                eprintln!("m8 image-ocr: tesseract absent — asserting the unavailable branch");
                assert_eq!(d["status"], "ocr_unavailable", "{name}: {d}");
                assert_eq!(d["char_count"], 0, "{name}: {d}");
                assert_eq!(c["total_chars"], 0, "{name}");
            }
        }
        // a zero-byte image: stored, but compiling it is bad input, not a 500
        let d = ingest("empty.png", "image/png", String::new());
        let (s, body) = compile(&d["koid"]);
        assert_eq!(s, "400 Bad Request", "{body}");
        m3_cleanup(k, db);
    }

    fn m9_ingest(
        k: &crate::Kernel,
        db: &std::path::Path,
        filename: &str,
        mime: &str,
        bytes: &[u8],
    ) -> (String, serde_json::Value) {
        use base64::Engine;
        m3_call(
            k,
            db,
            "POST",
            "/api/v1/documents",
            serde_json::json!({
                "filename": filename, "mime_type": mime,
                "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
            }),
        )
    }

    #[test]
    fn m10_office_and_tabular_formats_are_extracted_and_compile_with_tables() {
        let office = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../ingestion/tests/fixtures/office"
        );
        let read = |f: &str| std::fs::read(format!("{office}/{f}")).expect("fixture");
        let (k, db) = m3_kernel("office");
        // Browsers send "" or a legacy type for these; the extension decides.
        for (file, sent, pages) in [
            ("workbook.xlsx", "", 2),
            ("contacts.csv", "application/vnd.ms-excel", 1),
            ("deck.pptx", "application/octet-stream", 4),
            ("metrics.tsv", "", 1),
        ] {
            let (status, body) = m9_ingest(&k, &db, file, sent, &read(file));
            assert_eq!(status, "200 OK", "{file}: {body}");
            assert_eq!(body["data"]["status"], "extracted", "{file}: {body}");
            assert_eq!(body["data"]["page_count"], pages, "{file}: {body}");
            let (status, compiled) = m3_call(
                &k,
                &db,
                "POST",
                "/api/v1/documents/compile",
                serde_json::json!({ "koid": body["data"]["koid"] }),
            );
            assert_eq!(status, "200 OK", "{file}: {compiled}");
            assert!(
                compiled.to_string().contains("TableCell"),
                "{file}: table cells reach compile as cell-cited facts: {compiled}"
            );
        }
        m3_cleanup(k, db);
    }

    #[test]
    fn m11_a_document_over_a_policy_limit_is_classified_not_silently_ingested() {
        let office = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../ingestion/tests/fixtures/office"
        );
        let dir = std::env::temp_dir().join(format!("aikoql-limit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let art = dir.join("deck");
        std::fs::copy(format!("{office}/deck.pptx"), &art).unwrap();
        let policy = aikoql_ingestion::ProcessingPolicy {
            max_pages: 2,
            ..Default::default()
        };
        let pptx = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
        let out = crate::tools::ingestion::extract_for_ingest_with(
            &dir.to_string_lossy(),
            &art.to_string_lossy(),
            "deck",
            "deck.pptx",
            pptx,
            &policy,
        );
        assert_eq!(out.status, "limit_exceeded");
        assert_eq!(out.pages, 0);
        let err = out.error.unwrap_or_default();
        assert!(
            err.contains("max_pages") && err.contains("4 pages > 2"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn m9_an_image_type_no_extractor_reads_is_refused_at_upload() {
        let (k, db) = m3_kernel("heic");
        let (status, body) = m9_ingest(&k, &db, "IMG_0001.heic", "image/heic", b"ftypheic");
        assert_eq!(status, "400 Bad Request", "{body}");
        let msg = body["error"].as_str().unwrap_or("");
        assert!(
            msg.contains("unsupported mime type") && msg.contains("PNG or JPEG"),
            "{body}"
        );
        // non-image types keep the old behaviour: stored, just not extractable
        let (status, body) = m9_ingest(&k, &db, "a.zip", "application/zip", b"PK\x03\x04");
        assert_eq!(status, "200 OK", "{body}");
        m3_cleanup(k, db);
    }

    #[test]
    fn m9_re_uploading_an_image_after_ocr_is_installed_refreshes_its_status() {
        if !aikoql_ingestion::tool_available("tesseract") {
            eprintln!("skip: tesseract not installed");
            return;
        }
        let png = include_bytes!("../../../../ingestion/tests/fixtures/ocr-text.png");
        let (k, db) = m3_kernel("stale-status");
        let sha = aikoql_ingestion::content_hash(png);
        // as left behind by an upload made while OCR was not installed
        k.deploy_document(
            "scan.png",
            "image/png",
            &sha,
            png.len() as i64,
            1,
            0,
            "ocr_unavailable",
            &crate::Subject::with_roles("admin", &["admin"]),
        )
        .expect("seed stale document");
        let (status, body) = m9_ingest(&k, &db, "scan.png", "image/png", png);
        assert_eq!(status, "200 OK", "{body}");
        assert_eq!(body["data"]["status"], "ocr_complete", "{body}");
        let (_, list) = m3_call(
            &k,
            &db,
            "GET",
            "/api/v1/list-documents",
            serde_json::json!({}),
        );
        let doc = &list["data"]["documents"][0];
        assert_eq!(doc["status"], "ocr_complete", "{list}");
        assert!(doc["char_count"].as_i64().unwrap_or(0) > 0, "{list}");
        assert_eq!(
            list["data"]["documents"].as_array().map(|d| d.len()),
            Some(1)
        );
        m3_cleanup(k, db);
    }

    #[test]
    fn m3_a_stale_or_missing_expected_version_cannot_overwrite() {
        let _serial = crate::http::ontology_serial();
        let (k, db) = m3_kernel("cas");
        let (_, one) = m3_call(
            &k,
            &db,
            "POST",
            "/api/v1/ontology",
            serde_json::json!({"definition": m3_def("ns", "First")}),
        );
        let v1 = one["data"]["version"].as_u64().expect("version");
        let (_, two) = m3_call(
            &k,
            &db,
            "POST",
            "/api/v1/ontology",
            serde_json::json!({"definition": m3_def("ns", "Second"), "expected_version": v1}),
        );
        assert!(two["data"]["version"].is_u64(), "{two}");
        for (label, body) in [
            (
                "stale",
                serde_json::json!({"definition": m3_def("ns", "Lost"), "expected_version": v1}),
            ),
            (
                "missing",
                serde_json::json!({"definition": m3_def("ns", "Lost")}),
            ),
        ] {
            let (status, out) = m3_call(&k, &db, "POST", "/api/v1/ontology", body);
            let want = if label == "stale" {
                "409 Conflict"
            } else {
                "400 Bad Request"
            };
            assert_eq!(status, want, "{label} expected_version: {out}");
        }
        let reg = crate::http::load_ontology_registry(&k);
        assert!(reg.resolve_class("Second").is_some() && reg.resolve_class("Lost").is_none());
        m3_cleanup(k, db);
    }

    #[test]
    fn m3_an_invalid_definition_is_rejected_and_stores_nothing() {
        let _serial = crate::http::ontology_serial();
        let (k, db) = m3_kernel("invalid");
        let (ok, good) = m3_call(
            &k,
            &db,
            "POST",
            "/api/v1/ontology",
            serde_json::json!({"definition": m3_def("ns", "Good")}),
        );
        assert_eq!(ok, "200 OK", "{good}");
        let v = good["data"]["version"].as_u64().expect("version");
        let bad = serde_json::json!({
            "namespace": "ns", "version": "1.0",
            "classes": [{"name": "Orphan", "parent": "NoSuchParent"}],
        });
        let (status, out) = m3_call(
            &k,
            &db,
            "POST",
            "/api/v1/ontology",
            serde_json::json!({"definition": bad, "expected_version": v}),
        );
        assert_eq!(status, "400 Bad Request", "unknown parent: {out}");
        let reg = crate::http::load_ontology_registry(&k);
        assert!(reg.resolve_class("Good").is_some() && reg.resolve_class("Orphan").is_none());
        assert_eq!(m3_ontology_kos(&k), 1);
        m3_cleanup(k, db);
    }

    #[test]
    fn s04_get_prove_reads_koid_from_query_string() {
        // POC-4 S-4: the studio calls GET /api/v1/prove?koid=…; the REST
        // layer must fold query args into tool args (GET carries no body) or
        // the call dies with "missing argument: koid".
        let db = std::env::temp_dir().join(format!("mnemo-s04-prove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&db);
        let k = crate::Kernel::open(
            std::sync::Arc::new(
                aikoql_storage_v2::AikoqlStorageEngineV2::open(std::path::Path::new(&db))
                    .expect("open store"),
            ),
            std::sync::Arc::new(crate::SystemClock),
            0,
        )
        .expect("open kernel");
        let r = k
            .remember(crate::RememberRequest::create(
                crate::Subject::with_roles("admin", &["admin"]),
                crate::Metadata {
                    type_name: "Fact".into(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
            ))
            .expect("remember");
        let koid = r.koid.to_hex();

        let sessions = Mutex::new(HashMap::new());
        sessions.lock().unwrap().insert(
            "tok".into(),
            crate::HttpSession {
                username: "admin".into(),
                roles: vec!["admin".into()],
                created: std::time::Instant::now(),
                ttl_secs: 86_400,
            },
        );
        let rate = Mutex::new(crate::rate_limiter::RateLimiter::new(false, 1));

        let (status, _ct, body) = route_v1(
            "GET",
            &format!("/api/v1/prove?koid={koid}"),
            "",
            &k,
            &db.to_string_lossy(),
            &sessions,
            Some("tok".into()),
            &rate,
            None,
        );
        drop(k);
        let _ = std::fs::remove_dir_all(&db);
        let _ = std::fs::remove_file(format!("{}.audit.log", db.to_string_lossy()));

        assert_eq!(
            status, "200 OK",
            "GET prove with ?koid= must verify the chain, got: {body}"
        );
        let j: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(j["data"]["chain_valid"], true, "chain must verify: {body}");
    }

    #[test]
    fn s04_studio_prove_call_carries_the_koid() {
        // POC-4 S-4: the studio's Prove button must send the inspected KOID
        // and guard the empty-input case instead of a blind GET /api/v1/prove.
        let html = crate::studio::page_for_tests();
        assert!(
            html.contains("/api/v1/prove?koid=' + encodeURIComponent"),
            "studio prove must carry the koid in the query string"
        );
        assert!(
            html.contains("Enter a KOID"),
            "studio prove must guard the empty-input case"
        );
    }

    #[test]
    fn s03_studio_panels_render_events_with_kind_actor_note_and_formatted_times() {
        // POC-4 S-3/S-6: provenance + timeline must render events[] with
        // kind/actor/note/audit_hash and human-readable times (ISO + relative),
        // plus the event-time column when the version carries valid_from.
        let html = crate::studio::page_for_tests();
        assert!(
            html.contains("function fmtTime("),
            "studio needs a time formatter"
        );
        assert!(
            html.contains("data.events || data.versions"),
            "panels must prefer events[] over versions[]"
        );
        assert!(
            html.contains("event time: '"),
            "timeline must label the event-time column"
        );
        assert!(
            html.contains(";color:var(--muted);\">Actor: '"),
            "provenance must show the actor"
        );
        assert!(
            html.contains(";color:var(--muted);\">Note: '"),
            "provenance must show the note"
        );
        assert!(
            html.contains("e.audit_hash"),
            "provenance must show the audit hash"
        );
        assert!(
            html.contains("'s ago'"),
            "formatter must render relative time"
        );
    }

    #[test]
    fn s04_studio_prove_renders_the_response_fields() {
        // POC-4 S-4 follow-through: the prove panel must render the
        // response's actual fields — the chain_valid verdict and the
        // hex-joined head hash — not fields the response never carries.
        let html = crate::studio::page_for_tests();
        assert!(
            html.contains("chain_valid"),
            "prove panel must show the chain_valid verdict"
        );
        assert!(
            html.contains("padStart(2,'0')"),
            "prove panel must hex-join the head-hash array"
        );
        assert!(
            html.contains("(data.events||0)"),
            "prove panel must render the events count (events is a number, not an array)"
        );
    }

    #[test]
    fn s01_graph_nodes_style_from_epistemic_status_not_lifecycle() {
        // POC-4 S-1: retracted rows keep lifecycle.state "draft" — the graph
        // must style nodes from extensions.epistemic_status (superseded →
        // dashed border + muted palette) and surface the epistemic_history
        // transition in the tooltip.
        let html = crate::studio::page_for_tests();
        assert!(
            html.contains("n.extensions&&n.extensions.epistemic_status"),
            "node styling must read extensions.epistemic_status"
        );
        assert!(
            html.contains("borderDashes:[4,3]"),
            "superseded nodes must get a dashed border"
        );
        assert!(
            html.contains("epistemic_history"),
            "the tooltip must surface the epistemic transition"
        );
    }

    #[test]
    fn s02_graph_derives_virtual_retraction_edges() {
        // POC-4 S-2: retraction creates no SUPERSEDES edge (B3-3) — the
        // studio derives virtual "retracts" edges: same tenant + case_id +
        // kind, with the successor's valid_from equal to the predecessor's
        // valid_to_asserted.
        let html = crate::studio::page_for_tests();
        assert!(
            html.contains("valid_to_asserted"),
            "the virtual-edge heuristic must key on valid_to_asserted"
        );
        assert!(
            html.contains("label:'retracts'"),
            "virtual edges must be labeled retracts"
        );
        assert!(
            html.contains("params.push('detail=1')"),
            "the graph load must request detail payloads (extensions)"
        );
    }

    #[test]
    fn s08_editor_1030_error_surface_points_at_auto_discover() {
        // POC-4 S-8: when the query editor surfaces an AIKOQL1030, it must
        // point at the discovery flow instead of leaving a dead end.
        let html = crate::studio::page_for_tests();
        assert!(
            html.contains("includes('AIKOQL1030')"),
            "the editor must detect the unknown-type error"
        );
        assert!(
            html.contains("run Ontology → Auto-Discover"),
            "the editor must point at the discovery flow"
        );
    }

    #[test]
    fn s05_editor_labels_the_timebases() {
        // POC-4 S-5: AS_OF (wall-clock MVCC over commit_ts) and BETWEEN
        // (asserted event-time over valid_from/valid_to_asserted) are
        // different timebases that the editor presented as one "time travel"
        // — against 2023-event/2026-ingest corpora, event-anchored AS_OF
        // always looked broken. The editor label must name each timebase.
        let html = crate::studio::page_for_tests();
        assert!(
            html.contains("AS_OF = wall-clock"),
            "the editor must label AS_OF's timebase"
        );
        assert!(
            html.contains("BETWEEN = event-time"),
            "the editor must label BETWEEN's timebase"
        );
    }

    #[test]
    fn s10_api_helper_401_shows_login_overlay() {
        // POC-4 S-10: after a server restart invalidates the session, the
        // studio kept the stale "admin (admin)" badge and printed the raw
        // "login required" body into the results panel — a manual reload was
        // the only way back. A 401 from the api helper must drop the stale
        // session and return the user to the login overlay with a message.
        let html = crate::studio::page_for_tests();
        assert!(
            html.contains("res.status === 401"),
            "the api helper must detect a 401"
        );
        assert!(
            html.contains("Session expired"),
            "a 401 must explain the session expiry"
        );
        assert!(
            html.contains("showLogin()"),
            "a 401 must return the user to the login overlay"
        );
    }

    #[test]
    fn s13_sidebar_does_not_animate_its_width() {
        // POC-4 S-13: the sidebar's hover expansion animated its width, so
        // the nav buttons' hit area moved under a probing mouse — Playwright
        // reported "element not stable / intercepts pointer events" with the
        // sidebar div itself as the interceptor, and a slow-machine user
        // sees the same flakiness. The audit found no overlay: the moving
        // hit area is the width transition, so the rule must not carry one.
        let html = crate::studio::page_for_tests();
        assert!(
            !html.contains("transition: width"),
            "the sidebar must not animate its width under the cursor"
        );
        assert!(
            html.contains("#sidebar:hover { width: 190px; }"),
            "the hover expansion itself stays (icons → labels)"
        );
    }

    #[test]
    fn with_principal_token_overrides_body() {
        let sessions = Mutex::new(HashMap::new());
        sessions.lock().unwrap().insert(
            "tok".into(),
            crate::HttpSession {
                username: "admin".into(),
                roles: vec!["admin".into()],
                created: std::time::Instant::now(),
                ttl_secs: 86_400,
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

    // ---- Over-limit documents are processed batch by batch ----------------

    fn b_office(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/../../../ingestion/tests/fixtures/office/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture")
    }

    fn b_open(db: &std::path::Path) -> crate::Kernel {
        crate::Kernel::open(
            std::sync::Arc::new(aikoql_storage_v2::AikoqlStorageEngineV2::open(db).expect("open")),
            std::sync::Arc::new(crate::SystemClock),
            0,
        )
        .expect("open kernel")
    }

    const B_PPTX: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.presentation";

    fn b_ingest(
        k: &crate::Kernel,
        db: &std::path::Path,
        policy: &aikoql_ingestion::ProcessingPolicy,
    ) -> serde_json::Value {
        use base64::Engine;
        crate::tools::ingestion::document_ingest_with(
            k,
            &serde_json::json!({
                "filename": "deck.pptx", "mime_type": B_PPTX,
                "content_base64": base64::engine::general_purpose::STANDARD.encode(b_office("deck.pptx")),
                "subject": "admin", "roles": ["admin"],
            }),
            &db.to_string_lossy(),
            policy,
        )
        .expect("ingest")
    }

    /// Polls REST status to a terminal state (bounded; the worker is fast).
    fn b_wait(k: &crate::Kernel, db: &std::path::Path, koid: &str) -> serde_json::Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let path = format!("/api/v1/documents/{koid}/status");
            let (status, body) = m3_call(k, db, "GET", &path, serde_json::json!({}));
            assert_eq!(status, "200 OK", "{body}");
            if body["data"]["status"] != "processing" {
                return body["data"].clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "never finished: {body}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Every number under a "page"/"page_number" key in a JSON tree.
    fn b_pages(v: &serde_json::Value, out: &mut std::collections::BTreeSet<u64>) {
        match v {
            serde_json::Value::Object(m) => {
                for (key, val) in m {
                    if key == "page" || key == "page_number" {
                        if let Some(n) = val.as_u64() {
                            out.insert(n);
                        }
                    }
                    b_pages(val, out);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| b_pages(x, out)),
            _ => {}
        }
    }

    #[test]
    fn batch_an_over_limit_document_is_processed_in_batches_then_compiles_whole() {
        let (k, db) = m3_kernel("batch-ok");
        let policy = aikoql_ingestion::ProcessingPolicy {
            max_pages: 1,
            ..Default::default()
        };
        let resp = b_ingest(&k, &db, &policy);
        assert_eq!(resp["status"], "processing", "{resp}");
        assert_eq!(resp["page_count"], 4, "{resp}");
        assert_eq!(resp["batches_total"], 4, "{resp}");
        let msg = resp["message"].as_str().unwrap_or("");
        assert!(msg.contains("4 batches") && msg.contains("wait"), "{resp}");
        let koid = resp["koid"].as_str().unwrap().to_string();

        let done = b_wait(&k, &db, &koid);
        assert_eq!(done["status"], "extracted", "{done}");
        assert_eq!(done["batches_done"], 4, "{done}");
        assert_eq!(done["pages_done"], 4, "{done}");
        let batches = done["batches"].as_array().expect("batches").clone();
        assert_eq!(batches.len(), 4, "{done}");
        let mut chars = 0;
        for (i, b) in batches.iter().enumerate() {
            assert_eq!(b["first_page"], i as u64 + 1, "{b}");
            assert_eq!(b["last_page"], i as u64 + 1, "{b}");
            assert_eq!(b["status"], "extracted", "{b}");
            chars += b["char_count"].as_i64().unwrap_or(0);
        }
        assert!(chars > 0 && done["char_count"] == chars, "{done}");

        let (status, list) = m3_call(
            &k,
            &db,
            "GET",
            "/api/v1/list-documents",
            serde_json::json!({}),
        );
        assert_eq!(status, "200 OK");
        let row = &list["data"]["documents"][0];
        assert_eq!(
            (row["batches_total"].clone(), row["batches_done"].clone()),
            (4.into(), 4.into()),
            "{list}"
        );

        // compile reads the stored batch results (the global policy would
        // not matter): every slide, real page numbers
        let (status, compiled) = m3_call(
            &k,
            &db,
            "POST",
            "/api/v1/documents/compile",
            serde_json::json!({ "koid": koid }),
        );
        assert_eq!(status, "200 OK", "{compiled}");
        let mut pages = std::collections::BTreeSet::new();
        b_pages(&compiled["data"], &mut pages);
        assert!(
            pages.contains(&4) && pages.iter().all(|p| (1..=4).contains(p)),
            "{pages:?}"
        );
        assert_eq!(
            compiled["data"]["batches"].as_array().map(|a| a.len()),
            Some(4)
        );
        m3_cleanup(k, db);
    }

    #[test]
    fn batch_a_failed_batch_is_recorded_and_the_others_are_kept() {
        let (k, db) = m3_kernel("batch-partial");
        // slides with an image fail their batch; the rest succeed
        let policy = aikoql_ingestion::ProcessingPolicy {
            max_pages: 1,
            max_image_count: 0,
            ..Default::default()
        };
        let resp = b_ingest(&k, &db, &policy);
        assert_eq!(resp["status"], "processing", "{resp}");
        let koid = resp["koid"].as_str().unwrap().to_string();
        let done = b_wait(&k, &db, &koid);
        assert_eq!(done["status"], "partial", "{done}");
        let batches = done["batches"].as_array().unwrap();
        let failed: Vec<_> = batches.iter().filter(|b| b["status"] == "failed").collect();
        assert!(!failed.is_empty() && failed.len() < 4, "{done}");
        assert!(
            failed.iter().all(|b| b["error"]
                .as_str()
                .unwrap_or("")
                .contains("max_image_count")),
            "{done}"
        );
        assert!(
            done["message"].as_str().unwrap_or("").contains("failed"),
            "{done}"
        );
        let (status, compiled) = m3_call(
            &k,
            &db,
            "POST",
            "/api/v1/documents/compile",
            serde_json::json!({ "koid": koid }),
        );
        assert_eq!(
            status, "200 OK",
            "a partial document compiles what was read: {compiled}"
        );
        assert_eq!(
            compiled["data"]["failed_batches"]
                .as_array()
                .map(|a| a.len()),
            Some(failed.len()),
            "{compiled}"
        );
        m3_cleanup(k, db);
    }

    #[test]
    fn batch_a_document_within_max_pages_over_the_image_limit_is_split_until_it_fits() {
        let (k, db) = m3_kernel("batch-split");
        // 4 slides fit max_pages; only slide 4 has an image, so the deck is
        // over max_image_count = 0 and is split until just slide 4 fails
        let policy = aikoql_ingestion::ProcessingPolicy {
            max_pages: 4,
            max_image_count: 0,
            ..Default::default()
        };
        let resp = b_ingest(&k, &db, &policy);
        assert_eq!(resp["status"], "processing", "{resp}");
        let koid = resp["koid"].as_str().unwrap().to_string();
        let done = b_wait(&k, &db, &koid);
        assert_eq!(done["status"], "partial", "{done}");
        assert_eq!(done["batches_done"], done["batches_total"], "{done}");
        let batches = done["batches"].as_array().unwrap();
        let mut next = 1;
        for b in batches {
            assert_eq!(b["first_page"], next, "contiguous, in page order: {done}");
            next = b["last_page"].as_u64().unwrap() + 1;
            let failed = b["status"] == "failed";
            assert_eq!(failed, b["first_page"] == 4, "only slide 4 fails: {done}");
        }
        assert_eq!(next, 5, "{done}");
        m3_cleanup(k, db);
    }

    #[test]
    fn batch_compile_while_processing_is_a_409_not_a_500() {
        let (k, db) = m3_kernel("batch-409");
        let admin = crate::Subject::with_roles("admin", &["admin"]);
        let r = k
            .deploy_document(
                "big.pdf",
                "application/pdf",
                "abc",
                10,
                50,
                0,
                "processing",
                &admin,
            )
            .unwrap();
        let (status, body) = m3_call(
            &k,
            &db,
            "POST",
            "/api/v1/documents/compile",
            serde_json::json!({ "koid": r.koid.to_hex() }),
        );
        assert_eq!(status, "409 Conflict", "{body}");
        let msg = body["error"].as_str().unwrap_or("");
        assert!(
            msg.contains("still processing") && msg.contains("wait"),
            "{msg}"
        );
        m3_cleanup(k, db);
    }

    #[test]
    fn batch_a_restart_marks_unfinished_documents_interrupted() {
        let (k, db) = m3_kernel("batch-restart");
        let alice = crate::Subject::with_roles("alice", &[]).in_tenant("t1");
        let r = k
            .deploy_document(
                "big.pdf",
                "application/pdf",
                "abc",
                10,
                50,
                0,
                "processing",
                &alice,
            )
            .unwrap();
        drop(k);
        let k = b_open(&db); // the restart
        assert_eq!(
            crate::tools::ingestion::interrupt_unfinished_documents(&k),
            1
        );
        let path = format!("/api/v1/documents/{}/status", r.koid.to_hex());
        let (_, body) = m3_call(&k, &db, "GET", &path, serde_json::json!({}));
        assert_eq!(body["data"]["status"], "interrupted", "{body}");
        assert!(
            body["data"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("Upload the file again"),
            "{body}"
        );
        let (status, body) = m3_call(
            &k,
            &db,
            "POST",
            "/api/v1/documents/compile",
            serde_json::json!({ "koid": r.koid.to_hex() }),
        );
        assert_eq!(status, "409 Conflict", "{body}");
        assert_eq!(
            crate::tools::ingestion::interrupt_unfinished_documents(&k),
            0,
            "idempotent"
        );

        // re-uploading an interrupted document processes it again
        let policy = aikoql_ingestion::ProcessingPolicy {
            max_pages: 2,
            ..Default::default()
        };
        let first = b_ingest(&k, &db, &policy);
        let koid = first["koid"].as_str().unwrap().to_string();
        b_wait(&k, &db, &koid);
        let admin = crate::Subject::with_roles("admin", &["admin"]);
        let deck = k
            .list_documents(&admin)
            .unwrap()
            .into_iter()
            .find(|d| d.koid.to_hex() == koid)
            .unwrap();
        k.set_document_properties(
            &deck.koid,
            [(
                "status".to_string(),
                crate::Value::Text("interrupted".into()),
            )]
            .into(),
            &admin,
        )
        .unwrap();
        let again = b_ingest(&k, &db, &policy);
        assert_eq!(again["status"], "processing", "{again}");
        assert_eq!(b_wait(&k, &db, &koid)["status"], "extracted");
        m3_cleanup(k, db);
    }

    // ---- source schema discovery routes ----

    #[test]
    fn the_sources_writes_are_admin_routes_and_the_list_is_not() {
        for p in ["/api/v1/sources/discover", "/api/v1/sources/ontology"] {
            assert!(is_admin_route(p), "{p}");
        }
        assert!(!is_admin_route("/api/v1/sources"));
    }

    #[test]
    fn sources_routes_map_errors_to_the_right_statuses() {
        use crate::sources::testkit::*;
        let (k, db) = m3_kernel("sources-routes");
        let crm = crm_db("routes");
        let missing = tmp_db("missing-routes")
            .parent()
            .unwrap()
            .join("gone.sqlite");
        crate::sources::with_test_sources(
            vec![sqlite_source("crm", None), sqlite_source("broken", None)],
            vec![
                ("crm".into(), conn(&crm)),
                ("broken".into(), conn(&missing)),
            ],
            || {
                let call =
                    |m: &str, path: &str, body: serde_json::Value| m3_call(&k, &db, m, path, body);
                let (st, out) = call("GET", "/api/v1/sources", serde_json::json!({}));
                assert!(st.starts_with("200"), "{st} {out}");
                assert_eq!(out["data"].as_array().unwrap().len(), 2);
                assert!(
                    call("POST", "/api/v1/sources/discover", serde_json::json!({}))
                        .0
                        .starts_with("400")
                );
                assert!(call(
                    "POST",
                    "/api/v1/sources/discover",
                    serde_json::json!({"source": "nope"})
                )
                .0
                .starts_with("404"));
                let (st, out) = call(
                    "POST",
                    "/api/v1/sources/discover",
                    serde_json::json!({"source": "broken"}),
                );
                assert!(st.starts_with("502"), "{st} {out}");
                assert!(!out["error"].as_str().unwrap().contains("gone.sqlite"));
                let (st, out) = call(
                    "POST",
                    "/api/v1/sources/discover",
                    serde_json::json!({"source": "crm"}),
                );
                assert!(
                    st.starts_with("200") && out["data"]["changed"] == true,
                    "{st} {out}"
                );
                let (st, pv) = call(
                    "POST",
                    "/api/v1/sources/ontology",
                    serde_json::json!({"sources": ["crm"], "preview": true}),
                );
                assert!(st.starts_with("200"), "{st} {pv}");
                let (st, _) = call(
                    "POST",
                    "/api/v1/sources/ontology",
                    serde_json::json!({"sources": ["crm"], "preview": false, "fingerprints": {"crm": "stale"}}),
                );
                assert!(st.starts_with("409"), "{st}");
                let (st, ok) = call(
                    "POST",
                    "/api/v1/sources/ontology",
                    serde_json::json!({"sources": ["crm"], "preview": false, "fingerprints": pv["data"]["fingerprints"]}),
                );
                assert!(
                    st.starts_with("200") && ok["data"]["saved"] == true,
                    "{st} {ok}"
                );
                assert!(call("POST", "/api/v1/sources/zzz", serde_json::json!({}))
                    .0
                    .starts_with("404"));
                assert!(call(
                    "POST",
                    "/api/v1/sources/ontology",
                    serde_json::json!({"sources": []})
                )
                .0
                .starts_with("400"));
            },
        );
        m3_cleanup(k, db);
    }
}
