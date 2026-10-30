//! HTTP surface: REST/graph endpoints, login/sessions, Prometheus metrics,
//! and the metrics listener.
//! Extracted from main.rs (R7 modularization). No behavior changes.

use crate::helpers::*;
use crate::tools::*;
use crate::{
    api_rest, error, graph_ui, info, json, studio, thread, Arc, Direction, ExtensionMap,
    GraphEngineApi, HashMap, HashSet, HttpSession, Instant, Kernel, KnowledgeContext,
    KnowledgeObject, LifecycleState, Metadata, Mutex, OnceLock, OntologyDef, OntologyRegistry,
    Origin, PropertyMap, Read, ReferentialPolicy, RememberRequest, SchemaRegistry, Subject,
    TcpListener, TcpStream, TraverseQuery, Value, Write, J, KOID, ONTOLOGY_TYPE, SERVER_START,
};
use std::sync::RwLock;

// ---- S-8 (POC-4): hot-reloadable compile-time ontology ----

/// The process-wide ontology slot: the startup load, the studio's
/// Auto-Discover, and REST Ontology remembers all republish through here;
/// `aikoql_endpoint` prefers it over the startup snapshot it is handed.
static ONTOLOGY_SLOT: OnceLock<RwLock<Arc<OntologyRegistry>>> = OnceLock::new();

pub(crate) fn install_ontology(reg: Arc<OntologyRegistry>) {
    match ONTOLOGY_SLOT.get() {
        Some(slot) => {
            // justified: RwLock poison is unrecoverable
            *slot.write().unwrap() = reg;
        }
        None => {
            let _ = ONTOLOGY_SLOT.set(RwLock::new(reg));
        }
    }
}

pub(crate) fn current_ontology() -> Option<Arc<OntologyRegistry>> {
    ONTOLOGY_SLOT.get().map(|slot| slot.read().unwrap().clone()) // justified: poison is unrecoverable
}

/// Load the active ontology from stored Ontology KOs (MRFC-0041).
/// Prefer manually-curated ontologies over auto-discovered ones.
/// If multiple exist, pick the latest non-discovered, fall back to discovered.
pub(crate) fn load_ontology_registry(k: &Kernel) -> Arc<OntologyRegistry> {
    let subj = Subject::with_roles("system", &["admin"]);
    match k.scan_by_type(&subj, ONTOLOGY_TYPE) {
        Ok(kos) if !kos.is_empty() => {
            let manual: Vec<_> = kos
                .iter()
                .filter(|ko| !ko.metadata.tags.contains(&"auto-discovered".to_string()))
                .collect();
            let candidate = if !manual.is_empty() {
                // Prefer the latest manually-created ontology.
                manual
                    .into_iter()
                    .max_by_key(|ko| ko.commit_ts)
                    .expect("manual is non-empty") // justified: guarded by !manual.is_empty() above
            } else {
                // Fall back to latest auto-discovered.
                kos.iter()
                    .max_by_key(|ko| ko.commit_ts)
                    .expect("kos is non-empty") // justified: guarded by the outer Ok(kos) if !kos.is_empty() arm
            };
            match OntologyDef::from_ko(candidate) {
                Ok(def) => {
                    let source = if candidate
                        .metadata
                        .tags
                        .contains(&"auto-discovered".to_string())
                    {
                        "auto-discovered"
                    } else {
                        "manual"
                    };
                    info!(namespace = %def.namespace, version = %def.version,
                          classes = def.classes.len(),
                          mappings = def.mappings.len(),
                          source = source,
                          "ontology loaded");
                    match OntologyRegistry::new(def) {
                        Ok(r) => Arc::new(r),
                        Err(e) => {
                            info!(error = %e, "ontology registry failed to initialize — using empty registry");
                            Arc::new(OntologyRegistry::empty())
                        }
                    }
                }
                Err(e) => {
                    info!(error = %e, "ontology KO found but failed to decode — using empty registry");
                    Arc::new(OntologyRegistry::empty())
                }
            }
        }
        _ => {
            info!("no ontology KO found — using empty registry");
            Arc::new(OntologyRegistry::empty())
        }
    }
}

pub(crate) fn serve_metrics(
    kernel: Arc<Kernel>,
    ontology: Arc<OntologyRegistry>,
    addr: &str,
    db_path: &Arc<String>,
    rate_limit: Arc<Mutex<crate::rate_limiter::RateLimiter>>,
    auth: Arc<AuthResolver>,
    admin: Option<Arc<dyn aikoql_storage_v2::engine::StorageAdminApi>>,
    tls: Option<Arc<rustls::ServerConfig>>,
    mcp: Option<Arc<crate::mcp_http::McpHttp>>,
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
                let rl = rate_limit.clone();
                let auth = auth.clone();
                let admin = admin.clone();
                let tls = tls.clone();
                let mcp = mcp.clone();
                std::thread::spawn(move || match tls {
                    None => handle_http(
                        &mut s,
                        &k,
                        &sess,
                        &db,
                        &ont,
                        &rl,
                        &auth,
                        admin.as_deref(),
                        mcp.as_deref(),
                    ),
                    Some(cfg) => {
                        let Some(mut t) = https_accept(s, cfg) else {
                            return;
                        };
                        handle_http(
                            &mut t,
                            &k,
                            &sess,
                            &db,
                            &ont,
                            &rl,
                            &auth,
                            admin.as_deref(),
                            mcp.as_deref(),
                        );
                        t.conn.send_close_notify();
                        let _ = t.flush();
                    }
                });
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

/// The TLS handshake for one HTTPS connection, bounded so a silent peer
/// cannot hold the thread; `None` (logged in debug) when it fails.
fn https_accept(
    s: TcpStream,
    cfg: Arc<rustls::ServerConfig>,
) -> Option<rustls::StreamOwned<rustls::ServerConnection, TcpStream>> {
    let mut conn = rustls::ServerConnection::new(cfg).ok()?;
    let mut s = s;
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    while conn.is_handshaking() {
        if let Err(e) = conn.complete_io(&mut s) {
            if cfg!(debug_assertions) {
                eprintln!("https handshake: {e}");
            }
            return None;
        }
    }
    let _ = s.set_read_timeout(None);
    Some(rustls::StreamOwned::new(conn, s))
}

/// Build graph JSON: { nodes: [...], edges: [...] }.
/// Query params: ?koid=<hex> to center on a node, &detail=1 for properties.
/// Without koid, returns all heads + their outbound relationships.
pub(crate) fn graph_api(k: &Kernel, path: &str, subject: &Subject) -> Result<String, String> {
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

    // P3-M1 (auth006): the caller's session subject — never a hardcoded
    // admin. The tenant claim confines every kernel read/write.
    let browser_ctx = KnowledgeContext::from(subject.clone());

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

/// P3-M1 (§53): HTTP login credentials. Users come from [auth].users with
/// argon2id hashes (`aikoql hash-password`); with no configured users,
/// AIKOQL_ADMIN_PASSWORD bootstraps a single admin at serve start. Neither
/// present → is_configured() is false and every login fails (fail-closed,
/// no hardcoded credentials anywhere).
pub(crate) struct AuthResolver {
    users: Vec<crate::config::AuthUser>,
    ttl_secs: u64,
    /// per configured login: (consecutive failures, locked until)
    fails: Mutex<HashMap<String, (u32, Instant)>>,
}

/// Wrong passwords a login gets before the backoff starts.
const FREE_FAILS: u32 = 5;
/// The longest backoff, in seconds.
const MAX_BACKOFF_SECS: u64 = 900;

impl AuthResolver {
    pub(crate) fn new(
        users: Vec<crate::config::AuthUser>,
        admin_password: Option<&str>,
        ttl_secs: u64,
    ) -> Self {
        let mut users = users;
        if let Some(pw) = admin_password {
            if !users.iter().any(|u| u.username == "admin") {
                use argon2::password_hash::PasswordHasher;
                let salt = argon2::password_hash::SaltString::generate(
                    &mut argon2::password_hash::rand_core::OsRng,
                );
                match argon2::Argon2::default().hash_password(pw.as_bytes(), &salt) {
                    Ok(h) => users.push(crate::config::AuthUser {
                        username: "admin".into(),
                        hash: h.to_string(),
                        roles: vec!["admin".into()],
                    }),
                    Err(e) => eprintln!("admin password hash failed: {e}"),
                }
            }
        }
        AuthResolver {
            users,
            ttl_secs,
            fails: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn is_configured(&self) -> bool {
        !self.users.is_empty()
    }

    pub(crate) fn ttl_secs(&self) -> u64 {
        self.ttl_secs
    }

    /// argon2id-verify `password` against the configured hash for
    /// `username`; the user's roles on match.
    /// A configured login's current roles (None: no such login) — what an
    /// OAuth grant carries at each check, so removing a login revokes it.
    pub(crate) fn roles_of(&self, username: &str) -> Option<Vec<String>> {
        self.users
            .iter()
            .find(|u| u.username == username)
            .map(|u| u.roles.clone())
    }

    /// Seconds `username` must wait before its next sign-in attempt.
    pub(crate) fn locked_for(&self, username: &str) -> Option<u64> {
        // justified: Mutex poison is unrecoverable
        let fails = self.fails.lock().unwrap();
        let left = fails
            .get(username)?
            .1
            .checked_duration_since(Instant::now())?;
        Some(left.as_secs() + u64::from(left.subsec_nanos() > 0))
    }

    /// Refused while `username` is backing off (locked_for); a wrong
    /// password extends the backoff, a right one clears it.
    pub(crate) fn verify(&self, username: &str, password: &str) -> Option<Vec<String>> {
        use argon2::password_hash::PasswordVerifier;
        let u = self.users.iter().find(|u| u.username == username)?;
        if self.locked_for(username).is_some() {
            return None;
        }
        let ok = argon2::PasswordHash::new(&u.hash).is_ok_and(|parsed| {
            argon2::Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        });
        // justified: Mutex poison is unrecoverable
        let mut fails = self.fails.lock().unwrap();
        if ok {
            fails.remove(username);
            return Some(u.roles.clone());
        }
        let (n, until) = fails
            .entry(username.to_string())
            .or_insert((0, Instant::now()));
        *n += 1;
        if *n >= FREE_FAILS {
            let wait = 1u64
                .checked_shl(*n - FREE_FAILS)
                .unwrap_or(u64::MAX)
                .min(MAX_BACKOFF_SECS);
            *until = Instant::now() + std::time::Duration::from_secs(wait);
        }
        None
    }
}

pub(crate) fn handle_login(
    body: &str,
    sessions: &Mutex<HashMap<String, HttpSession>>,
    auth: &AuthResolver,
) -> Result<String, (&'static str, String)> {
    let unauthorized = |e: String| ("401 Unauthorized", e);
    let creds: J =
        serde_json::from_str(body).map_err(|e| unauthorized(format!("bad JSON: {}", e)))?;
    let username = creds.get("username").and_then(|v| v.as_str()).unwrap_or("");
    let password = creds.get("password").and_then(|v| v.as_str()).unwrap_or("");

    if let Some(wait) = auth.locked_for(username) {
        return Err((
            "429 Too Many Requests",
            format!("too many failed sign-ins; retry in {wait} s"),
        ));
    }
    let roles = auth
        .verify(username, password)
        .ok_or_else(|| unauthorized("invalid credentials".into()))?;
    // P3-M1 (auth002): 256-bit CSPRNG session token (64 hex chars). The old
    // time+pid token was predictable; sessions are bearer credentials now.
    let mut raw = [0u8; 32];
    getrandom::getrandom(&mut raw).map_err(|e| unauthorized(format!("session token: {e}")))?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    // justified: Mutex poison is unrecoverable
    sessions.lock().unwrap().insert(
        token.clone(),
        HttpSession {
            username: username.to_string(),
            roles,
            created: Instant::now(),
            ttl_secs: auth.ttl_secs(),
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
    // P3-M1 (auth003): the session's configured TTL ([auth].session_ttl_seconds,
    // default 24h). TTL 0 = login disabled-by-expiry.
    if sess.created.elapsed().as_secs() >= sess.ttl_secs {
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
            json!({"created": r.koid.to_hex(), "version": r.version, "commit_ts": commit_ts_millis(r.commit_ts)})
                .to_string(),
        );
    }

    // Query: MATCH, TRAVERSE, etc. — ontology-aware compilation.
    // R9: compile scoped so the plan carries the caller's roles + tenant.
    let schema = SchemaRegistry::new(); // ponytail: empty registry; ontology handles resolution
                                        // S-8 (POC-4): prefer the hot-reloaded registry (published by
                                        // Auto-Discover / REST Ontology remembers) over the startup snapshot.
    let hot = current_ontology();
    let ont = hot.as_deref().unwrap_or(ontology);
    let plans = match aikoql_compiler::parser::compile_with_ontology_scoped(
        query,
        &subject.name,
        &subject.roles,
        subject.tenant.as_deref(),
        &schema,
        Some(ont),
    ) {
        Ok(plans) => plans,
        // S-7 (POC-4): the strict compile rejects a stored-but-unregistered
        // type with AIKOQL1030 (stable code, MRFC-0010 §10) while the MCP
        // tool compiles without schema validation — the studio must agree
        // with MCP on the same KB. Any other error stays fail-closed.
        Err(e) if e.starts_with("AIKOQL1030") => {
            vec![aikoql_compiler::parser::compile_scoped(
                query,
                &subject.name,
                &subject.roles,
                subject.tenant.as_deref(),
            )
            .map_err(|e| e.to_string())?]
        }
        Err(e) => return Err(e),
    };
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
                        // T-69: the row's tenant (null for tenantless) — the
                        // REST query surface mirrors the MCP tool.
                        "tenant": ko.metadata.tenant,
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
            // P5-M5: group rows carry properties only (no KO identity).
            aikoql_runtime::RowSet::Grouped(groups) => {
                for g in groups {
                    all_kos.push(json!({
                        "properties": g.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
                    }));
                }
            }
            // P5-M6: one row per pair — left object plus the right match
            // (or null on an unmatched LEFT row).
            aikoql_runtime::RowSet::Joined(pairs) => {
                for (l, r) in pairs {
                    all_kos.push(json!({
                        "koid": l.koid.to_hex(),
                        "type_name": l.metadata.type_name,
                        "version": l.version,
                        "properties": l.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>(),
                        "joined": r.map(|ro| json!({
                            "koid": ro.koid.to_hex(),
                            "type_name": ro.metadata.type_name,
                            "version": ro.version,
                            "properties": ro.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
                        }))
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

/// Percent-decoding over bytes ('+' is a space), then UTF-8 (lossy):
/// a multi-byte character decodes whole, an escape at the very end
/// decodes, and an invalid escape is kept as written. The hex digits are
/// read as bytes — a str slice there could split a character and panic.
pub(crate) fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                        continue;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

pub(crate) fn explain_endpoint(query: &str, k: &Kernel) -> Result<String, String> {
    // P5-M3: EXPLAIN shows the logical pipeline plus each operator's
    // physical strategy (qm002) — one line per op via PhysicalPlan::summary.
    let plan = aikoql_compiler::parser::compile_physical(query).map_err(|e| e.to_string())?;
    // P5-M9 (ND-08): the cost lines show the CBO's per-op estimate and the
    // statistics freshness behind the choice (EXPLAIN COST surface).
    let cost = aikoql_runtime::cbo::explain_cost(k, query).map_err(|e| e.to_string())?;
    Ok(json!({
        "query": query,
        "operators": plan.summary(),
        "operator_count": plan.operators.len(),
        "cost": cost,
    })
    .to_string())
}

pub(crate) fn handle_http(
    stream: &mut (impl Read + Write),
    k: &Kernel,
    sessions: &Mutex<HashMap<String, HttpSession>>,
    db_path: &Arc<String>,
    ontology: &OntologyRegistry,
    rate_limit: &Mutex<crate::rate_limiter::RateLimiter>,
    auth: &AuthResolver,
    admin: Option<&dyn aikoql_storage_v2::engine::StorageAdminApi>,
    mcp: Option<&crate::mcp_http::McpHttp>,
) {
    // Headers can span reads (a slow client, or past one TCP segment): read
    // until the blank line. shortcut: headers past 64 KB are cut, raise the
    // buffer if a client ever needs more.
    let mut buf = [0u8; 65536];
    let mut n = 0;
    while n < buf.len() && !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf[n..]) {
            Ok(0) | Err(_) if n == 0 => return,
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
        // not a request line (e.g. a TLS hello on this plain port): stop
        // here, the parse below drops it, instead of waiting for headers
        if !buf[0].is_ascii_uppercase() {
            break;
        }
    }
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
    // MCP over Streamable HTTP and its OAuth endpoints: bodies are capped
    // before they are read
    if (route == "/mcp" || route.starts_with("/oauth/"))
        && content_len.is_some_and(|cl| cl > crate::mcp_http::MAX_BODY)
    {
        crate::mcp_http::write_reply(
            stream,
            (
                "413 Payload Too Large",
                vec![],
                "text/plain; charset=utf-8",
                format!(
                    "an MCP request is at most {} bytes",
                    crate::mcp_http::MAX_BODY
                ),
            ),
        );
        return;
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

    // the OAuth authorization server (discovery, registration, sign-in, tokens)
    if let Some(o) = mcp.and_then(|m| m.oauth()) {
        if let Some(reply) = o.route(method, route, path, hdr_part, &body_str) {
            crate::mcp_http::write_reply(stream, reply);
            return;
        }
    }

    if route == "/mcp" {
        if let (Some(m), "GET") = (mcp, method) {
            m.stream(hdr_part, stream); // the session's event stream, held open
            return;
        }
        let reply = match mcp {
            Some(m) => m.handle(method, hdr_part, &body_str, k, db_path, rate_limit, admin),
            None => (
                "401 Unauthorized",
                vec![("WWW-Authenticate", "Bearer".to_string())],
                "text/plain; charset=utf-8",
                "MCP over HTTP needs --tcp-token or [auth] logins (OAuth)".to_string(),
            ),
        };
        crate::mcp_http::write_reply(stream, reply);
        return;
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
            rate_limit,
            admin,
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
            // PRR-3: surface semantic readiness alongside server health.
            let sem = crate::semantic_status_snapshot();
            let body = json!({
                "status": "ok",
                "uptime_seconds": (uptime * 10.0).round() / 10.0,
                "semantic": {"state": sem.state, "detail": sem.detail},
            })
            .to_string();
            ("200 OK", "application/json", body)
        }
        "/metrics" => {
            let body = prometheus_metrics(k, admin);
            ("200 OK", "text/plain; version=0.0.4", body)
        }
        "/api/login" if method == "POST" => match handle_login(&body_str, sessions, auth) {
            Ok(token) => (
                "200 OK",
                "application/json",
                json!({"token": token}).to_string(),
            ),
            Err((status, e)) => (status, "application/json", json!({"error": e}).to_string()),
        },
        // P3-M1 (auth005/auth006): the pre-v1 route aliases are part of the
        // same surface — session required, and graph runs as the session's
        // subject (never the old hardcoded graph-browser admin).
        _p if route.starts_with("/api/graph") => match validate_token(token.as_deref(), sessions) {
            Some(s) => match graph_api(k, path, &s) {
                Ok(b) => ("200 OK", "application/json", b),
                Err(e) => ("500 Internal Server Error", "text/plain", e),
            },
            None => (
                "401 Unauthorized",
                "application/json",
                json!({"error": "login required"}).to_string(),
            ),
        },
        _p if route.starts_with("/api/schema") => {
            match validate_token(token.as_deref(), sessions) {
                Some(_) => match schema_endpoint(k) {
                    Ok(b) => ("200 OK", "application/json", b),
                    Err(e) => (
                        "500 Internal Server Error",
                        "application/json",
                        json!({"error": e, "code": "INTERNAL"}).to_string(),
                    ),
                },
                None => (
                    "401 Unauthorized",
                    "application/json",
                    json!({"error": "login required"}).to_string(),
                ),
            }
        }
        _p if route.starts_with("/api/explain") => match validate_token(token.as_deref(), sessions)
        {
            Some(_) => {
                let query = parse_query_param(path, "query");
                match explain_endpoint(&query, k) {
                    Ok(b) => ("200 OK", "application/json", b),
                    Err(e) => (
                        "400 Bad Request",
                        "application/json",
                        json!({"error": e, "code": "PARSE_ERROR"}).to_string(),
                    ),
                }
            }
            None => (
                "401 Unauthorized",
                "application/json",
                json!({"error": "login required"}).to_string(),
            ),
        },
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

pub(crate) fn prometheus_metrics(
    k: &Kernel,
    admin: Option<&dyn aikoql_storage_v2::engine::StorageAdminApi>,
) -> String {
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

    // P3-M2 (§57): storage gauges ride the same scrape; None = non-v2 backend.
    let mut storage_metrics = String::new();
    if let Some(admin) = admin {
        match admin.storage_stats() {
            Ok(s) => {
                storage_metrics = format!(
                    "# HELP aikoql_storage_wal_bytes WAL bytes appended by the storage engine.\n\
                 # TYPE aikoql_storage_wal_bytes counter\n\
                 aikoql_storage_wal_bytes {}\n\
                 # HELP aikoql_storage_segment_bytes Bytes in live segments.\n\
                 # TYPE aikoql_storage_segment_bytes gauge\n\
                 aikoql_storage_segment_bytes {}\n\
                 # HELP aikoql_storage_compaction_backlog_bytes L0 bytes awaiting compaction.\n\
                 # TYPE aikoql_storage_compaction_backlog_bytes gauge\n\
                 aikoql_storage_compaction_backlog_bytes {}\n",
                    s.write.wal_bytes, s.segments.bytes, s.write.compaction_backlog_bytes
                )
            }
            Err(e) => {
                eprintln!("metrics: storage_stats: {}", e);
                metrics_error = 1;
            }
        }
    }

    // MRFC-0080 R3c-2: a serving leader's replication lag rides the scrape
    if let (Some(acked), Some(admin)) = (crate::replication::LEADER.get(), admin) {
        storage_metrics.push_str(&crate::replication::metrics(acked, admin.last_seq()));
    }

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
         aikoql_metrics_error {}\n\
         {}",
        seq,
        heads.len(),
        active,
        uptime,
        metrics_error,
        storage_metrics
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
    rate_limit: Arc<Mutex<crate::rate_limiter::RateLimiter>>,
    auth: Arc<AuthResolver>,
    admin: Option<Arc<dyn aikoql_storage_v2::engine::StorageAdminApi>>,
    tls: Option<Arc<rustls::ServerConfig>>,
    mcp: Option<Arc<crate::mcp_http::McpHttp>>,
) {
    info!(addr = %addr, https = tls.is_some(), mcp_http = mcp.is_some(), "metrics HTTP server started");
    thread::spawn(move || {
        serve_metrics(
            kernel, ontology, &addr, &db_path, rate_limit, auth, admin, tls, mcp,
        )
    });
}
