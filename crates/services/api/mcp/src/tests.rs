use crate::{model_store_dir, semantic_status_snapshot, set_semantic_status, validate_listen};

// Temp db paths written by THIS test thread, swept when the thread exits
// (the main thread's destructor runs at process exit — statics are NOT
// dropped on Windows MSVC, TLS is).
thread_local! {
    static TEMP_PATHS: std::cell::RefCell<TempSweeper> =
        const { std::cell::RefCell::new(TempSweeper { paths: Vec::new() }) };
}

struct TempSweeper {
    paths: Vec<std::path::PathBuf>,
}
impl Drop for TempSweeper {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = std::fs::remove_file(p);
            let _ = std::fs::remove_dir_all(p);
            // redb sidecar next to the registered stem (`{stem}.redb.artifacts`).
            let Some(name) = p.file_name() else { continue };
            if let Ok(rd) = std::fs::read_dir(p.parent().unwrap_or(std::path::Path::new("."))) {
                let prefix = format!("{}.", name.to_string_lossy());
                for e in rd.flatten() {
                    if e.file_name().to_string_lossy().starts_with(&prefix) {
                        let _ = std::fs::remove_file(e.path());
                        let _ = std::fs::remove_dir_all(e.path());
                    }
                }
            }
        }
    }
}

fn tmp_db(tag: &str) -> String {
    let p = std::env::temp_dir().join(format!("mnemo-{tag}-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    TEMP_PATHS.with(|t| t.borrow_mut().paths.push(p.clone()));
    p.to_string_lossy().into_owned()
}
#[test]
fn model_store_dir_flag_wins() {
    let p = model_store_dir(Some("C:/tmp/models"));
    assert_eq!(p, std::path::PathBuf::from("C:/tmp/models"));
}

#[test]
fn model_store_dir_default_ends_in_aikoql_models() {
    let p = model_store_dir(None);
    let mut comps = p.components().rev();
    assert_eq!(
        comps.next().map(|c| c.as_os_str()),
        Some(std::ffi::OsStr::new("models"))
    );
    assert_eq!(
        comps.next().map(|c| c.as_os_str()),
        Some(std::ffi::OsStr::new(".aikoql"))
    );
}

#[test]
fn semantic_status_roundtrip() {
    set_semantic_status("unavailable", "no model installed");
    let s = semantic_status_snapshot();
    assert_eq!(s.state, "unavailable");
    assert_eq!(s.detail, "no model installed");
    set_semantic_status("ready", "live");
    assert_eq!(semantic_status_snapshot().state, "ready");
}

// R1 (review round 3): plaintext TCP is loopback-only — a non-loopback bind
// is rejected fail-closed (the bearer token must not travel unencrypted).

#[test]
fn listen_remote_without_tls_rejected() {
    for bad in ["0.0.0.0:9090", "192.168.1.5:9090"] {
        let err = validate_listen(bad).unwrap_err();
        assert!(
            err.contains("non-loopback"),
            "remote {bad} must be rejected, got: {err}"
        );
    }
}

#[test]
fn listen_loopback_allowed() {
    assert_eq!(validate_listen("127.0.0.1:9090").unwrap(), "127.0.0.1:9090");
    assert_eq!(validate_listen("[::1]:9090").unwrap(), "[::1]:9090");
}

#[test]
fn listen_empty_host_maps_to_loopback() {
    assert_eq!(validate_listen(":9090").unwrap(), "127.0.0.1:9090");
}

#[test]
fn listen_invalid_address_rejected() {
    assert!(validate_listen("not an address").is_err());
}
use crate::http::truncate;

#[test]
fn truncate_never_splits_multibyte_chars() {
    // 25 x 'a' + '—' (bytes 25..28) + 'zzzz' = 32 bytes. max 30 → end 27
    // lands inside the em dash and must back off to a char boundary.
    let s = "aaaaaaaaaaaaaaaaaaaaaaaaa—zzzz";
    let t = truncate(s, 30);
    assert!(t.ends_with("..."));
    assert_eq!(&t[t.len() - 4..], "a...");
}

#[test]
fn truncate_passthrough_short_strings() {
    assert_eq!(truncate("hi", 10), "hi");
}

#[test]
fn enrich_file_contains_adds_file_entities_and_relations() {
    use aikoql_ingestion::{EntityCandidate, Evidence, KnowledgeIr};
    let mut ir = KnowledgeIr {
        entities: vec![
            EntityCandidate {
                name: "graph_api".into(),
                type_hint: Some("Function".into()),
                mentions: vec![],
                confidence: 0.8,
                evidence: Evidence {
                    document_id: Some("src/main.rs".into()),
                    ..Default::default()
                },
            },
            EntityCandidate {
                name: "retry_loop".into(),
                type_hint: Some("Function".into()),
                mentions: vec![],
                confidence: 0.8,
                evidence: Evidence {
                    document_id: Some("src/main.rs".into()),
                    ..Default::default()
                },
            },
            // doc == name fallback path entity: no duplicate File entity,
            // no self-contains relation.
            EntityCandidate {
                name: "src/lib.rs".into(),
                type_hint: Some("file".into()),
                mentions: vec![],
                confidence: 0.8,
                evidence: Evidence {
                    document_id: Some("src/lib.rs".into()),
                    ..Default::default()
                },
            },
        ],
        ..Default::default()
    };
    crate::ingest::enrich_file_contains(&mut ir);
    let files: Vec<&str> = ir
        .entities
        .iter()
        .filter(|e| e.type_hint.as_deref() == Some("file"))
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(files, vec!["src/lib.rs", "src/main.rs"]);
    let contains: Vec<(&str, &str, &str)> = ir
        .relations
        .iter()
        .map(|r| (r.subject.as_str(), r.predicate.as_str(), r.object.as_str()))
        .collect();
    assert_eq!(contains.len(), 2);
    assert!(contains.contains(&("src/main.rs", "contains", "graph_api")));
    assert!(contains.contains(&("src/main.rs", "contains", "retry_loop")));
}

#[test]
fn semantic_scores_parses_caches_and_scores() {
    // Regression check for the EMB_CACHE self-deadlock: the cache-insert
    // branch used to re-lock the mutex it already held via a match
    // scrutinee temporary, wedging the first request (and every request
    // after it) forever. This test walks both branches: parse+insert,
    // then cache-hit.
    let db = tmp_db("sem");
    let _ = std::fs::remove_file(&db);
    let engine = crate::RedbEngine::open(&db).expect("open store");
    let k = crate::Kernel::open(
        std::sync::Arc::new(engine),
        std::sync::Arc::new(crate::SystemClock),
        0,
    )
    .expect("open kernel");

    let mut props = crate::PropertyMap::new();
    props.insert(
        "entity_embeddings".into(),
        crate::Value::Text(r#"{"a::b":[1.0,0.0]}"#.into()),
    );
    let r = k
        .remember(crate::RememberRequest {
            context: crate::KnowledgeContext::from(&crate::Subject::with_roles("test", &["admin"])),
            koid: None,
            expected_version: Some(0),
            idempotency_key: Some("sem-scores-test".into()),
            metadata: crate::Metadata {
                type_name: "aikoql:ingested-directory".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: crate::ExtensionMap::new(),
            origin: crate::Origin::Human,
            note: None,
            referential_policy: crate::ReferentialPolicy::Permissive,
        })
        .expect("remember");
    let args = serde_json::json!({"koid": r.koid.to_hex(), "subject": "test", "roles": ["admin"]});

    let scores = crate::tools::semantic_scores(&k, &args, &[1.0, 0.0]).expect("scores");
    assert!((scores["a::b"] - 1.0).abs() < 1e-6);
    let cached = crate::tools::semantic_scores(&k, &args, &[1.0, 0.0]).expect("cached hit");
    assert_eq!(cached.len(), 1);

    drop(k);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn snapshot_manifest_props_carry_source_revision() {
    use crate::ingest::snapshot_manifest_props;
    use crate::Value;
    let with_rev = aikoql_ingestion::KnowledgeIr {
        source_revision: Some("abc123def".into()),
        ..Default::default()
    };
    let props = snapshot_manifest_props(&with_rev, "E:/repo", "{}".into(), "{}".into());
    assert_eq!(
        props.get("source_revision"),
        Some(&Value::Text("abc123def".into())),
        "revision must be a first-class snapshot property"
    );
    assert_eq!(
        props.get("source_path"),
        Some(&Value::Text("E:/repo".into()))
    );
    assert_eq!(props.get("entity_count"), Some(&Value::Int(0)));

    let without_rev = aikoql_ingestion::KnowledgeIr::default();
    let props = snapshot_manifest_props(&without_rev, "E:/repo", "{}".into(), "{}".into());
    assert!(
        !props.contains_key("source_revision"),
        "non-git ingest must omit the revision column"
    );
}

/// PRG-007: execution_id makes execute_program exactly-once — replays with
/// the same id return the stored result without re-running, and the journal
/// (aikoql:execution records) holds exactly one record per id.
#[test]
fn execute_program_idempotency_execution_id_replays() {
    let db = tmp_db("prg7");
    let _ = std::fs::remove_file(&db);
    let k = crate::Kernel::open(
        std::sync::Arc::new(crate::RedbEngine::open(&db).expect("open store")),
        std::sync::Arc::new(crate::SystemClock),
        0,
    )
    .expect("open kernel");
    let subject = crate::Subject::with_roles("test", &["admin"]);

    // Seed: two facts the program can filter on.
    for name in ["Alice", "Bob"] {
        let mut req = crate::RememberRequest::create(
            subject.clone(),
            crate::Metadata {
                type_name: "Doc".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
        );
        req.properties
            .insert("name".into(), crate::Value::Text(name.into()));
        k.remember(req).expect("seed fact");
    }

    // Deploy a parameterized program.
    let prog = crate::tools::tool_deploy_program(
        &k,
        &serde_json::json!({
            "subject": "test", "roles": ["admin"],
            "name": "FindDoc",
            "body": "MATCH Doc WHERE name == \"{{who}}\" RETURN *",
            "language": "aikoql"
        }),
    )
    .expect("deploy");
    let prog_koid = prog["koid"].as_str().unwrap().to_string();

    // First execution with an execution_id.
    let exec1 = crate::tools::tool_execute_program(
        &k,
        &serde_json::json!({
            "subject": "test", "roles": ["admin"],
            "koid": &prog_koid, "params": {"who": "Alice"}, "execution_id": "exec-1"
        }),
    )
    .expect("exec1");
    assert_eq!(exec1["count"], 1);
    assert_eq!(exec1["results"][0]["properties"]["name"], "Alice");

    // Same execution_id, different params: replay — must return the stored
    // result of the first run, proving the program was not re-run.
    let replay = crate::tools::tool_execute_program(
        &k,
        &serde_json::json!({
            "subject": "test", "roles": ["admin"],
            "koid": &prog_koid, "params": {"who": "Bob"}, "execution_id": "exec-1"
        }),
    )
    .expect("replay");
    assert_eq!(replay, exec1);

    // A new execution_id runs again.
    let exec2 = crate::tools::tool_execute_program(
        &k,
        &serde_json::json!({
            "subject": "test", "roles": ["admin"],
            "koid": &prog_koid, "params": {"who": "Bob"}, "execution_id": "exec-2"
        }),
    )
    .expect("exec2");
    assert_eq!(exec2["count"], 1);
    assert_eq!(exec2["results"][0]["properties"]["name"], "Bob");

    // Journal: exactly one record per id, carrying the FIRST run's params —
    // the replay did not overwrite it (the write committed exactly once).
    let (rec1_koid, _, _) = k
        .resolve_idempotency(&format!("execute-program-{prog_koid}-exec-1"))
        .expect("resolve")
        .expect("exec-1 record");
    let (rec2_koid, _, _) = k
        .resolve_idempotency(&format!("execute-program-{prog_koid}-exec-2"))
        .expect("resolve")
        .expect("exec-2 record");
    assert_ne!(rec1_koid, rec2_koid);
    let rec1 = k
        .get(crate::KnowledgeContext::from(&subject), &rec1_koid)
        .expect("get exec-1 record");
    assert_eq!(rec1.metadata.type_name, "aikoql:execution");
    assert_eq!(
        rec1.properties.get("program"),
        Some(&crate::Value::Text(prog_koid.clone()))
    );
    assert!(matches!(
        rec1.properties.get("params"),
        Some(crate::Value::Text(s)) if s.contains("\"Alice\"")
    ));

    drop(k);
    let _ = std::fs::remove_file(&db);
}

// ---------------------------------------------------------------------------
// P3-M1 (§53–55): auth surface — RED trio against today's behavior
// ---------------------------------------------------------------------------

/// auth002 RED: the session token must be a 256-bit CSPRNG hex string
/// (64 chars). Today it is `{:x}{:x}` time-nanos + pid — short, predictable,
/// and structurally derivable from the process start time.
#[test]
fn auth002_session_token_256bit_unpredictable() {
    let auth = crate::http::AuthResolver::new(vec![], Some("admin"), 86_400);
    let sessions = crate::Mutex::new(crate::HashMap::new());
    let body = serde_json::json!({"username": "admin", "password": "admin"}).to_string();
    let t1 = crate::http::handle_login(&body, &sessions, &auth).expect("login");
    let t2 = crate::http::handle_login(&body, &sessions, &auth).expect("login");
    assert_eq!(t1.len(), 64, "token must be 256-bit hex (32 bytes)");
    assert!(
        t1.chars().all(|c| c.is_ascii_hexdigit()),
        "token must be hex-encoded"
    );
    assert_ne!(t1, t2, "two logins must never share a token");
}

/// auth005 RED: every route outside the pinned allowlist (openapi.json,
/// abi-version, metrics-info — health/metrics/login live outside route_v1)
/// must refuse an unauthenticated caller. Today 11 arms serve anonymously.
#[test]
fn auth005_route_matrix_unauthenticated_401() {
    let db = tmp_db("auth5");
    let _ = std::fs::remove_file(&db);
    let k = crate::Kernel::open(
        std::sync::Arc::new(crate::RedbEngine::open(&db).expect("open store")),
        std::sync::Arc::new(crate::SystemClock),
        0,
    )
    .expect("open kernel");
    let sessions = crate::Mutex::new(crate::HashMap::new());
    let rl = crate::Mutex::new(crate::rate_limiter::RateLimiter::new(true, 100_000));

    // Allowlist stays open — any status except the auth failure counts.
    for (method, path) in [
        ("GET", "/api/v1/openapi.json"),
        ("GET", "/api/v1/abi-version"),
        ("GET", "/api/v1/metrics-info"),
    ] {
        let (status, _, _) =
            crate::api_rest::route_v1(method, path, "", &k, &db, &sessions, None, &rl);
        assert!(
            !status.starts_with("401"),
            "{method} {path} must stay on the allowlist, got {status}"
        );
    }

    // Everything else must 401 without a session.
    for (method, path) in [
        ("GET", "/api/v1/audit"),
        ("GET", "/api/v1/backups"),
        ("POST", "/api/v1/discover-ontology"),
        ("GET", "/api/v1/schema"),
        ("GET", "/api/v1/graph"),
        ("POST", "/api/v1/backup"),
        ("POST", "/api/v1/restore"),
        ("POST", "/api/v1/verify-backup"),
        ("POST", "/api/v1/remember"),
        ("GET", "/api/v1/get/deadbeef"),
        ("POST", "/api/v1/aikoql"),
        ("POST", "/api/v1/documents"),
        ("POST", "/api/v1/agent/memory-search"),
    ] {
        let (status, _, body) =
            crate::api_rest::route_v1(method, path, "", &k, &db, &sessions, None, &rl);
        assert!(
            status.starts_with("401"),
            "{method} {path} unauthenticated must 401, got {status}: {body}"
        );
    }

    drop(k);
    let _ = std::fs::remove_file(&db);
}

/// auth008 (regression): the REST rate limiter keys per principal — an
/// exhausted token bucket never bleeds into other tokens or "anon".
#[test]
fn auth008_rate_limiter_keys_per_principal() {
    let mut rl = crate::rate_limiter::RateLimiter::new(true, 3);
    for _ in 0..3 {
        assert!(rl.check_at("tok-a", 1000).is_ok());
    }
    assert!(rl.check_at("tok-a", 1000).is_err(), "tok-a exhausted");
    assert!(
        rl.check_at("tok-b", 1000).is_ok(),
        "separate principal bucket"
    );
    assert!(
        rl.check_at("anon", 1000).is_ok(),
        "anonymous bucket untouched"
    );
}

/// auth001 RED: login verifies CONFIGURED credentials (argon2id) — no
/// hardcoded admin/admin. The resolver here bootstraps admin from the
/// AIKOQL_ADMIN_PASSWORD path; the second half pins [auth].users hashes.
#[test]
fn auth001_login_configured_creds_ok_wrong_401() {
    let auth = crate::http::AuthResolver::new(vec![], Some("s3cret-pw"), 86400);
    let sessions = crate::Mutex::new(crate::HashMap::new());
    let ok = serde_json::json!({"username": "admin", "password": "s3cret-pw"}).to_string();
    assert!(
        crate::http::handle_login(&ok, &sessions, &auth).is_ok(),
        "bootstrap admin must log in"
    );
    let wrong = serde_json::json!({"username": "admin", "password": "admin"}).to_string();
    assert!(
        crate::http::handle_login(&wrong, &sessions, &auth).is_err(),
        "the old hardcoded admin/admin pair must be dead"
    );
    let nobody = serde_json::json!({"username": "user", "password": "user"}).to_string();
    assert!(crate::http::handle_login(&nobody, &sessions, &auth).is_err());

    // [auth].users path: a configured hash verifies and maps to its roles.
    use argon2::password_hash::PasswordHasher;
    let salt =
        argon2::password_hash::SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    let hash = argon2::Argon2::default()
        .hash_password(b"cfg-pw", &salt)
        .expect("hash")
        .to_string();
    let auth = crate::http::AuthResolver::new(
        vec![crate::config::AuthUser {
            username: "ops".into(),
            hash,
            roles: vec!["operator".into()],
        }],
        None,
        86400,
    );
    let ok = serde_json::json!({"username": "ops", "password": "cfg-pw"}).to_string();
    let tok = crate::http::handle_login(&ok, &sessions, &auth).expect("configured login");
    let subj = crate::http::validate_token(Some(&tok), &sessions).expect("valid session");
    assert_eq!(subj.name, "ops");
    assert_eq!(subj.roles, vec!["operator".to_string()]);
}

/// auth003 RED: sessions carry the configured TTL and validate_token
/// enforces it (TTL 0 = every session already expired).
#[test]
fn auth003_session_expiry_rejects_after_ttl() {
    let auth = crate::http::AuthResolver::new(vec![], Some("pw"), 0);
    let sessions = crate::Mutex::new(crate::HashMap::new());
    let body = serde_json::json!({"username": "admin", "password": "pw"}).to_string();
    let tok = crate::http::handle_login(&body, &sessions, &auth).expect("login");
    assert!(
        crate::http::validate_token(Some(&tok), &sessions).is_none(),
        "TTL 0 must expire immediately"
    );

    let auth = crate::http::AuthResolver::new(vec![], Some("pw"), 86_400);
    let tok = crate::http::handle_login(&body, &sessions, &auth).expect("login");
    assert!(crate::http::validate_token(Some(&tok), &sessions).is_some());
}

/// auth004 RED (unit): remote HTTP requires configured credentials, and the
/// metrics listener is loopback-only unless armed — the same fail-closed
/// reasoning as --listen. (Binary-level spawn pins live in auth_surface.rs.)
#[test]
fn auth004_remote_http_refused_without_auth() {
    assert!(crate::remote_http_requires_auth(true, false).is_err());
    assert!(crate::remote_http_requires_auth(true, true).is_ok());
    assert!(crate::remote_http_requires_auth(false, false).is_ok());
    assert!(crate::validate_http_listen("0.0.0.0:9091", false).is_err());
    assert!(crate::validate_http_listen("0.0.0.0:9091", true).is_ok());
    assert!(crate::validate_http_listen("127.0.0.1:9091", false).is_ok());
    assert!(crate::validate_http_listen(":9091", false).is_ok());
    assert!(crate::validate_http_listen("nonsense", false).is_err());
}

/// auth006 RED: graph_api executes as the CALLER's subject — a
/// tenant-confined session sees nothing of another tenant's heads. A
/// hardcoded admin context (today's graph-browser) would see everything.
#[test]
fn auth006_graph_runs_as_session_subject_not_hardcoded_admin() {
    let db = tmp_db("auth6");
    let _ = std::fs::remove_file(&db);
    let k = crate::Kernel::open(
        std::sync::Arc::new(crate::RedbEngine::open(&db).expect("open store")),
        std::sync::Arc::new(crate::SystemClock),
        0,
    )
    .expect("open kernel");

    // Seed one head under tenant "acme".
    let seed = crate::Subject::with_roles("seed", &["admin"]);
    let mut req = crate::RememberRequest::create(
        seed,
        crate::Metadata {
            type_name: "Note".into(),
            tenant: Some("acme".into()),
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("body".into(), crate::Value::Text("x".into()));
    k.remember(req).expect("seed head");

    // Confined subject: either an empty graph or a confinement error —
    // never the acme head. (The pin: an internal admin ctx would see it.)
    let confined = crate::Subject {
        name: "bob".into(),
        roles: vec![],
        tenant: Some("other".into()),
    };
    // confined away is also not-hardcoded-admin
    if let Ok(s) = crate::http::graph_api(&k, "/api/graph", &confined) {
        let v: serde_json::Value = serde_json::from_str(&s).expect("graph json");
        assert_eq!(
            v["nodes"].as_array().map(|a| a.len()).unwrap_or(0),
            0,
            "confined subject must see no heads"
        );
    }

    // An unscoped admin sees the head.
    let free = crate::Subject {
        name: "alice".into(),
        roles: vec!["admin".into()],
        tenant: None,
    };
    let s = crate::http::graph_api(&k, "/api/graph", &free).expect("admin graph");
    let v: serde_json::Value = serde_json::from_str(&s).expect("graph json");
    assert!(
        v["nodes"].as_array().map(|a| a.len()).unwrap_or(0) >= 1,
        "unscoped admin must see the seeded head"
    );

    drop(k);
    let _ = std::fs::remove_file(&db);
}

// ---------------------------------------------------------------------------
// P3-M2 (met004–006) — StorageAdmin surface REDs. met004 is behavioral:
// the catalog and the role gate fail on today's code. met005/006 target the
// planned API surface (StorageAdminApi in aikoql-storage-v2, call_tool's
// admin param) — compile-level REDs, acceptable for new surface (rule 1).
// ---------------------------------------------------------------------------

/// met004 RED: the operator-gated storage admin surface — the catalog must
/// list storage_stats/storage_compact/storage_checkpoint, and the capability
/// gate admits only operator/admin. developer and auditor are denied on
/// TCP; the stdio role-less passthrough (P1-10) still holds.
#[test]
fn met004_storage_admin_tools_exist_and_are_operator_gated() {
    let names: Vec<String> = crate::tool_registry::tools_list()["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for tool in ["storage_stats", "storage_compact", "storage_checkpoint"] {
        assert!(names.contains(&tool.to_string()), "missing tool: {tool}");
    }

    use crate::authz::check_capability;
    use crate::session::TrustMode;
    let roles = |r: &[&str]| -> Vec<String> { r.iter().map(|s| s.to_string()).collect() };
    for tool in ["storage_stats", "storage_compact", "storage_checkpoint"] {
        assert!(check_capability(TrustMode::Tcp, &roles(&["operator"]), tool).is_ok());
        assert!(check_capability(TrustMode::Tcp, &roles(&["admin"]), tool).is_ok());
        assert!(
            check_capability(TrustMode::Tcp, &roles(&["developer"]), tool).is_err(),
            "developer must not run {tool}"
        );
        assert!(
            check_capability(TrustMode::Tcp, &roles(&["auditor"]), tool).is_err(),
            "auditor must not run {tool}"
        );
    }
    // Stdio role-less passthrough (review P1-10) extends to the new tools.
    assert!(check_capability(TrustMode::Stdio, &[], "storage_stats").is_ok());
}

/// met005 RED: the /metrics payload gains aikoql_storage_* series when a
/// StorageAdminApi cap is present and stays bare without one. (Compile RED:
/// StorageAdminApi and the prometheus_metrics admin param do not exist yet.)
#[test]
fn met005_metrics_carry_storage_series_only_with_cap() {
    use aikoql_kernel::storage::store::StorageEngine;
    use aikoql_storage_v2::engine::StorageAdminApi; // RED: new surface
    use aikoql_storage_v2::AikoqlStorageEngineV2;
    use std::sync::Arc;

    let dir = std::env::temp_dir().join(format!("mnemo-met005-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let e = Arc::new(AikoqlStorageEngineV2::open(dir.to_str().unwrap()).unwrap());
    let engine: Arc<dyn StorageEngine> = e.clone();
    let cap: Arc<dyn StorageAdminApi> = e.clone(); // RED: trait missing
    let k = crate::Kernel::open(engine, Arc::new(crate::SystemClock), 0).expect("open kernel");

    let bare = crate::http::prometheus_metrics(&k, None);
    assert!(
        !bare.contains("aikoql_storage_"),
        "no cap, no storage series:\n{bare}"
    );

    let with = crate::http::prometheus_metrics(&k, Some(cap.as_ref()));
    for series in [
        "aikoql_storage_wal_bytes",
        "aikoql_storage_segment_bytes",
        "aikoql_storage_compaction_backlog_bytes",
    ] {
        assert!(with.contains(series), "missing series {series} in:\n{with}");
    }

    drop(k);
    let _ = std::fs::remove_dir_all(&dir);
}

/// met006 RED: storage_compact through the MCP dispatcher returns
/// CompactStats-shaped JSON and the oracle (engine.scan) re-verifies the
/// database byte-equal afterwards; a call without the admin cap is refused,
/// never silently ignored. (Compile RED: call_tool's admin param + the
/// StorageAdminApi impl do not exist yet.)
#[test]
fn met006_storage_compact_via_mcp_returns_stats_and_preserves_data() {
    use aikoql_kernel::storage::store::{StorageEngine, WriteBatch};
    use aikoql_storage_v2::db::Config;
    use aikoql_storage_v2::engine::StorageAdminApi; // RED: new surface
    use aikoql_storage_v2::AikoqlStorageEngineV2;
    use std::sync::Arc;

    let dir = std::env::temp_dir().join(format!("mnemo-met006-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut c = Config::new(dir.clone());
    c.memtable_bytes = 512; // force flushes so compact has segments to merge
    c.l0_compact_trigger = 0;
    c.checkpoint_bytes = 0;
    let e = Arc::new(AikoqlStorageEngineV2::open_with_config(c).unwrap());
    let kernel_engine: Arc<dyn StorageEngine> = e.clone();
    let scan_engine: Arc<dyn StorageEngine> = e.clone();
    let cap: Arc<dyn StorageAdminApi> = e.clone(); // RED: trait missing

    for i in 0..5 {
        let mut b = WriteBatch::new();
        b.put(format!("k{i}").into_bytes(), vec![0x2e; 300]);
        kernel_engine.write_batch(&b).unwrap();
    }
    let k = crate::Kernel::open(kernel_engine.clone(), Arc::new(crate::SystemClock), 0)
        .expect("open kernel");
    let mut session = crate::session::McpSession::default();

    // Oracle baseline AFTER kernel open — the kernel writes its own
    // meta/type_index record on open, and only compact-induced changes may
    // differ after.
    let before: Vec<(Vec<u8>, Vec<u8>)> = scan_engine.scan(b"").unwrap();
    assert_eq!(before.len(), 6, "oracle must see all 5 keys + type_index");
    let path = dir.to_str().unwrap().to_string();

    let denied = crate::tool_registry::call_tool(
        &k,
        "storage_compact",
        &crate::json!({}),
        &path,
        &mut session,
        None,
    )
    .expect("call_tool answers (stdio passthrough)");
    assert_eq!(
        denied["isError"], true,
        "no-cap call must be refused loudly"
    );
    let denied_text = denied["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        denied_text.contains("storage admin unavailable"),
        "got: {denied_text}"
    );

    let out = crate::tool_registry::call_tool(
        &k,
        "storage_compact",
        &crate::json!({}),
        &path,
        &mut session,
        Some(cap.as_ref()),
    )
    .expect("storage_compact with cap");
    assert_eq!(out["isError"], false, "compact must succeed: {out}");
    let text = out["content"][0]["text"].as_str().unwrap_or("");
    let payload: serde_json::Value = serde_json::from_str(text).expect("compact payload is JSON");
    assert!(
        payload["segments_in"].as_u64().unwrap_or(0) >= 2,
        "CompactStats-shaped result, merged the flushed pile: {payload}"
    );

    // The oracle re-verifies the db after the admin-triggered merge.
    let after: Vec<(Vec<u8>, Vec<u8>)> = scan_engine.scan(b"").unwrap();
    assert_eq!(before, after, "compact must preserve the key-value content");

    drop((k, e, kernel_engine, scan_engine, cap));
    let _ = std::fs::remove_dir_all(&dir);
}
