//! Task 5: OIDC SSO — the enterprise login flow on both surfaces: the
//! Studio overlay ("Sign in with SSO" → IdP → /oidc/callback → session) and
//! MCP /oauth/authorize (a redirect to the IdP instead of the password
//! form). Every test spawns the debug binary with a TEST-keypair license and
//! AIKOQL_OIDC_MOCK=1 (the debug-only in-process IdP stand-in), drives real
//! HTTP, and reads the user store back for role assertions.

mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value as J;
use sha2::Digest as _;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// The redirect URI the tests register (loopback http is allowed).
const CALLBACK: &str = "http://127.0.0.1:9/callback";

/// A licensed oidc_sso server with the mock IdP: claims override the
/// mock's default {"sub":"mock-user","name":null,"roles":[]}.
fn serve(tag: &str, claims: Option<&str>, admin_password: &str) -> common::Server {
    let (pk, blob) = common::sign("test", None, &["oidc-sso"]);
    let pk_b64 = URL_SAFE_NO_PAD.encode(&pk);
    let lic = std::env::temp_dir().join(format!("aikoql-oidc-{tag}-{}.lic", std::process::id()));
    std::fs::write(&lic, &blob).unwrap();
    common::register_temp_path(&lic);
    let db = std::env::temp_dir().join(format!("aikoql-oidc-{tag}-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-oidc-{tag}-{}.log", std::process::id()));
    common::register_temp_path(&log);

    let lic_str = lic.to_str().unwrap().to_string();
    let mut envs = vec![
        ("AIKOQL_LICENSE".to_string(), lic_str),
        ("AIKOQL_LICENSE_PUBLIC_KEY".to_string(), pk_b64),
        ("AIKOQL_OIDC_MOCK".to_string(), "1".to_string()),
        (
            "AIKOQL_ADMIN_PASSWORD".to_string(),
            admin_password.to_string(),
        ),
    ];
    if let Some(c) = claims {
        envs.push(("AIKOQL_OIDC_MOCK_CLAIMS".to_string(), c.to_string()));
    }
    let envs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    common::spawn_server(&db, &log, &envs, &[])
}

/// Poll /health until the server answers.
fn wait_ready(srv: &common::Server) {
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(
        health.starts_with("HTTP/1.") && health.contains(" 200 "),
        "health: {health}"
    );
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> J {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {:?}", self.body))
    }
    fn location(&self) -> String {
        self.header("location")
            .expect("Location header")
            .to_string()
    }
}

/// One HTTP request; the server closes after the response.
fn http(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    s.flush().unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let raw = String::from_utf8_lossy(&out);
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        })
        .collect();
    Resp {
        status,
        headers,
        body: body.to_string(),
    }
}

fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn post_form(port: u16, path: &str, pairs: &[(&str, &str)]) -> Resp {
    let body: String = pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", urlenc(v)))
        .collect::<Vec<_>>()
        .join("&");
    http(
        port,
        "POST",
        path,
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &body,
    )
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// Register a public client; its id comes back in the 201.
fn register(port: u16) -> String {
    let r = http(
        port,
        "POST",
        "/oauth/register",
        &[("Content-Type", "application/json")],
        &format!(r#"{{"redirect_uris": ["{CALLBACK}"], "client_name": "task5-test"}}"#),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    r.json()["client_id"]
        .as_str()
        .expect("client_id")
        .to_string()
}

/// The stored roles of `sub`; None when the row or the store is absent.
fn store_roles(db: &Path, sub: &str) -> Option<Vec<String>> {
    let conn = rusqlite::Connection::open(db.join("users.db")).ok()?;
    let raw: Option<String> = conn
        .query_row("SELECT roles FROM users WHERE sub = ?1", [sub], |row| {
            row.get(0)
        })
        .ok()?;
    raw.and_then(|r| serde_json::from_str(&r).ok())
}

/// Overwrite the stored roles (the store is the authority). The server may
/// hold the file briefly — retry past SQLITE_BUSY.
fn set_roles(db: &Path, sub: &str, roles: &[&str]) {
    let conn = rusqlite::Connection::open(db.join("users.db")).expect("users.db");
    let roles_json = serde_json::to_string(roles).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match conn.execute(
            "UPDATE users SET roles = ?1 WHERE sub = ?2",
            rusqlite::params![roles_json, sub],
        ) {
            Ok(_) => return,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                assert!(Instant::now() < deadline, "users.db stayed busy");
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("set roles: {e}"),
        }
    }
}

/// The Studio SSO round trip: /api/login/sso → the IdP → /oidc/callback,
/// then the session token from the home redirect.
fn sso_token(port: u16) -> (String, String) {
    let start = http(port, "GET", "/api/login/sso", &[], "");
    assert_eq!(start.status, 302, "{}", start.body);
    let loc = start.location();
    assert!(
        loc.starts_with("https://oidc.test.invalid/authorize"),
        "{loc}"
    );
    let state = loc
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    // the IdP (mock) sends the browser back — any code exchanges
    let cb = http(
        port,
        "GET",
        &format!("/oidc/callback?code=placeholder&state={state}"),
        &[],
        "",
    );
    assert_eq!(cb.status, 302, "{}", cb.body);
    let home = cb.location();
    assert!(home.starts_with("/#token="), "{home}");
    let mut token = String::new();
    let mut user = String::new();
    for pair in home.trim_start_matches("/#").split('&') {
        if let Some(v) = pair.strip_prefix("token=") {
            token = v.to_string();
        }
        if let Some(v) = pair.strip_prefix("user=") {
            user = v.to_string();
        }
    }
    (token, user)
}

#[test]
fn sso_login_creates_session_and_user() {
    let srv = serve("login", None, "pw");
    wait_ready(&srv);
    // the Studio overlay grows the SSO button when the IdP is configured
    let page = http(srv.port, "GET", "/", &[], "");
    assert_eq!(page.status, 200);
    assert!(
        page.body.contains("Sign in with SSO"),
        "{}",
        &page.body[..page.body.len().min(200)]
    );
    let (token, user) = sso_token(srv.port);
    assert_eq!(user, "mock-user");
    // the session is live
    let schema = http(
        srv.port,
        "GET",
        &format!("/api/schema?token={token}"),
        &[],
        "",
    );
    assert_eq!(schema.status, 200, "{}", schema.body);
    // and the store holds the upserted user with the default viewer role
    assert_eq!(
        store_roles(&srv.db, "mock-user"),
        Some(vec!["viewer".to_string()])
    );
}

#[test]
fn oauth_authorize_redirects_to_idp() {
    let srv = serve("authz", None, "pw");
    wait_ready(&srv);
    let client = register(srv.port);
    let verifier = "v".repeat(50);
    let q = format!(
        "/oauth/authorize?response_type=code&client_id={client}&redirect_uri={CALLBACK}&code_challenge={}&code_challenge_method=S256&state=client-state",
        challenge(&verifier)
    );
    let r = http(srv.port, "GET", &q, &[], "");
    assert_eq!(r.status, 302, "{}", r.body);
    let loc = r.location();
    assert!(
        loc.starts_with("https://oidc.test.invalid/authorize?response_type=code"),
        "{loc}"
    );
    assert!(loc.contains("state="), "{loc}");
}

#[test]
fn unknown_claim_maps_to_viewer() {
    let srv = serve(
        "janitor",
        Some(r#"{"sub":"janitor","name":null,"roles":["janitor"]}"#),
        "pw",
    );
    wait_ready(&srv);
    let (token, user) = sso_token(srv.port);
    assert_eq!(user, "janitor");
    // unknown claim roles map to viewer — never admin
    assert_eq!(
        store_roles(&srv.db, "janitor"),
        Some(vec!["viewer".to_string()])
    );
    let schema = http(
        srv.port,
        "GET",
        &format!("/api/schema?token={token}"),
        &[],
        "",
    );
    assert_eq!(schema.status, 200, "{}", schema.body);
}

#[test]
fn breakglass_admin_login_works_when_idp_configured() {
    let (pk, blob) = common::sign("test", None, &["oidc-sso"]);
    let pk_b64 = URL_SAFE_NO_PAD.encode(&pk);
    let lic = std::env::temp_dir().join(format!("aikoql-oidc-glass-{}.lic", std::process::id()));
    std::fs::write(&lic, &blob).unwrap();
    common::register_temp_path(&lic);
    let db = std::env::temp_dir().join(format!("aikoql-oidc-glass-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log1 = std::env::temp_dir().join(format!("aikoql-oidc-glass-1-{}.log", std::process::id()));
    common::register_temp_path(&log1);
    let log2 = std::env::temp_dir().join(format!("aikoql-oidc-glass-2-{}.log", std::process::id()));
    common::register_temp_path(&log2);

    let lic_str = lic.to_str().unwrap().to_string();
    let envs = |pw: &str| -> Vec<(String, String)> {
        vec![
            ("AIKOQL_LICENSE".to_string(), lic_str.clone()),
            ("AIKOQL_LICENSE_PUBLIC_KEY".to_string(), pk_b64.clone()),
            ("AIKOQL_OIDC_MOCK".to_string(), "1".to_string()),
            ("AIKOQL_ADMIN_PASSWORD".to_string(), pw.to_string()),
        ]
    };
    // boot 1: the bootstrap admin seeds the store with first-pw
    let first = envs("first-pw");
    let first: Vec<(&str, &str)> = first
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let srv1 = common::spawn_server(&db, &log1, &first, &[]);
    wait_ready(&srv1);
    srv1.shutdown();
    // boot 2: a different AIKOQL_ADMIN_PASSWORD — the store already has an
    // admin, so nothing is re-seeded
    let second = envs("second-pw");
    let second: Vec<(&str, &str)> = second
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let srv2 = common::spawn_server(&db, &log2, &second, &[]);
    wait_ready(&srv2);
    // break-glass: the FIRST password (the stored hash) logs in
    let ok = http(
        srv2.port,
        "POST",
        "/api/login",
        &[("Content-Type", "application/json")],
        r#"{"username":"admin","password":"first-pw"}"#,
    );
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert!(ok.body.contains("\"token\""), "{}", ok.body);
    // the second boot's env password does not — the env only seeds
    let bad = http(
        srv2.port,
        "POST",
        "/api/login",
        &[("Content-Type", "application/json")],
        r#"{"username":"admin","password":"second-pw"}"#,
    );
    assert_eq!(bad.status, 401, "{}", bad.body);
}

#[test]
fn live_role_revocation() {
    let srv = serve(
        "revoke",
        Some(r#"{"sub":"boss","name":null,"roles":["admin"]}"#),
        "pw",
    );
    wait_ready(&srv);
    let client = register(srv.port);
    let verifier = "v".repeat(50);
    let authz = http(
        srv.port,
        "GET",
        &format!(
            "/oauth/authorize?response_type=code&client_id={client}&redirect_uri={CALLBACK}&code_challenge={}&code_challenge_method=S256&state=st1",
            challenge(&verifier)
        ),
        &[],
        "",
    );
    assert_eq!(authz.status, 302, "{}", authz.body);
    let state = authz
        .location()
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    // the IdP (mock) sends the browser back — exchange succeeds for any code
    let cb = http(
        srv.port,
        "GET",
        &format!("/oidc/callback?code=whatever&state={state}"),
        &[],
        "",
    );
    assert_eq!(cb.status, 302, "{}", cb.body);
    let code = cb
        .location()
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let tok = post_form(
        srv.port,
        "/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client),
            ("redirect_uri", CALLBACK),
            ("code_verifier", &verifier),
        ],
    );
    assert_eq!(tok.status, 200, "{}", tok.body);
    let access = tok.json()["access_token"]
        .as_str()
        .expect("access_token")
        .to_string();
    let bearer = format!("Bearer {access}");
    // the token authenticates: an MCP session initializes
    let init = http(
        srv.port,
        "POST",
        "/mcp",
        &[
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
        ],
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
    );
    assert_eq!(init.status, 200, "{}", init.body);
    let sid = init
        .header("mcp-session-id")
        .expect("session id")
        .to_string();
    // the admin claim grants the restricted tool
    let call = http(
        srv.port,
        "POST",
        "/mcp",
        &[
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
            ("mcp-session-id", sid.as_str()),
        ],
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"invalidate","arguments":{}}}"#,
    );
    assert_eq!(call.status, 200);
    assert!(!call.body.contains("capability denied"), "{}", call.body);
    // flip the store roles admin → viewer (the store is the authority)
    set_roles(&srv.db, "boss", &["viewer"]);
    // the same token, a fresh session: the live lookup sees viewer
    let init2 = http(
        srv.port,
        "POST",
        "/mcp",
        &[
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
        ],
        r#"{"jsonrpc":"2.0","id":3,"method":"initialize","params":{}}"#,
    );
    assert_eq!(init2.status, 200, "{}", init2.body);
    let sid2 = init2
        .header("mcp-session-id")
        .expect("session id")
        .to_string();
    let denied = http(
        srv.port,
        "POST",
        "/mcp",
        &[
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
            ("mcp-session-id", sid2.as_str()),
        ],
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"invalidate","arguments":{}}}"#,
    );
    assert_eq!(denied.status, 200);
    assert!(denied.body.contains("capability denied"), "{}", denied.body);
}

/// Controller fix (concern 2): a config-file admin ([auth].users) keeps its
/// v0.4.0 password when the multi-user store is on — the store hash is tried
/// first, then the config user (never the AIKOQL_ADMIN_PASSWORD env entry,
/// which only seeds).
#[test]
fn config_user_admin_login_still_works_in_enterprise() {
    let (pk, blob) = common::sign("test", None, &["multi-user"]);
    let pk_b64 = URL_SAFE_NO_PAD.encode(&pk);
    let lic = std::env::temp_dir().join(format!("aikoql-oidc-cfgadmin-{}.lic", std::process::id()));
    std::fs::write(&lic, &blob).unwrap();
    common::register_temp_path(&lic);
    let db = std::env::temp_dir().join(format!("aikoql-oidc-cfgadmin-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-oidc-cfgadmin-{}.log", std::process::id()));
    common::register_temp_path(&log);
    // the config user's argon2id hash via the binary's own CLI (no argon2
    // dev-dependency needed)
    let hashed = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
        .arg("hash-password")
        .arg("cfg-pw")
        .output()
        .expect("run hash-password");
    assert!(
        hashed.status.success(),
        "hash-password: {}",
        String::from_utf8_lossy(&hashed.stderr)
    );
    let phc = String::from_utf8(hashed.stdout).expect("hash-password stdout");
    let cfg =
        std::env::temp_dir().join(format!("aikoql-oidc-cfgadmin-{}.toml", std::process::id()));
    std::fs::write(
        &cfg,
        format!(
            "[auth]\nusers = [\n  {{ username = \"admin\", hash = \"{}\", roles = [\"admin\"] }},\n]\n",
            phc.trim()
        ),
    )
    .unwrap();
    common::register_temp_path(&cfg);

    let lic_str = lic.to_str().unwrap().to_string();
    let envs = [
        ("AIKOQL_LICENSE".to_string(), lic_str),
        ("AIKOQL_LICENSE_PUBLIC_KEY".to_string(), pk_b64),
        ("AIKOQL_ADMIN_PASSWORD".to_string(), "seed-pw".to_string()),
    ];
    let envs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let cfg_str = cfg.to_str().unwrap().to_string();
    let extra = ["--config", cfg_str.as_str()];
    let srv = common::spawn_server(&db, &log, &envs, &extra);
    wait_ready(&srv);

    // the config-file admin's v0.4.0 password keeps working
    let ok = http(
        srv.port,
        "POST",
        "/api/login",
        &[("Content-Type", "application/json")],
        r#"{"username":"admin","password":"cfg-pw"}"#,
    );
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert!(ok.body.contains("\"token\""), "{}", ok.body);
    // the seeded store hash is still the break-glass credential
    let glass = http(
        srv.port,
        "POST",
        "/api/login",
        &[("Content-Type", "application/json")],
        r#"{"username":"admin","password":"seed-pw"}"#,
    );
    assert_eq!(glass.status, 200, "{}", glass.body);
    // and the store holds the seeded admin
    assert_eq!(
        store_roles(&srv.db, "admin"),
        Some(vec!["admin".to_string()])
    );
}

#[test]
fn unlicensed_studio_password_login_unchanged() {
    let db = std::env::temp_dir().join(format!("aikoql-oidc-community-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log =
        std::env::temp_dir().join(format!("aikoql-oidc-community-{}.log", std::process::id()));
    common::register_temp_path(&log);
    let srv = common::spawn_server(&db, &log, &[("AIKOQL_ADMIN_PASSWORD", "pw")], &[]);
    wait_ready(&srv);
    // the v0.4.0 overlay: password login, no SSO button
    let page = http(srv.port, "GET", "/", &[], "");
    assert_eq!(page.status, 200);
    assert!(
        !page.body.contains("Sign in with SSO"),
        "no SSO without a license"
    );
    let login = http(
        srv.port,
        "POST",
        "/api/login",
        &[("Content-Type", "application/json")],
        r#"{"username":"admin","password":"pw"}"#,
    );
    assert_eq!(login.status, 200, "{}", login.body);
    assert!(login.body.contains("\"token\""), "{}", login.body);
    // community mode never creates the enterprise store
    assert!(!db.join("users.db").exists());
}
