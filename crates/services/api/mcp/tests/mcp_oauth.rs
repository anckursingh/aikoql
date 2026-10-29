//! OAuth 2.1 for MCP over HTTP — the flow claude.ai's connectors and Claude
//! Code run (claude.com/docs/connectors/building/authentication; MCP
//! authorization 2025-06-18): a 401 on /mcp points at RFC 9728 protected
//! resource metadata; RFC 8414 authorization server metadata; RFC 7591
//! dynamic client registration of a public client; the authorization-code
//! grant with S256 PKCE behind a login + consent page (the server's argon2
//! [auth] logins); rotating refresh tokens; and /mcp accepting the issued
//! access tokens next to the static --tcp-token ones.

use base64::Engine as _;
use serde_json::{json, Value as J};
use sha2::Digest as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CLAUDE_CALLBACK: &str = "https://claude.ai/api/mcp/auth_callback";

fn free_port() -> u16 {
    static GIVEN: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());
    loop {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut given = GIVEN.lock().unwrap();
        if !given.contains(&port) {
            given.push(port);
            return port;
        }
    }
}

struct Server {
    child: Child,
    db: PathBuf,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.db);
    }
}

impl Server {
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// A server with an admin login (AIKOQL_ADMIN_PASSWORD) and a static token.
fn serve(tag: &str) -> Server {
    let port = free_port();
    let db = std::env::temp_dir().join(format!("mcp-oauth-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    spawn(db, port)
}

impl Server {
    /// The same server again — same data directory, same port (tokens are
    /// bound to the URL they were issued for).
    fn restart(mut self) -> Server {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (db, port) = (std::mem::take(&mut self.db), self.port);
        std::mem::forget(self); // its Drop would remove the data directory
                                // the old process must have released the port before the new one binds
        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect(("127.0.0.1", port)).is_ok() {
            assert!(
                Instant::now() < deadline,
                "the old server never released {port}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        spawn(db, port)
    }
}

fn spawn(db: PathBuf, port: u16) -> Server {
    let child = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
        .arg("serve")
        .args(["--metrics-addr", &format!("127.0.0.1:{port}")])
        .args(["--tcp-token", "static-tok::admin"])
        .arg(&db)
        .env("AIKOQL_ADMIN_PASSWORD", "oauth-pw")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn aikoql-mcp serve");
    let deadline = Instant::now() + Duration::from_secs(20);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "server never listened on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
    Server { child, db, port }
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
}

fn http(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Resp {
        status,
        headers,
        body: body.to_string(),
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn post_form(port: u16, path: &str, pairs: &[(&str, &str)]) -> Resp {
    http(
        port,
        "POST",
        path,
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &form(pairs),
    )
}

/// The S256 challenge of a verifier.
fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()))
}

fn register(port: u16, redirects: &[&str]) -> String {
    let r = http(
        port,
        "POST",
        "/oauth/register",
        &[("Content-Type", "application/json")],
        &json!({"redirect_uris": redirects, "client_name": "Claude", "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"]})
        .to_string(),
    );
    assert_eq!(r.status, 201, "register: {}", r.body);
    r.json()["client_id"]
        .as_str()
        .expect("a client_id")
        .to_string()
}

/// One authorization: the login form posted back; the redirect's Location.
fn authorize(srv: &Server, client: &str, redirect: &str, verifier: &str, password: &str) -> Resp {
    let resource = format!("{}/mcp", srv.base());
    post_form(
        srv.port,
        "/oauth/authorize",
        &[
            ("response_type", "code"),
            ("client_id", client),
            ("redirect_uri", redirect),
            ("code_challenge", &challenge(verifier)),
            ("code_challenge_method", "S256"),
            ("state", "st-123"),
            ("scope", "mcp"),
            ("resource", &resource),
            ("username", "admin"),
            ("password", password),
            ("approve", "1"),
        ],
    )
}

fn code_of(location: &str) -> String {
    let q = location.split_once('?').map(|(_, q)| q).unwrap_or("");
    q.split('&')
        .find_map(|p| p.strip_prefix("code="))
        .expect("a code")
        .to_string()
}

fn exchange(srv: &Server, client: &str, redirect: &str, code: &str, verifier: &str) -> Resp {
    let resource = format!("{}/mcp", srv.base());
    post_form(
        srv.port,
        "/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect),
            ("client_id", client),
            ("code_verifier", verifier),
            ("resource", &resource),
        ],
    )
}

fn mcp_initialize(port: u16, bearer: &str) -> Resp {
    let auth = format!("Bearer {bearer}");
    http(
        port,
        "POST",
        "/mcp",
        &[
            ("Authorization", &auth),
            ("Content-Type", "application/json"),
        ],
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {}}})
        .to_string(),
    )
}

const VERIFIER: &str = "a-long-random-pkce-verifier-0123456789-abcdefghijklmnopqrstuvwxyz";

#[test]
fn o001_discovery_from_a_401() {
    let srv = serve("001");
    let r = http(
        srv.port,
        "POST",
        "/mcp",
        &[("Content-Type", "application/json")],
        "{}",
    );
    assert_eq!(r.status, 401);
    let www = r.header("www-authenticate").unwrap_or_default().to_string();
    let meta_url = format!("{}/.well-known/oauth-protected-resource", srv.base());
    assert!(
        www.contains(&format!("resource_metadata=\"{meta_url}\"")),
        "WWW-Authenticate: {www}"
    );
    let prm = http(
        srv.port,
        "GET",
        "/.well-known/oauth-protected-resource",
        &[],
        "",
    );
    assert_eq!(prm.status, 200, "{}", prm.body);
    let prm = prm.json();
    assert_eq!(
        prm["resource"],
        json!(format!("{}/mcp", srv.base())),
        "the resource is the MCP URL exactly"
    );
    assert_eq!(prm["authorization_servers"][0], json!(srv.base()));
    let asm = http(
        srv.port,
        "GET",
        "/.well-known/oauth-authorization-server",
        &[],
        "",
    );
    assert_eq!(asm.status, 200, "{}", asm.body);
    let asm = asm.json();
    assert_eq!(asm["issuer"], json!(srv.base()));
    for k in [
        "authorization_endpoint",
        "token_endpoint",
        "registration_endpoint",
    ] {
        assert!(
            asm[k].as_str().is_some_and(|u| u.starts_with(&srv.base())),
            "{k}: {asm}"
        );
    }
    assert_eq!(asm["code_challenge_methods_supported"], json!(["S256"]));
    assert!(asm["token_endpoint_auth_methods_supported"]
        .as_array()
        .unwrap()
        .contains(&json!("none")));
    assert!(asm["grant_types_supported"]
        .as_array()
        .unwrap()
        .contains(&json!("refresh_token")));
}

#[test]
fn o002_the_full_flow_ends_in_a_working_mcp_session() {
    let srv = serve("002");
    let client = register(srv.port, &[CLAUDE_CALLBACK]);
    // the consent page names the client and the redirect host
    let resource = format!("{}/mcp", srv.base());
    let page_path = format!(
        "/oauth/authorize?response_type=code&client_id={client}&redirect_uri={}&code_challenge={}\
         &code_challenge_method=S256&state=st-123&scope=mcp&resource={}",
        enc(CLAUDE_CALLBACK),
        challenge(VERIFIER),
        enc(&resource)
    );
    let page = http(srv.port, "GET", &page_path, &[], "");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(
        page.body.contains("claude.ai") && page.body.contains("<form"),
        "consent page: {}",
        page.body
    );
    assert_eq!(
        page.header("x-frame-options"),
        Some("DENY"),
        "no framing of the login page"
    );
    // sign in → redirected back with a code and the state
    let r = authorize(&srv, &client, CLAUDE_CALLBACK, VERIFIER, "oauth-pw");
    assert_eq!(r.status, 302, "{}", r.body);
    let location = r.header("location").expect("a redirect").to_string();
    assert!(
        location.starts_with(&format!("{CLAUDE_CALLBACK}?")) && location.contains("state=st-123"),
        "{location}"
    );
    // code → tokens
    let t = exchange(
        &srv,
        &client,
        CLAUDE_CALLBACK,
        &code_of(&location),
        VERIFIER,
    );
    assert_eq!(t.status, 200, "{}", t.body);
    let t = t.json();
    assert_eq!(t["token_type"], json!("Bearer"));
    assert!(t["expires_in"].as_u64().is_some_and(|e| e > 0));
    assert!(t["refresh_token"].is_string());
    // the access token opens an MCP session
    let init = mcp_initialize(srv.port, t["access_token"].as_str().unwrap());
    assert_eq!(init.status, 200, "{}", init.body);
    assert!(init.header("mcp-session-id").is_some());
}

#[test]
fn o003_what_the_flow_refuses() {
    let srv = serve("003");
    let client = register(srv.port, &[CLAUDE_CALLBACK]);
    // a wrong password never yields a code
    let bad = authorize(&srv, &client, CLAUDE_CALLBACK, VERIFIER, "wrong");
    assert!(
        bad.header("location").is_none_or(|l| !l.contains("code=")),
        "a wrong password redirected with a code"
    );
    // an unregistered redirect is never redirected to
    let evil = authorize(
        &srv,
        &client,
        "https://evil.example/cb",
        VERIFIER,
        "oauth-pw",
    );
    assert_eq!(evil.status, 400, "{}", evil.body);
    assert!(
        evil.header("location").is_none(),
        "redirected to an unregistered URI"
    );
    // a wrong PKCE verifier, a reused code, a mismatched redirect: invalid_grant
    let ok = authorize(&srv, &client, CLAUDE_CALLBACK, VERIFIER, "oauth-pw");
    let code = code_of(ok.header("location").unwrap());
    let wrong_pkce = exchange(
        &srv,
        &client,
        CLAUDE_CALLBACK,
        &code,
        "not-the-verifier-0123456789-0123456789-0123456789",
    );
    assert_eq!(wrong_pkce.status, 400);
    assert_eq!(wrong_pkce.json()["error"], json!("invalid_grant"));
    let ok = authorize(&srv, &client, CLAUDE_CALLBACK, VERIFIER, "oauth-pw");
    let code = code_of(ok.header("location").unwrap());
    assert_eq!(
        exchange(&srv, &client, CLAUDE_CALLBACK, &code, VERIFIER).status,
        200
    );
    let reused = exchange(&srv, &client, CLAUDE_CALLBACK, &code, VERIFIER);
    assert_eq!(
        reused.json()["error"],
        json!("invalid_grant"),
        "a code is single-use"
    );
    let ok = authorize(&srv, &client, CLAUDE_CALLBACK, VERIFIER, "oauth-pw");
    let code = code_of(ok.header("location").unwrap());
    let moved = exchange(&srv, &client, "https://claude.ai/other", &code, VERIFIER);
    assert_eq!(
        moved.json()["error"],
        json!("invalid_grant"),
        "the redirect must match the authorization's"
    );
    // DCR refuses a redirect that is neither HTTPS nor loopback
    let r = http(
        srv.port,
        "POST",
        "/oauth/register",
        &[("Content-Type", "application/json")],
        &json!({"redirect_uris": ["http://evil.example/cb"]}).to_string(),
    );
    assert_eq!(r.status, 400, "{}", r.body);
}

#[test]
fn o004_refresh_tokens_rotate() {
    let srv = serve("004");
    let client = register(srv.port, &[CLAUDE_CALLBACK]);
    let ok = authorize(&srv, &client, CLAUDE_CALLBACK, VERIFIER, "oauth-pw");
    let t = exchange(
        &srv,
        &client,
        CLAUDE_CALLBACK,
        &code_of(ok.header("location").unwrap()),
        VERIFIER,
    )
    .json();
    let old = t["refresh_token"].as_str().unwrap().to_string();
    let r = post_form(
        srv.port,
        "/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &old),
            ("client_id", &client),
        ],
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let fresh = r.json();
    assert_ne!(fresh["refresh_token"], json!(old), "a new refresh token");
    assert_eq!(
        mcp_initialize(srv.port, fresh["access_token"].as_str().unwrap()).status,
        200
    );
    let again = post_form(
        srv.port,
        "/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &old),
            ("client_id", &client),
        ],
    );
    assert_eq!(again.status, 400);
    assert_eq!(
        again.json()["error"],
        json!("invalid_grant"),
        "the old refresh token is dead"
    );
}

#[test]
fn o005_claude_code_loopback_redirects_match_on_any_port() {
    let srv = serve("005");
    let client = register(
        srv.port,
        &["http://localhost/callback", "http://127.0.0.1/callback"],
    );
    for redirect in [
        "http://localhost:5555/callback",
        "http://127.0.0.1:61000/callback",
    ] {
        let r = authorize(&srv, &client, redirect, VERIFIER, "oauth-pw");
        assert_eq!(r.status, 302, "{redirect}: {}", r.body);
        assert!(r.header("location").unwrap().starts_with(redirect));
    }
    let other_path = authorize(
        &srv,
        &client,
        "http://localhost:5555/elsewhere",
        VERIFIER,
        "oauth-pw",
    );
    assert_eq!(other_path.status, 400, "only the port is ignored");
}

#[test]
fn o006_bad_tokens_are_401_and_static_tokens_still_work() {
    let srv = serve("006");
    let r = mcp_initialize(srv.port, "not-a-token");
    assert_eq!(r.status, 401);
    assert!(r
        .header("www-authenticate")
        .is_some_and(|w| w.contains("resource_metadata=")));
    assert_eq!(
        mcp_initialize(srv.port, "static-tok").status,
        200,
        "the --tcp-token tokens are unchanged"
    );
}

/// A sign-in survives a restart: the grants live in the database (as the
/// sign-ins of database-backed connectors do), so the access token still
/// opens a session, the refresh token still rotates, and the registered
/// client is still known — no new sign-in.
#[test]
fn o007_a_sign_in_survives_a_restart() {
    let srv = serve("007");
    let client = register(srv.port, &[CLAUDE_CALLBACK]);
    let ok = authorize(&srv, &client, CLAUDE_CALLBACK, VERIFIER, "oauth-pw");
    let t = exchange(
        &srv,
        &client,
        CLAUDE_CALLBACK,
        &code_of(ok.header("location").unwrap()),
        VERIFIER,
    )
    .json();
    let (access, refresh) = (
        t["access_token"].as_str().unwrap().to_string(),
        t["refresh_token"].as_str().unwrap().to_string(),
    );

    let srv = srv.restart();
    assert_eq!(
        mcp_initialize(srv.port, &access).status,
        200,
        "the access token outlives the restart"
    );
    let r = post_form(
        srv.port,
        "/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", &client),
        ],
    );
    assert_eq!(
        r.status, 200,
        "the refresh token outlives the restart: {}",
        r.body
    );
    assert_eq!(
        mcp_initialize(srv.port, r.json()["access_token"].as_str().unwrap()).status,
        200
    );
    let again = authorize(&srv, &client, CLAUDE_CALLBACK, VERIFIER, "oauth-pw");
    assert_eq!(
        again.status, 302,
        "the registered client is still known: {}",
        again.body
    );
}

fn tool_text(port: u16, bearer: &str, sid: &str, name: &str, args: J) -> String {
    let auth = format!("Bearer {bearer}");
    let r = http(
        port,
        "POST",
        "/mcp",
        &[("Authorization", &auth), ("Mcp-Session-Id", sid), ("Content-Type", "application/json")],
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": name, "arguments": args}}).to_string(),
    );
    assert_eq!(r.status, 200, "{name}: {}", r.body);
    r.json()["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The grants live under the store's reserved sys/oauth/ keys, outside the
/// repository: a sign-in adds no knowledge object, and nothing of it shows
/// in the schema a user can discover.
#[test]
fn o008_sign_ins_are_not_knowledge() {
    let srv = serve("008");
    let sid = mcp_initialize(srv.port, "static-tok")
        .header("mcp-session-id")
        .unwrap()
        .to_string();
    let objects = |sid: &str| {
        serde_json::from_str::<J>(&tool_text(
            srv.port,
            "static-tok",
            sid,
            "metrics",
            json!({}),
        ))
        .unwrap()["total_objects"]
            .clone()
    };
    let before = objects(&sid);
    assert!(before.is_u64(), "metrics reports total_objects: {before}");
    let client = register(srv.port, &[CLAUDE_CALLBACK]);
    let ok = authorize(&srv, &client, CLAUDE_CALLBACK, VERIFIER, "oauth-pw");
    let t = exchange(
        &srv,
        &client,
        CLAUDE_CALLBACK,
        &code_of(ok.header("location").unwrap()),
        VERIFIER,
    )
    .json();
    assert!(t["access_token"].is_string());
    assert_eq!(objects(&sid), before, "a sign-in is not a knowledge object");
    let schema = tool_text(srv.port, "static-tok", &sid, "discover_schema", json!({}));
    assert!(
        !schema.contains("oauth") && !schema.contains(&client),
        "OAuth state leaks into the schema"
    );
}
