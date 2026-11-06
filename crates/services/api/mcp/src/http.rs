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
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::RwLock;

// ---- S-8 (POC-4): hot-reloadable compile-time ontology ----

/// The process-wide ontology slot: the startup load, the studio's
/// Auto-Discover, and REST Ontology remembers all republish through here;
/// `aikoql_endpoint` prefers it over the startup snapshot it is handed.
static ONTOLOGY_SLOT: OnceLock<RwLock<Arc<OntologyRegistry>>> = OnceLock::new();

/// Tests that install into (or read back from) the process-wide registry slot
/// take this lock: parallel tests would otherwise overwrite each other's slot
/// between a publish and its assertion.
#[cfg(test)]
pub(crate) fn ontology_serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

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

/// Per-tenant compile registries: only tenants that have an Ontology KO of
/// their own appear here (so the map is bounded by those tenants, never by
/// the tenant strings callers send). Replaced wholesale by
/// [`refresh_ontologies`].
static TENANT_ONTOLOGIES: OnceLock<RwLock<HashMap<String, Arc<OntologyRegistry>>>> =
    OnceLock::new();

fn decode_registry(ko: &KnowledgeObject) -> Option<Arc<OntologyRegistry>> {
    let def = OntologyDef::from_ko(ko).ok()?;
    OntologyRegistry::new(def).ok().map(Arc::new)
}

/// Republish every compile registry from the stored Ontology KOs: the global
/// one and each tenant's own. Called at startup and after any ontology write.
pub(crate) fn refresh_ontologies(k: &Kernel) {
    install_ontology(load_ontology_registry(k));
    let subj = Subject::with_roles("system", &["admin"]);
    let tenants: std::collections::BTreeSet<String> = k
        .scan_by_type(&subj, ONTOLOGY_TYPE)
        .map(|kos| {
            kos.into_iter()
                .filter_map(|ko| ko.metadata.tenant)
                .collect()
        })
        .unwrap_or_default();
    let mut map = HashMap::new();
    for t in tenants {
        if let Some(reg) = active_ontology_for(k, Some(&t)).and_then(|(ko, _)| decode_registry(&ko))
        {
            map.insert(t, reg);
        }
    }
    let slot = TENANT_ONTOLOGIES.get_or_init(|| RwLock::new(HashMap::new()));
    *slot.write().unwrap() = map; // justified: RwLock poison is unrecoverable
}

/// The registry a query for `tenant` compiles against: that tenant's own
/// ontology when it has saved one, else the process-wide (untenanted) one.
/// A lookup for any other tenant string reads, and caches, nothing.
pub(crate) fn registry_for(tenant: Option<&str>) -> Option<Arc<OntologyRegistry>> {
    if let Some(t) = tenant {
        let own = TENANT_ONTOLOGIES
            .get()
            .and_then(|slot| slot.read().unwrap().get(t).cloned()); // justified: poison is unrecoverable
        if own.is_some() {
            return own;
        }
    }
    current_ontology()
}

/// How many per-tenant registries are held (bounded by tenants that have an
/// ontology — never by the tenant strings callers supply).
#[cfg(test)]
pub(crate) fn tenant_registry_count() -> usize {
    TENANT_ONTOLOGIES
        .get()
        .map_or(0, |slot| slot.read().unwrap().len()) // justified: poison is unrecoverable
}

/// The ontology the compiler uses, and the one Studio shows: the newest
/// *manual* untenanted Ontology KO, else the newest auto-discovered one. A
/// tenant's own ontology is never the process-wide one. Returns the KO and
/// its source ("manual" | "auto-discovered").
pub(crate) fn active_ontology(k: &Kernel) -> Option<(KnowledgeObject, &'static str)> {
    active_ontology_for(k, None)
}

/// The same selection within one scope: `None` = the untenanted (global)
/// ontologies, `Some(t)` = tenant `t`'s own.
pub(crate) fn active_ontology_for(
    k: &Kernel,
    tenant: Option<&str>,
) -> Option<(KnowledgeObject, &'static str)> {
    let subj = Subject::with_roles("system", &["admin"]);
    let scoped: Vec<KnowledgeObject> = k
        .scan_by_type(&subj, ONTOLOGY_TYPE)
        .ok()?
        .into_iter()
        .filter(|ko| ko.metadata.tenant.as_deref() == tenant)
        .collect();
    let auto = |ko: &KnowledgeObject| ko.metadata.tags.iter().any(|t| t == "auto-discovered");
    let newest = |manual: bool| {
        scoped
            .iter()
            .filter(|ko| auto(ko) != manual)
            .max_by_key(|ko| ko.commit_ts)
            .cloned()
    };
    newest(true)
        .map(|ko| (ko, "manual"))
        .or_else(|| newest(false).map(|ko| (ko, "auto-discovered")))
}

pub(crate) fn load_ontology_registry(k: &Kernel) -> Arc<OntologyRegistry> {
    let Some((candidate, source)) = active_ontology(k) else {
        info!("no ontology KO found — using empty registry");
        return Arc::new(OntologyRegistry::empty());
    };
    match OntologyDef::from_ko(&candidate) {
        Ok(def) => {
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
    // Logged only once the socket is bound and listening — the "started" line
    // in spawn_metrics precedes the bind, so it cannot gate a client.
    info!(addr = %addr, "metrics HTTP server listening");
    let sessions: Arc<Mutex<HashMap<String, HttpSession>>> = Arc::new(Mutex::new(HashMap::new()));
    // Enterprise (Task 5): the serving handle on the user store main.rs
    // opened (users.db); None in community mode.
    crate::session::open_users(db_path.as_str());
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
                // AG-52: the peer for the login per-IP layer (before the
                // TLS wrap consumes the stream).
                let peer_ip = s
                    .peer_addr()
                    .map(|a| a.ip())
                    .unwrap_or(IpAddr::from([0, 0, 0, 0]));
                std::thread::spawn(move || match tls {
                    None => handle_http(
                        &mut s,
                        peer_ip,
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
                            peer_ip,
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
    // From Authorization header: Bearer abc123 (name and scheme are
    // case-insensitive, RFC 9110 — browsers and proxies vary the casing)
    for line in req.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.eq_ignore_ascii_case("authorization") {
            continue;
        }
        let value = value.trim();
        if value.len() > 7 && value[..7].eq_ignore_ascii_case("bearer ") {
            return Some(value[7..].trim().to_string());
        }
    }
    session_cookie(req)
}

const SESSION_COOKIE: &str = "aikoql_session";

/// The Studio session cookie value, if the request carries one.
fn session_cookie(req: &str) -> Option<String> {
    req.lines()
        .filter_map(|l| l.split_once(':'))
        .filter(|(n, _)| n.eq_ignore_ascii_case("cookie"))
        .flat_map(|(_, v)| v.split(';'))
        .find_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k == SESSION_COOKIE && !v.is_empty()).then(|| v.to_string())
        })
}

fn has_header(req: &str, name: &str) -> bool {
    req.lines()
        .filter_map(|l| l.split_once(':'))
        .any(|(n, _)| n.eq_ignore_ascii_case(name))
}

/// CSRF guard: a write whose only credential is the ambient session cookie
/// must carry the custom `X-Aikoql-Csrf` header. A cross-site page cannot
/// add a custom header without a CORS preflight (which this server answers
/// without credentials support), and SameSite=Strict already withholds the
/// cookie from cross-site requests; this is the second layer.
fn cookie_write_without_csrf(method: &str, path: &str, req: &str) -> bool {
    !matches!(method, "GET" | "HEAD" | "OPTIONS")
        // login is credential-based and ignores any cookie: a stale one must
        // not lock the sign-in form out
        && path.split('?').next() != Some("/api/login")
        && !path.contains("token=")
        && !has_header(req, "authorization")
        && session_cookie(req).is_some()
        && !has_header(req, "x-aikoql-csrf")
}

/// `Set-Cookie` for a session (`ttl` seconds) or, with `ttl` 0, its removal.
/// `Secure` only behind a TLS-terminating proxy that says so.
fn session_cookie_header(token: &str, ttl: u64, req: &str) -> String {
    let secure = req.lines().filter_map(|l| l.split_once(':')).any(|(n, v)| {
        n.eq_ignore_ascii_case("x-forwarded-proto") && v.trim().eq_ignore_ascii_case("https")
    });
    format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={ttl}{}",
        if secure { "; Secure" } else { "" }
    )
}

/// P3-M1 (§53): HTTP login credentials. Users come from [auth].users with
/// argon2id hashes (`aikoql hash-password`); with no configured users,
/// AIKOQL_ADMIN_PASSWORD bootstraps a single admin at serve start. Neither
/// present → is_configured() is false and every login fails (fail-closed,
/// no hardcoded credentials anywhere).
pub(crate) struct AuthResolver {
    users: Vec<crate::config::AuthUser>,
    /// The AIKOQL_ADMIN_PASSWORD bootstrap, kept apart from `users`: it arms
    /// is_configured() (and so the OAuth surface), but enterprise break-glass
    /// consults config users only — the env password just seeds the store.
    admin: Option<crate::config::AuthUser>,
    ttl_secs: u64,
    /// per configured login: (consecutive failures, locked until unix ms)
    fails: Mutex<HashMap<String, (u32, i64)>>,
    /// AG-52: per source-IP failures, same doubling shape — a username
    /// spray can't dodge the per-login backoff. In-memory only (a restart
    /// resets it; the per-login state persists).
    ips: Mutex<HashMap<IpAddr, (u32, i64)>>,
    /// AG-52: X-Forwarded-For is trusted only from this peer.
    trusted_proxy: Option<IpAddr>,
    /// AG-52: the login-backoff state file (survives restarts). None = the
    /// in-memory behavior (tests, embedded use).
    state_path: Option<PathBuf>,
}

/// Wrong passwords a login gets before the backoff starts.
const FREE_FAILS: u32 = 5;
/// Wrong passwords one address gets before ITS backoff starts (an address
/// is shared — NAT — so the free allowance is wider).
pub(crate) const FREE_IP_FAILS: u32 = 20;
/// The longest backoff, in seconds.
const MAX_BACKOFF_SECS: u64 = 900;

/// The doubling backoff the login and per-IP layers share: failure n
/// (1-based, past the free allowance) waits 2^(n-free) s, capped.
fn backoff_secs(n: u32, free: u32) -> u64 {
    1u64.checked_shl(n.saturating_sub(free))
        .unwrap_or(u64::MAX)
        .min(MAX_BACKOFF_SECS)
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl AuthResolver {
    /// Test-only convenience (AG-52's `with_state` is the serve path);
    /// cfg(test) so the bin build does not flag it dead
    #[cfg(test)]
    pub(crate) fn new(
        users: Vec<crate::config::AuthUser>,
        admin_password: Option<&str>,
        ttl_secs: u64,
    ) -> Self {
        Self::with_state(users, admin_password, ttl_secs, None)
    }

    /// AG-52: `state_path` persists the fail map next to the database, so a
    /// serve restart keeps its login backoffs; None keeps the in-memory
    /// behavior.
    pub(crate) fn with_state(
        users: Vec<crate::config::AuthUser>,
        admin_password: Option<&str>,
        ttl_secs: u64,
        state_path: Option<PathBuf>,
    ) -> Self {
        let admin = admin_password
            .filter(|_| !users.iter().any(|u| u.username == "admin"))
            .and_then(|pw| {
                use argon2::password_hash::PasswordHasher;
                let salt = argon2::password_hash::SaltString::generate(
                    &mut argon2::password_hash::rand_core::OsRng,
                );
                match argon2::Argon2::default().hash_password(pw.as_bytes(), &salt) {
                    Ok(h) => Some(crate::config::AuthUser {
                        username: "admin".into(),
                        hash: h.to_string(),
                        roles: vec!["admin".into()],
                    }),
                    Err(e) => {
                        eprintln!("admin password hash failed: {e}");
                        None
                    }
                }
            });
        // AG-52: reload the persisted fail state; a torn or unreadable file
        // reads as no state (the backoff restarts — fail open, login never
        // blocked by a corrupted sidecar).
        let fails = state_path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<HashMap<String, (u32, i64)>>(&s).ok())
            .unwrap_or_default();
        AuthResolver {
            users,
            admin,
            ttl_secs,
            fails: Mutex::new(fails),
            ips: Mutex::new(HashMap::new()),
            trusted_proxy: None,
            state_path,
        }
    }

    /// AG-52: arm the proxy trust — set once at serve start, before the
    /// resolver is shared.
    pub(crate) fn set_trusted_proxy(&mut self, proxy: Option<IpAddr>) {
        self.trusted_proxy = proxy;
    }

    pub(crate) fn is_configured(&self) -> bool {
        !self.users.is_empty() || self.admin.is_some()
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
            .or(self.admin.as_ref().filter(|a| a.username == username))
            .map(|u| u.roles.clone())
    }

    /// AG-52: the fail state as JSON (only live locks, so the file stays
    /// small), written atomically — a crash mid-write keeps the last state.
    fn save(&self) {
        let Some(path) = &self.state_path else { return };
        // justified: Mutex poison is unrecoverable
        let fails = self.fails.lock().unwrap();
        let now = unix_ms();
        let live: HashMap<&String, &(u32, i64)> = fails
            .iter()
            .filter(|(_, (_, until))| *until > now)
            .collect();
        let Ok(json) = serde_json::to_string(&live) else {
            return;
        };
        let tmp = path.with_extension("login-backoff.tmp");
        if std::fs::write(&tmp, json).is_err() {
            return;
        }
        let _ = std::fs::rename(&tmp, path);
    }

    /// Seconds `username` must wait before its next sign-in attempt.
    pub(crate) fn locked_for(&self, username: &str) -> Option<u64> {
        // justified: Mutex poison is unrecoverable
        let fails = self.fails.lock().unwrap();
        let left = fails.get(username)?.1 - unix_ms();
        (left > 0).then_some((left as u64).div_ceil(1000))
    }

    /// Refused while `username` is backing off (locked_for); a wrong
    /// password extends the backoff, a right one clears it.
    pub(crate) fn verify(&self, username: &str, password: &str) -> Option<Vec<String>> {
        use argon2::password_hash::PasswordVerifier;
        let u = self
            .users
            .iter()
            .find(|u| u.username == username)
            .or(self.admin.as_ref().filter(|a| a.username == username))?;
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
            drop(fails);
            self.save();
            return Some(u.roles.clone());
        }
        let (n, until) = fails.entry(username.to_string()).or_insert((0, unix_ms()));
        *n += 1;
        if *n >= FREE_FAILS {
            *until = unix_ms() + (backoff_secs(*n, FREE_FAILS) * 1000) as i64;
        }
        drop(fails);
        self.save();
        None
    }

    /// argon2id-verify against the CONFIG-FILE users only. Enterprise
    /// break-glass falls back here after the store hash: a config-file admin
    /// keeps its v0.4.0 password, while the env bootstrap entry is never
    /// consulted (in enterprise the env only seeds the user store). Honors
    /// the per-login backoff; the caller counts refusals at the IP layer.
    pub(crate) fn verify_config(&self, username: &str, password: &str) -> Option<Vec<String>> {
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
        ok.then(|| u.roles.clone())
    }

    /// AG-52: seconds `ip` must wait before its next login attempt.
    pub(crate) fn ip_wait(&self, ip: IpAddr) -> Option<u64> {
        // justified: Mutex poison is unrecoverable
        let ips = self.ips.lock().unwrap();
        let left = ips.get(&ip)?.1 - unix_ms();
        (left > 0).then_some((left as u64).div_ceil(1000))
    }

    /// AG-52: record a failed login attempt from `ip` (callers count every
    /// refusal — unknown usernames included).
    pub(crate) fn ip_failure(&self, ip: IpAddr) {
        // justified: Mutex poison is unrecoverable
        let mut ips = self.ips.lock().unwrap();
        let (n, until) = ips.entry(ip).or_insert((0, unix_ms()));
        *n += 1;
        if *n >= FREE_IP_FAILS {
            *until = unix_ms() + (backoff_secs(*n, FREE_IP_FAILS) * 1000) as i64;
        }
    }

    /// AG-52: the client address for login-rate purposes — the peer,
    /// except when the peer is the configured trusted proxy, in which case
    /// the last X-Forwarded-For hop (the one the proxy appended) is the
    /// client. An unparseable entry falls back to the peer.
    pub(crate) fn client_ip(&self, peer: IpAddr, headers: &str) -> IpAddr {
        let Some(proxy) = self.trusted_proxy else {
            return peer;
        };
        if peer != proxy {
            return peer;
        }
        for line in headers.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.trim().eq_ignore_ascii_case("x-forwarded-for") {
                if let Some(last) = value.split(',').next_back().map(str::trim) {
                    if let Ok(ip) = last.parse() {
                        return ip;
                    }
                }
                break;
            }
        }
        peer
    }
}

pub(crate) fn handle_login(
    body: &str,
    sessions: &Mutex<HashMap<String, HttpSession>>,
    auth: &AuthResolver,
    client_ip: IpAddr,
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
    // AG-52: the per-IP layer sits in front of the credential check, so a
    // spray across usernames is refused before argon2 even runs.
    if let Some(wait) = auth.ip_wait(client_ip) {
        return Err((
            "429 Too Many Requests",
            format!("too many failed sign-ins from your address; retry in {wait} s"),
        ));
    }
    // Enterprise break-glass (Task 5): with multi-user on, the bootstrap
    // admin verifies against the users.db hash — AIKOQL_ADMIN_PASSWORD only
    // seeded it. Config-file users keep the AuthResolver path untouched.
    let roles = match crate::session::users() {
        Some(store) if username == crate::users::ADMIN_SUB => store
            .lock()
            .unwrap() // justified: Mutex poison is unrecoverable
            .verify_admin_password(password)
            .unwrap_or(false)
            .then(|| vec!["admin".to_string()])
            // a config-file admin ([auth].users) keeps its v0.4.0 password;
            // the env bootstrap entry is not consulted (it only seeds)
            .or_else(|| auth.verify_config(username, password)),
        _ => auth.verify(username, password),
    }
    .ok_or_else(|| {
        auth.ip_failure(client_ip);
        unauthorized("invalid credentials".into())
    })?;
    let token = mint_session(sessions, username, roles, auth.ttl_secs()).map_err(unauthorized)?;
    Ok(token)
}

/// Mint a 256-bit CSPRNG session token (64 hex chars — P3-M1 auth002) and
/// store the session.
fn mint_session(
    sessions: &Mutex<HashMap<String, HttpSession>>,
    username: &str,
    roles: Vec<String>,
    ttl_secs: u64,
) -> Result<String, String> {
    let mut raw = [0u8; 32];
    getrandom::getrandom(&mut raw).map_err(|e| format!("session token: {e}"))?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    // justified: Mutex poison is unrecoverable
    sessions.lock().unwrap().insert(
        token.clone(),
        HttpSession {
            username: username.to_string(),
            roles,
            created: Instant::now(),
            ttl_secs,
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
    let mut subject = Subject {
        name: sess.username.clone(),
        roles: sess.roles.clone(),
        tenant: None, // REST sessions carry no tenant claim; callers scope via query param
    };
    // Enterprise (Task 5): the store's roles are authoritative, per request
    // — a revocation takes effect on the next call. Users not in the store
    // (config-file logins) keep their session snapshot.
    if let Some(store) = crate::session::users() {
        // justified: Mutex poison is unrecoverable
        if let Ok(Some(roles)) = store.lock().unwrap().roles_of(&sess.username) {
            subject.roles = roles;
        }
    }
    Some(subject)
}

/// Task 6: the session's live roles for the REST gate — the user store is
/// authoritative when open (a revocation takes effect on the next call),
/// exactly as validate_token resolves them, but WITHOUT the TTL check: the
/// gate must never change a session's community-mode TTL behavior.
pub(crate) fn live_roles(
    token: Option<&str>,
    sessions: &Mutex<HashMap<String, HttpSession>>,
) -> Option<Vec<String>> {
    let token = token?;
    // justified: Mutex poison is unrecoverable
    let guard = sessions.lock().unwrap();
    let sess = guard.get(token)?;
    let mut roles = sess.roles.clone();
    if let Some(store) = crate::session::users() {
        // justified: Mutex poison is unrecoverable
        if let Ok(Some(store_roles)) = store.lock().unwrap().roles_of(&sess.username) {
            roles = store_roles;
        }
    }
    Some(roles)
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
                                        // Per-tenant: the caller's tenant (its session scope, else the requested
                                        // one) compiles against its own ontology, falling back to the global.
    let hot = registry_for(subject.tenant.as_deref().or(tenant));
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
    peer_ip: IpAddr,
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

    // AG-52: the client address for login-rate purposes — XFF only when
    // the peer is the configured trusted proxy (per-IP layer).
    let client_ip = auth.client_ip(peer_ip, hdr_part);

    // the OAuth authorization server (discovery, registration, sign-in, tokens)
    if let Some(o) = mcp.and_then(|m| m.oauth()) {
        if let Some(reply) = o.route(method, route, path, hdr_part, &body_str, client_ip) {
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

    if cookie_write_without_csrf(method, path, &req) {
        let body = json!({"error": "missing X-Aikoql-Csrf header"}).to_string();
        let mut resp = format!(
            "HTTP/1.0 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
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

    // Enterprise (Task 8): the admin API — /api/license for any
    // authenticated session; the user routes dispatch only when the
    // multi-user store is open (they are gated admin-only inside
    // route_admin with the T6 live-role check). Community mode has no
    // store: those paths fall through to the v0.4.0 404, byte-for-byte —
    // no 401, no JSON, no "user store unavailable". Task 9 adds
    // /api/audit-export under the audit-export flag, same fall-through.
    let admin_reply = if route == "/api/license"
        || ((route == "/api/users" || route.starts_with("/api/users/"))
            && crate::session::users().is_some())
    {
        let (status, ct, body) =
            api_rest::route_admin(method, route, &body_str, sessions, token.as_deref());
        Some((status, ct, Vec::new(), body))
    } else if route == "/api/audit-export"
        && crate::license::LICENSE_STATE
            .get()
            .is_some_and(|s| s.features.audit_export)
    {
        Some(api_rest::route_audit_export(
            sessions,
            token.as_deref(),
            db_path.as_str(),
        ))
    } else {
        None
    };
    if let Some((status, ct, extra_headers, body)) = admin_reply {
        let mut resp = format!(
            "HTTP/1.0 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
            status,
            ct,
            body.len()
        );
        for (h, v) in extra_headers {
            resp.push_str(&format!("{h}: {v}\r\n"));
        }
        for (h, v) in api_rest::cors_headers() {
            resp.push_str(&format!("{}: {}\r\n", h, v));
        }
        resp.push_str("\r\n");
        resp.push_str(&body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    // Enterprise SSO (Task 5): /api/login/sso starts an IdP sign-in and
    // /oidc/callback finishes it. Without an oidc_sso license + OIDC config
    // (gate() None) these 404 — the v0.4.0 surface is byte-for-byte unchanged.
    if let Some(reply) = sso_routes(method, route, path, sessions, auth, mcp) {
        crate::mcp_http::write_reply(stream, reply);
        return;
    }

    let mut set_cookie: Option<String> = None;
    let (status, content_type, body) = match route {
        "/ui" => (
            "200 OK",
            "text/html; charset=utf-8",
            graph_ui::GRAPH_UI_HTML.to_string(),
        ),
        "/" | "/studio" => (
            "200 OK",
            "text/html; charset=utf-8",
            studio::studio_html(
                crate::oidc::gate().is_some(),
                crate::license::LICENSE_STATE.get(),
            ),
        ),
        "/studio/assets/app.js" => (
            "200 OK",
            "application/javascript; charset=utf-8",
            studio::APP_JS.to_string(),
        ),
        "/studio/assets/editor.js" => (
            "200 OK",
            "application/javascript; charset=utf-8",
            studio::EDITOR_JS.to_string(),
        ),
        "/studio/assets/vis-network.js" => (
            "200 OK",
            "application/javascript; charset=utf-8",
            studio::VIS_JS.to_string(),
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
        "/api/login" if method == "POST" => {
            match handle_login(&body_str, sessions, auth, client_ip) {
                Ok(token) => {
                    set_cookie = Some(session_cookie_header(&token, auth.ttl_secs(), &req));
                    (
                        "200 OK",
                        "application/json",
                        json!({"token": token}).to_string(),
                    )
                }
                Err((status, e)) => (status, "application/json", json!({"error": e}).to_string()),
            }
        }
        // who the cookie/Bearer session is, so Studio can resume after a reload
        "/api/session" if method == "GET" => match validate_token(token.as_deref(), sessions) {
            Some(s) => (
                "200 OK",
                "application/json",
                json!({"username": s.name, "roles": s.roles}).to_string(),
            ),
            None => (
                "401 Unauthorized",
                "application/json",
                json!({"error": "login required"}).to_string(),
            ),
        },
        "/api/logout" if method == "POST" => {
            if let Some(t) = token.as_deref() {
                sessions.lock().unwrap().remove(t); // justified: Mutex poison is unrecoverable
            }
            set_cookie = Some(session_cookie_header("", 0, &req));
            (
                "200 OK",
                "application/json",
                json!({"ok": true}).to_string(),
            )
        }
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
    if let Some(c) = &set_cookie {
        resp.push_str(&format!("Set-Cookie: {c}\r\n"));
    }
    if matches!(route, "/" | "/studio") {
        for (k, v) in studio::STUDIO_HEADERS {
            resp.push_str(&format!("{}: {}\r\n", k, v));
        }
    }
    resp.push_str("\r\n");
    resp.push_str(&body);
    let _ = stream.write_all(resp.as_bytes());
}

/// Task 5: the Studio SSO entry and the IdP callback. None for any other
/// route (and for everything when OIDC is not active).
fn sso_routes(
    method: &str,
    route: &str,
    path: &str,
    sessions: &Mutex<HashMap<String, HttpSession>>,
    auth: &AuthResolver,
    mcp: Option<&crate::mcp_http::McpHttp>,
) -> Option<crate::mcp_http::Reply> {
    match (method, route) {
        ("GET", "/api/login/sso") => {
            let provider = crate::oidc::gate()?;
            let verifier = crate::oidc::pkce_verifier();
            // no resume: the callback mints a Studio session
            let state = crate::session::oidc_flows().begin(verifier.clone(), None);
            let url = match provider.authorize_url(&state, &verifier) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(error = %e, "OIDC authorize URL failed");
                    return Some((
                        "500 Internal Server Error",
                        vec![],
                        "text/plain; charset=utf-8",
                        format!("OIDC sign-in is not available: {e}\n"),
                    ));
                }
            };
            let mut h = vec![("Cache-Control", "no-store".to_string())];
            h.push(("Location", url));
            Some(("302 Found", h, "text/plain", String::new()))
        }
        ("GET", "/oidc/callback") => Some(callback(path, sessions, auth, mcp)),
        _ => None,
    }
}

/// /oidc/callback — the IdP sends the browser back here with code+state.
/// Exchange → upsert the user → either resume the interrupted MCP authorize
/// (issue its code) or mint a Studio session.
fn callback(
    path: &str,
    sessions: &Mutex<HashMap<String, HttpSession>>,
    auth: &AuthResolver,
    mcp: Option<&crate::mcp_http::McpHttp>,
) -> crate::mcp_http::Reply {
    let page = |status: &'static str, msg: String| {
        (
            status,
            vec![],
            "text/plain; charset=utf-8",
            format!("{msg}\n"),
        )
    };
    let Some(provider) = crate::oidc::gate() else {
        return page("404 Not Found", "OIDC is not configured".into());
    };
    let code = parse_query_param(path, "code");
    let state = parse_query_param(path, "state");
    let flow = match crate::session::oidc_flows().take(&state) {
        Some(f) => f,
        None => return page("400 Bad Request", "unknown or used OIDC state".into()),
    };
    let claims = match provider.exchange(&code, &flow.verifier) {
        Ok(c) => c,
        Err(e) => return page("401 Unauthorized", format!("OIDC sign-in failed: {e}")),
    };
    // Final review I-4: the bootstrap admin's sub is reserved — an IdP
    // identity colliding with it must never overwrite the break-glass row
    // (the store refuses too; this gives the clean status).
    if claims.sub == crate::users::ADMIN_SUB {
        return page(
            "403 Forbidden",
            format!(
                "the break-glass admin '{}' cannot sign in via SSO",
                crate::users::ADMIN_SUB
            ),
        );
    }
    let roles = crate::oidc::map_roles(&claims.roles);
    // the store is authoritative: first login inserts, later logins refresh
    if let Some(store) = crate::session::users() {
        let store = store.lock().unwrap(); // justified: Mutex poison is unrecoverable
        if let Err(e) = store.upsert_oidc_user(&claims.sub, claims.name.as_deref(), &roles) {
            drop(store);
            return page("500 Internal Server Error", format!("user store: {e}"));
        }
    }
    match flow.resume {
        Some(pending) => {
            // the interrupted MCP sign-in continues: issue its code
            let Some(oauth) = mcp.and_then(|m| m.oauth()) else {
                return page("404 Not Found", "OAuth is not configured".into());
            };
            oauth.finish_sso(&pending, &claims.sub)
        }
        None => {
            // Studio login: a session token, handed to the overlay via the
            // home redirect's URL fragment (fragments never reach servers,
            // proxies or Referer; the page's api() sends authToken as a
            // query param itself).
            let token = match mint_session(sessions, &claims.sub, roles, auth.ttl_secs()) {
                Ok(t) => t,
                Err(e) => return page("500 Internal Server Error", format!("session: {e}")),
            };
            let user = enc_query(&claims.sub);
            let mut h = vec![("Cache-Control", "no-store".to_string())];
            h.push(("Location", format!("/#token={token}&user={user}")));
            ("302 Found", h, "text/plain", String::new())
        }
    }
}

/// Percent-encoding for the callback's home redirect (a sub may carry
/// characters that would break the fragment).
fn enc_query(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
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
    ) + &crate::observability::prometheus_tool_metrics()
        + &crate::observability::prometheus_recall_metrics()
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

#[cfg(test)]
mod token_tests {
    use super::extract_token;

    #[test]
    fn bearer_header_name_is_case_insensitive() {
        // Header names are case-insensitive (RFC 9110); a client that sends
        // `authorization:` must authenticate like one that sends `Authorization:`.
        for name in ["Authorization", "authorization", "AUTHORIZATION"] {
            let req = format!("GET /api/x HTTP/1.1\r\nHost: h\r\n{name}: Bearer tok-1\r\n\r\n");
            assert_eq!(
                extract_token("/api/x", &req).as_deref(),
                Some("tok-1"),
                "{name}"
            );
        }
    }

    #[test]
    fn the_session_cookie_is_the_last_resort_after_header_and_query() {
        let cookie = "GET /x HTTP/1.1\r\nCookie: a=1; aikoql_session=ck-1; b=2\r\n\r\n";
        assert_eq!(extract_token("/x", cookie).as_deref(), Some("ck-1"));
        let both =
            "GET /x HTTP/1.1\r\ncookie: aikoql_session=ck-1\r\nAuthorization: Bearer hd-1\r\n\r\n";
        assert_eq!(extract_token("/x", both).as_deref(), Some("hd-1"));
        assert_eq!(extract_token("/x?token=q-1", both).as_deref(), Some("q-1"));
        let other = "GET /x HTTP/1.1\r\nCookie: aikoql_sessionx=no\r\n\r\n";
        assert_eq!(extract_token("/x", other), None);
    }

    #[test]
    fn query_token_still_works_for_existing_clients() {
        assert_eq!(
            extract_token("/api/x?token=abc", "GET /api/x?token=abc HTTP/1.0\r\n\r\n").as_deref(),
            Some("abc")
        );
    }
}
