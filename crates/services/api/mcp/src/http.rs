//! HTTP surface: REST/graph endpoints, login/sessions, Prometheus metrics,
//! and the metrics listener.
//! Extracted from main.rs (R7 modularization). No behavior changes.

use crate::*;

use crate::helpers::*;
use crate::tools::*;
pub(crate) fn serve_metrics(
    kernel: Arc<Kernel>,
    ontology: Arc<OntologyRegistry>,
    addr: &str,
    db_path: &Arc<String>,
) {
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            error!(%addr, %e, "metrics server bind failed");
            return;
        }
    };
    let sessions: Arc<Mutex<HashMap<String, HttpSession>>> = Arc::new(Mutex::new(HashMap::new()));
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                let k = kernel.clone();
                let sess = sessions.clone();
                let db = db_path.clone();
                let ont = ontology.clone();
                std::thread::spawn(move || handle_http(&mut s, &k, &sess, &db, &ont));
            }
            Err(e) => {
                // ponytail: don't die on transient accept errors.
                if cfg!(debug_assertions) {
                    eprintln!("metrics accept error: {}", e);
                }
            }
        }
    }
}

/// Build graph JSON: { nodes: [...], edges: [...] }.
/// Query params: ?koid=<hex> to center on a node, &detail=1 for properties.
/// Without koid, returns all heads + their outbound relationships.
pub(crate) fn graph_api(k: &Kernel, path: &str) -> Result<String, String> {
    let mut center_koid: Option<KOID> = None;
    let mut detail = false;
    let mut type_filter: Option<String> = None;

    // Parse query string (ponytail: manual parsing, no url crate dep).
    if let Some(qs) = path.split_once('?').map(|(_, q)| q) {
        for pair in qs.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                match k {
                    "koid" => {
                        center_koid =
                            Some(KOID::from_hex(v).map_err(|e| format!("bad koid: {}", e))?);
                    }
                    "detail" => {
                        detail = v == "1" || v == "true";
                    }
                    "type" => {
                        type_filter = Some(v.to_string());
                    }
                    _ => {}
                }
            }
        }
    }

    let browser_ctx = KnowledgeContext::from(Subject {
        name: "graph-browser".into(),
        roles: vec!["admin".into()],
        tenant: None, // unscoped admin; tenant is a visual filter below
    });

    // Parse tenant filter from query string.
    let tenant_filter: Option<String> = path.split_once('?').and_then(|(_, qs)| {
        qs.split('&')
            .filter_map(|p| p.split_once('='))
            .find(|(k, _)| *k == "tenant")
            .map(|(_, v)| v.to_string())
    });

    let heads = k.scan_heads().map_err(|e| format!("scan: {}", e))?;
    let mut nodes: Vec<J> = Vec::new();
    let mut edges: Vec<J> = Vec::new();
    let mut nodes_added: HashSet<String> = HashSet::new(); // to avoid duplicate nodes
    let mut edges_done: HashSet<(String, String)> = HashSet::new(); // to avoid duplicate edges
    let mut edge_counts: HashMap<String, usize> = HashMap::new();

    // Collect all heads (non-deleted), optionally filtered by tenant and type.
    let mut head_koids: Vec<KOID> = Vec::new();
    for (koid, _ver, _ts, state) in &heads {
        if *state == LifecycleState::Deleted {
            continue;
        }
        let ko = match k.get(browser_ctx.clone(), koid) {
            Ok(ko) => ko,
            Err(_) => continue,
        };
        if let Some(ref tf) = tenant_filter {
            if ko.metadata.tenant.as_deref() != Some(tf.as_str()) {
                continue;
            }
        }
        if let Some(ref tf) = type_filter {
            if ko.metadata.type_name != *tf {
                continue;
            }
        }
        head_koids.push(*koid);
    }

    // If a center KOID is specified, start traversal from it first.
    let start_koids: Vec<KOID> = if let Some(c) = center_koid {
        if let Ok(ko) = k.get(browser_ctx.clone(), &c) {
            if tenant_filter
                .as_ref()
                .is_none_or(|tf| ko.metadata.tenant.as_deref() == Some(tf.as_str()))
            {
                add_node_api(&mut nodes, &mut nodes_added, &ko, detail);
            }
        }
        let mut v = vec![c];
        v.extend(head_koids.iter().filter(|k| **k != c).cloned());
        v
    } else {
        head_koids
    };

    // Phase 1: add all head nodes (respecting tenant filter).
    for koid in &start_koids {
        let hex = koid.to_hex();
        if nodes_added.contains(&hex) {
            continue;
        }
        if let Ok(ko) = k.get(browser_ctx.clone(), koid) {
            add_node_api(&mut nodes, &mut nodes_added, &ko, detail);
        }
    }

    // Phase 2: traverse edges from EVERY starting node.
    for koid in &start_koids {
        traverse_edges(
            k,
            koid,
            &mut nodes,
            &mut nodes_added,
            &mut edges,
            &mut edges_done,
            detail,
            &browser_ctx,
            &mut edge_counts,
        );
    }

    // Apply edge counts to node sizes (hub nodes are bigger).
    for node in &mut nodes {
        let koid = node["koid"].as_str().unwrap_or("");
        let count = edge_counts.get(koid).copied().unwrap_or(0);
        // Size: base 18 + 4 per edge (max 42).
        let size = (18 + count * 4).min(42);
        node["size"] = json!(size);
        node["edge_count"] = json!(count);
    }

    // Collect available tenants for the filter dropdown.
    let mut tenants: Vec<String> = nodes
        .iter()
        .filter_map(|n| n["tenant"].as_str())
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    tenants.sort();

    Ok(json!({
        "nodes": nodes,
        "edges": edges,
        "tenants": tenants,
        "tenant_filter": tenant_filter,
    })
    .to_string())
}

pub(crate) fn add_node_api(
    nodes: &mut Vec<J>,
    nodes_added: &mut HashSet<String>,
    ko: &KnowledgeObject,
    detail: bool,
) {
    let hex = ko.koid.to_hex();
    if nodes_added.contains(&hex) {
        return;
    }
    nodes_added.insert(hex.clone());
    let c = color_for_type(&ko.metadata.type_name);
    let label = node_label(ko, 30);
    // justified: KO without tenant → empty tenant label
    let tenant = ko.metadata.tenant.clone().unwrap_or_default();
    let key_props: Vec<J> = ko
        .properties
        .iter()
        .take(3)
        .map(|(k, v)| json!({"key": k, "value": value_to_json(v)}))
        .collect();
    let mut node = json!({
        "koid": hex,
        "type_name": ko.metadata.type_name,
        "tenant": tenant,
        "label": label,
        "color": c,
        "version": ko.version,
        "key_props": key_props,
    });
    if detail {
        let props: serde_json::Map<String, J> = ko
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), value_to_json(v)))
            .collect();
        node["properties"] = json!(props);
        node["lifecycle"] = json!({
            "state": ko.lifecycle.state.to_string(),
            "origin": format!("{:?}", ko.lifecycle.origin),
        });
        node["tags"] = json!(ko.metadata.tags);
        node["schema_version"] = json!(ko.metadata.schema_version);
        node["security"] = json!({
            "owner": ko.security.owner,
            "classification": ko.security.classification,
            "acl_count": ko.security.acl.len(),
        });
        if let Some(ref sem) = ko.semantic {
            node["semantic"] = json!({
                "embedding_model": sem.embedding_model,
                "embedding_dims": sem.embedding.as_ref().map(|v| v.len()).unwrap_or(0),
                "confidence": sem.confidence,
                "summary": sem.summary,
            });
        }
        node["extensions"] = json!(ko
            .extensions
            .iter()
            .map(|(k, v)| (k.clone(), value_to_json(v)))
            .collect::<serde_json::Map<_, _>>());
        node["relationships"] = json!(ko
            .relationships
            .iter()
            .map(|r| json!({
                "target": r.target.to_hex(),
                "type": r.rel_type,
                "direction": format!("{:?}", r.direction),
            }))
            .collect::<Vec<_>>());
        node["event_refs"] = json!(ko.event_refs.len());
    }
    nodes.push(node);
}

/// Traverse outbound relationships from a node, adding edges and discovering new nodes.
/// Separate from node addition so every head gets edge-traversed, even nodes discovered
/// via another node's edges.
pub(crate) fn traverse_edges(
    k: &Kernel,
    koid: &KOID,
    nodes: &mut Vec<J>,
    nodes_added: &mut HashSet<String>,
    edges: &mut Vec<J>,
    edges_done: &mut HashSet<(String, String)>,
    detail: bool,
    ctx: &KnowledgeContext,
    edge_counts: &mut HashMap<String, usize>,
) {
    let q = TraverseQuery {
        context: ctx.clone(),
        start: *koid,
        rel_type: None,
        depth: 1,
        direction: Some(Direction::Outbound),
    };
    let hits = match k.traverse(q) {
        Ok(h) => h,
        Err(_) => return,
    };
    let source_hex = koid.to_hex();
    for h in &hits {
        let target_hex = h.koid.to_hex();
        // Skip duplicate edges.
        let edge_key = (source_hex.clone(), target_hex.clone());
        if edges_done.contains(&edge_key) {
            continue;
        }
        edges_done.insert(edge_key);
        edges.push(json!({
            "source": source_hex,
            "target": target_hex,
            "rel_type": h.rel_type,
        }));
        *edge_counts.entry(source_hex.clone()).or_insert(0) += 1;
        *edge_counts.entry(target_hex.clone()).or_insert(0) += 1;
        // Discover target node if not already added.
        if !nodes_added.contains(&target_hex) {
            if let Ok(ko) = k.get(ctx.clone(), &h.koid) {
                add_node_api(nodes, nodes_added, &ko, detail);
            }
        }
    }
}

/// Build a human-readable label for a knowledge object.
/// Priority: "name" property → "title" → first text property → type_name → KOID prefix.
pub(crate) fn node_label(ko: &KnowledgeObject, max_len: usize) -> String {
    // Try named properties first.
    for key in &["name", "title", "label", "subject", "id"] {
        if let Some(v) = ko.properties.get(*key) {
            let s = value_to_string(v);
            if !s.is_empty() {
                return truncate(&s, max_len);
            }
        }
    }
    // First text property
    for v in ko.properties.values() {
        if let Value::Text(s) = v {
            if !s.is_empty() {
                return truncate(s, max_len);
            }
        }
    }
    // Fallback: type_name
    truncate(&ko.metadata.type_name, max_len)
}

pub(crate) fn value_to_string(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        Value::Bytes(_) => "(binary)".into(),
        Value::List(items) => format!("[{} items]", items.len()),
        Value::Map(m) => format!("{{{} keys}}", m.len()),
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max.saturating_sub(3).min(s.len());
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

pub(crate) fn color_for_type(type_name: &str) -> &str {
    const COLORS: &[&str] = &[
        "#8be9fd", "#ff79c6", "#50fa7b", "#ffb86c", "#bd93f9", "#ff5555", "#f1fa8c", "#6be5c1",
        "#ff92d0", "#a6e3a1", "#89b4fa", "#fab387", "#cba6f7", "#f38ba8", "#94e2d5", "#74c7ec",
    ];
    let mut h: u32 = 0;
    for b in type_name.as_bytes() {
        h = h.wrapping_mul(31).wrapping_add(*b as u32);
    }
    COLORS[(h as usize) % COLORS.len()]
}

// ---------------------------------------------------------------------------
// HTTP auth helpers
// ---------------------------------------------------------------------------

pub(crate) fn extract_token(path: &str, req: &str) -> Option<String> {
    // From query param: ?token=abc123
    if let Some(qs) = path.split_once('?').map(|(_, q)| q) {
        for pair in qs.split('&') {
            if let Some(("token", v)) = pair.split_once('=') {
                return Some(v.to_string());
            }
        }
    }
    // From Authorization header: Bearer abc123
    for line in req.lines() {
        if let Some(v) = line.strip_prefix("Authorization: Bearer ") {
            return Some(v.trim().to_string());
        }
    }
    None
}

pub(crate) fn handle_login(
    body: &str,
    sessions: &Mutex<HashMap<String, HttpSession>>,
) -> Result<String, String> {
    let creds: J = serde_json::from_str(body).map_err(|e| format!("bad JSON: {}", e))?;
    let username = creds.get("username").and_then(|v| v.as_str()).unwrap_or("");
    let password = creds.get("password").and_then(|v| v.as_str()).unwrap_or("");

    // Default credentials (ponytail: hardcoded, config-file in prod).
    let valid = match username {
        "admin" => password == "admin",
        "user" => password == "user" || password == "readonly",
        _ => false,
    };
    if !valid {
        return Err("invalid credentials".into());
    }
    let roles: Vec<String> = if username == "admin" {
        vec!["admin".into()]
    } else {
        vec![]
    };
    // Generate a simple session token.
    // Generate session token from time + PID (ponytail: not cryptographic, fine for localhost UI).
    let token = format!(
        "{:x}{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::process::id()
    );
    // justified: Mutex poison is unrecoverable
    sessions.lock().unwrap().insert(
        token.clone(),
        HttpSession {
            username: username.to_string(),
            roles,
            created: Instant::now(),
        },
    );
    Ok(token)
}

pub(crate) fn validate_token(
    token: Option<&str>,
    sessions: &Mutex<HashMap<String, HttpSession>>,
) -> Option<Subject> {
    let token = token?;
    // justified: Mutex poison is unrecoverable
    let guard = sessions.lock().unwrap();
    let sess = guard.get(token)?;
    // Session expires after 24h.
    if sess.created.elapsed().as_secs() > 86400 {
        return None;
    }
    Some(Subject {
        name: sess.username.clone(),
        roles: sess.roles.clone(),
        tenant: None, // REST sessions carry no tenant claim; callers scope via query param
    })
}

pub(crate) fn aikoql_endpoint(
    k: &Kernel,
    query: &str,
    subject: &Subject,
    tenant: Option<&str>,
    ontology: &OntologyRegistry,
) -> Result<String, String> {
    if query.trim().is_empty() {
        return Err("empty query".into());
    }
    // Parse the aikoql statement.
    let stmt = aikoql_compiler::parser::parse(query).map_err(|e| e.to_string())?;

    // CREATE mutation.
    if let aikoql_compiler::parser::ast::Statement::Create(create) = &stmt {
        if !subject.roles.contains(&"admin".to_string()) {
            return Err("CREATE requires admin role".into());
        }
        let mut props = PropertyMap::new();
        for (k, v) in &create.properties {
            props.insert(k.clone(), compiler_expr_to_value(v));
        }
        let r = k
            .remember(RememberRequest {
                context: subject.clone().into(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: create.entity.clone(),
                    tenant: tenant.map(String::from),
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                security: None,
                extensions: ExtensionMap::new(),
                origin: Origin::Human,
                note: None,
                referential_policy: ReferentialPolicy::default(),
            })
            .map_err(|e| e.to_string())?;
        return Ok(
            json!({"created": r.koid.to_hex(), "version": r.version, "commit_ts": r.commit_ts})
                .to_string(),
        );
    }

    // Query: MATCH, TRAVERSE, etc. — ontology-aware compilation.
    // R9: compile scoped so the plan carries the caller's roles + tenant.
    let schema = SchemaRegistry::new(); // ponytail: empty registry; ontology handles resolution
    let plans = aikoql_compiler::parser::compile_with_ontology_scoped(
        query,
        &subject.name,
        &subject.roles,
        subject.tenant.as_deref(),
        &schema,
        Some(ontology),
    )
    .map_err(|e| e.to_string())?;
    // Execute all plans and merge results.
    let mut all_kos: Vec<serde_json::Value> = Vec::new();
    for plan in &plans {
        match aikoql_runtime::Interpreter::execute(k, plan).map_err(|e| e.to_string())? {
            aikoql_runtime::RowSet::Objects(kos) => {
                for ko in kos {
                    all_kos.push(json!({
                        "koid": ko.koid.to_hex(),
                        "type_name": ko.metadata.type_name,
                        "version": ko.version,
                        "properties": ko.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
                    }));
                }
            }
            aikoql_runtime::RowSet::Scored(scored) => {
                for (koid, score, tn, ver) in scored {
                    all_kos.push(json!({
                        "koid": koid.to_hex(), "score": score, "type_name": tn, "version": ver
                    }));
                }
            }
            aikoql_runtime::RowSet::Traversal(hits) => {
                for (koid, rt, depth) in hits {
                    all_kos.push(json!({
                        "koid": koid.to_hex(), "rel_type": rt, "depth": depth
                    }));
                }
            }
        }
    }
    Ok(json!({"results": all_kos}).to_string())
}

pub(crate) fn parse_query_param(path: &str, key: &str) -> String {
    path.split_once('?')
        .and_then(|(_, qs)| {
            qs.split('&')
                .filter_map(|p| p.split_once('='))
                .find(|(k, _)| *k == key)
                .map(|(_, v)| url_decode(v))
        })
        // justified: missing query param → empty value
        .unwrap_or_default()
}

pub(crate) fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(hex as char);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(' ');
        } else {
            out.push(bytes[i] as char);
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Schema discovery — critical for agent schema-awareness
// ---------------------------------------------------------------------------

pub(crate) fn schema_endpoint(k: &Kernel) -> Result<String, String> {
    let types = k.list_types().map_err(|e| format!("{}", e))?;
    let heads = k.scan_heads().map_err(|e| format!("{}", e))?;
    let mut schema: serde_json::Map<String, J> = serde_json::Map::new();

    // Aggregate property keys per type by scanning live objects.
    let ctx = KnowledgeContext::from(Subject {
        name: "schema-browser".into(),
        roles: vec!["admin".into()],
        tenant: None, // unscoped admin — full visibility
    });
    let mut type_props: HashMap<String, HashSet<String>> = HashMap::new();
    let mut type_counts: HashMap<String, usize> = HashMap::new();
    let mut type_tenants: HashMap<String, HashSet<String>> = HashMap::new();

    for (koid, _ver, _ts, state) in &heads {
        if *state == LifecycleState::Deleted {
            continue;
        }
        if let Ok(ko) = k.get(ctx.clone(), koid) {
            let tn = ko.metadata.type_name.clone();
            *type_counts.entry(tn.clone()).or_insert(0) += 1;
            if let Some(t) = &ko.metadata.tenant {
                type_tenants
                    .entry(tn.clone())
                    .or_default()
                    .insert(t.clone());
            }
            let entry = type_props.entry(tn).or_default();
            for key in ko.properties.keys() {
                entry.insert(key.clone());
            }
        }
    }

    for t in &types {
        // justified: type with no live objects → empty lists
        let info = json!({
            "count": type_counts.get(t).copied().unwrap_or(0),
            "properties": type_props.get(t).map(|s| s.iter().collect::<Vec<_>>()).unwrap_or_default(),
            "tenants": type_tenants.get(t).map(|s| s.iter().collect::<Vec<_>>()).unwrap_or_default(),
        });
        schema.insert(t.clone(), info);
    }

    Ok(json!({
        "types": types,
        "total_types": types.len(),
        "schema": schema,
    })
    .to_string())
}

// ---------------------------------------------------------------------------
// Query explain — shows the IR plan before execution
// ---------------------------------------------------------------------------

pub(crate) fn explain_endpoint(query: &str) -> Result<String, String> {
    let plan = aikoql_compiler::parser::compile(query).map_err(|e| e.to_string())?;
    Ok(json!({
        "query": query,
        "operators": plan.operators.iter().map(|op| format!("{:?}", op)).collect::<Vec<_>>(),
        "operator_count": plan.operators.len(),
    })
    .to_string())
}

pub(crate) fn handle_http(
    stream: &mut TcpStream,
    k: &Kernel,
    sessions: &Mutex<HashMap<String, HttpSession>>,
    db_path: &Arc<String>,
    ontology: &OntologyRegistry,
) {
    // ponytail: 64 KB buffer fits all practical HTTP requests. Browsers send
    // ~2-8 KB of headers; single read captures the full request.
    let mut buf = [0u8; 65536];
    let n = match stream.read(&mut buf) {
        Ok(0) => return,
        Ok(n) => n,
        Err(_) => return,
    };
    // Read body remainder if Content-Length exceeds what we already got.
    let req = String::from_utf8_lossy(&buf[..n]);
    let first_line = req.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0];
    let path = parts[1];
    let route = path.split('?').next().unwrap_or(path);
    let mut body_str = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    // If body truncated, read remainder.
    let hdr_part = req.split("\r\n\r\n").next().unwrap_or("");
    let mut content_len: Option<usize> = None;
    for line in hdr_part.lines() {
        if line.to_lowercase().starts_with("content-length:") {
            content_len = line.split(':').nth(1).and_then(|s| s.trim().parse().ok());
        }
    }
    if let Some(cl) = content_len {
        while body_str.len() < cl {
            let needed = cl - body_str.len();
            let extra = needed.max(4096);
            let mut rest = vec![0u8; extra];
            match stream.read(&mut rest) {
                Ok(0) => break,
                Ok(n) => body_str.push_str(&String::from_utf8_lossy(&rest[..n])),
                Err(_) => break,
            }
        }
    }

    // Extract token from query string or Authorization header.
    let token = extract_token(path, &req);

    // CORS preflight.
    if method == "OPTIONS" {
        let (status, ct, body) = api_rest::cors_preflight();
        let mut resp = format!(
            "HTTP/1.0 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
            status,
            ct,
            body.len()
        );
        for (k, v) in api_rest::cors_headers() {
            resp.push_str(&format!("{}: {}\r\n", k, v));
        }
        resp.push_str("\r\n");
        resp.push_str(&body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    // REST API v1 routes — handled separately, return early.
    if route.starts_with("/api/v1/") {
        let (status, ct, body) = api_rest::route_v1(
            method,
            path,
            &body_str,
            k,
            db_path.as_str(),
            sessions,
            token.clone(),
        );
        let mut resp = format!(
            "HTTP/1.0 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
            status,
            ct,
            body.len()
        );
        for (h, v) in api_rest::cors_headers() {
            resp.push_str(&format!("{}: {}\r\n", h, v));
        }
        resp.push_str("\r\n");
        resp.push_str(&body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    let (status, content_type, body) = match route {
        "/ui" => (
            "200 OK",
            "text/html; charset=utf-8",
            graph_ui::GRAPH_UI_HTML.to_string(),
        ),
        "/" | "/studio" => (
            "200 OK",
            "text/html; charset=utf-8",
            studio::STUDIO_HTML.to_string(),
        ),
        "/health" => {
            let uptime = SERVER_START
                .get()
                .map(|s| s.elapsed().as_secs_f64())
                .unwrap_or(0.0);
            let body =
                json!({"status":"ok","uptime_seconds":(uptime * 10.0).round() / 10.0}).to_string();
            ("200 OK", "application/json", body)
        }
        "/metrics" => {
            let body = prometheus_metrics(k);
            ("200 OK", "text/plain; version=0.0.4", body)
        }
        "/api/login" if method == "POST" => match handle_login(&body_str, sessions) {
            Ok(token) => (
                "200 OK",
                "application/json",
                json!({"token": token}).to_string(),
            ),
            Err(e) => (
                "401 Unauthorized",
                "application/json",
                json!({"error": e}).to_string(),
            ),
        },
        _p if route.starts_with("/api/graph") => {
            let body = graph_api(k, path);
            match body {
                Ok(b) => ("200 OK", "application/json", b),
                Err(e) => ("500 Internal Server Error", "text/plain", e),
            }
        }
        _p if route.starts_with("/api/schema") => match schema_endpoint(k) {
            Ok(b) => ("200 OK", "application/json", b),
            Err(e) => (
                "500 Internal Server Error",
                "application/json",
                json!({"error": e, "code": "INTERNAL"}).to_string(),
            ),
        },
        _p if route.starts_with("/api/explain") => {
            let query = parse_query_param(path, "query");
            match explain_endpoint(&query) {
                Ok(b) => ("200 OK", "application/json", b),
                Err(e) => (
                    "400 Bad Request",
                    "application/json",
                    json!({"error": e, "code": "PARSE_ERROR"}).to_string(),
                ),
            }
        }
        _p if route.starts_with("/api/aikoql") => {
            let session = validate_token(token.as_deref(), sessions);
            if let Some(session) = session {
                let query = parse_query_param(path, "query");
                let tenant = {
                    let t = parse_query_param(path, "tenant");
                    if t.is_empty() {
                        None
                    } else {
                        Some(t)
                    }
                };
                // R9: scope the REST subject to the requested tenant — the
                // kernel confines every read/write to that tenant's objects.
                let session = match &tenant {
                    Some(t) => session.in_tenant(t),
                    None => session,
                };
                let tenant_deref = tenant.as_deref();
                match aikoql_endpoint(k, &query, &session, tenant_deref, ontology) {
                    Ok(b) => ("200 OK", "application/json", b),
                    Err(e) => (
                        "400 Bad Request",
                        "application/json",
                        json!({"error": e}).to_string(),
                    ),
                }
            } else {
                (
                    "401 Unauthorized",
                    "application/json",
                    json!({"error": "login required"}).to_string(),
                )
            }
        }
        _ => ("404 Not Found", "text/plain", "Not Found\n".into()),
    };

    let mut resp = format!(
        "HTTP/1.0 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
        status,
        content_type,
        body.len(),
    );
    // CORS headers on all responses.
    for (k, v) in api_rest::cors_headers() {
        resp.push_str(&format!("{}: {}\r\n", k, v));
    }
    resp.push_str("\r\n");
    resp.push_str(&body);
    let _ = stream.write_all(resp.as_bytes());
}

pub(crate) fn prometheus_metrics(k: &Kernel) -> String {
    // R4: a storage failure must not render as "0 objects" — it is logged per
    // scrape and surfaced via the aikoql_metrics_error gauge.
    let mut metrics_error = 0u8;
    let (seq, _) = k.journal_head().unwrap_or_else(|e| {
        eprintln!("metrics: journal_head: {}", e);
        metrics_error = 1;
        (0, [0u8; 32])
    });
    let heads = k.scan_heads().unwrap_or_else(|e| {
        eprintln!("metrics: scan_heads: {}", e);
        metrics_error = 1;
        Vec::new()
    });
    let active = heads
        .iter()
        .filter(|(_, _, _, s)| *s != LifecycleState::Deleted)
        .count();
    let uptime = SERVER_START
        .get()
        .map(|s| s.elapsed().as_secs_f64())
        .unwrap_or(0.0);

    format!(
        "# HELP aikoql_journal_seq Monotonically increasing journal sequence number.\n\
         # TYPE aikoql_journal_seq counter\n\
         aikoql_journal_seq {}\n\
         # HELP aikoql_objects_total Total committed head objects.\n\
         # TYPE aikoql_objects_total gauge\n\
         aikoql_objects_total {}\n\
         # HELP aikoql_objects_active Active (non-deleted) head objects.\n\
         # TYPE aikoql_objects_active gauge\n\
         aikoql_objects_active {}\n\
         # HELP aikoql_uptime_seconds Server uptime in seconds.\n\
         # TYPE aikoql_uptime_seconds gauge\n\
         aikoql_uptime_seconds {:.1}\n\
         # HELP aikoql_metrics_error 1 if a store read failed during scrape.\n\
         # TYPE aikoql_metrics_error gauge\n\
         aikoql_metrics_error {}\n",
        seq,
        heads.len(),
        active,
        uptime,
        metrics_error
    )
}

// ---------------------------------------------------------------------------
// R7: metrics endpoint spawn, moved from main().
// ---------------------------------------------------------------------------

/// Spawn the Prometheus metrics HTTP endpoint on its own thread.
pub(crate) fn spawn_metrics(
    kernel: Arc<Kernel>,
    ontology: Arc<OntologyRegistry>,
    addr: String,
    db_path: Arc<String>,
) {
    info!(addr = %addr, "metrics HTTP server started");
    thread::spawn(move || serve_metrics(kernel, ontology, &addr, &db_path));
}
