//! MCP over Streamable HTTP (`POST /mcp` on the HTTP surface): what remote
//! MCP clients — claude.ai connectors, `claude mcp add --transport http` —
//! speak. One JSON-RPC message per POST, answered as application/json;
//! `initialize` opens a session (`Mcp-Session-Id`), every request carries
//! `Authorization: Bearer <--tcp-token>`; DELETE ends the session; GET (a
//! server-initiated stream) is 405. Foreign `Origin`s are refused (DNS
//! rebinding), bodies are capped at 1 MiB.

use rustls::pki_types::{pem::PemObject, CertificateDer, ServerName};
use serde_json::{json, Value as J};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_aikoql-mcp")
}

/// A free port, never the same one twice in this process (parallel tests
/// bind only after the probe released it).
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

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("mcp-http-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tls")
        .join(name)
        .to_string_lossy()
        .into_owned()
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

fn serve(tag: &str, extra: &[&str]) -> Server {
    let (db, port) = (tmp(tag), free_port());
    let child = Command::new(bin())
        .arg("serve")
        .args(["--metrics-addr", &format!("127.0.0.1:{port}")])
        .args([
            "--tcp-token",
            "http-tok::admin",
            "--tcp-token",
            "other-tok::admin",
        ])
        .args(extra)
        .arg(&db)
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

/// One HTTP response: status code, lower-cased headers, body.
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

fn parse(raw: &[u8]) -> Resp {
    let text = String::from_utf8_lossy(raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status line: {text:?}"));
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

fn request_on(
    s: &mut (impl Read + Write),
    method: &str,
    port: u16,
    headers: &[(&str, &str)],
    body: &str,
) -> Resp {
    let mut req = format!(
        "{method} /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json, text/event-stream\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body.as_bytes()).unwrap();
    s.flush().unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    parse(&raw)
}

fn request(port: u16, method: &str, headers: &[(&str, &str)], body: &str) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    request_on(&mut s, method, port, headers, body)
}

const BEARER: (&str, &str) = ("Authorization", "Bearer http-tok");

fn rpc(id: u64, method: &str, params: J) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn initialize(port: u16) -> String {
    let r = request(
        port,
        "POST",
        &[BEARER],
        &rpc(
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
             "clientInfo": {"name": "http-test", "version": "0"}}),
        ),
    );
    assert_eq!(r.status, 200, "initialize: {}", r.body);
    assert!(
        r.json()["result"]["protocolVersion"].is_string(),
        "{}",
        r.body
    );
    let sid = r
        .header("mcp-session-id")
        .expect("initialize assigns Mcp-Session-Id")
        .to_string();
    assert!(
        sid.len() >= 32 && sid.chars().all(|c| c.is_ascii_hexdigit()),
        "session id {sid:?}"
    );
    sid
}

fn tool(port: u16, sid: &str, id: u64, name: &str, args: J) -> J {
    let r = request(
        port,
        "POST",
        &[BEARER, ("Mcp-Session-Id", sid)],
        &rpc(id, "tools/call", json!({"name": name, "arguments": args})),
    );
    assert_eq!(r.status, 200, "{name}: {}", r.body);
    let text = r.json()["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn h001_initialize_then_tools_round_trip_in_a_session() {
    let srv = serve("001", &[]);
    let sid = initialize(srv.port);
    let list = request(
        srv.port,
        "POST",
        &[BEARER, ("Mcp-Session-Id", &sid)],
        &rpc(2, "tools/list", json!({})),
    );
    assert_eq!(list.status, 200);
    assert!(list.json()["result"]["tools"]
        .as_array()
        .is_some_and(|t| !t.is_empty()));
    let put = tool(
        srv.port,
        &sid,
        3,
        "remember",
        json!({"subject": "admin", "type_name": "note", "properties": {"n": 7}}),
    );
    let got = tool(
        srv.port,
        &sid,
        4,
        "get",
        json!({"subject": "admin", "koid": put["koid"]}),
    );
    assert_eq!(got["properties"]["n"], json!(7));
}

#[test]
fn h002_the_bearer_token_is_required_on_every_request() {
    let srv = serve("002", &[]);
    let init = rpc(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
    );
    let none = request(srv.port, "POST", &[], &init);
    assert_eq!(none.status, 401, "{}", none.body);
    assert!(none
        .header("www-authenticate")
        .is_some_and(|v| v.starts_with("Bearer")));
    let wrong = request(srv.port, "POST", &[("Authorization", "Bearer nope")], &init);
    assert_eq!(wrong.status, 401);
    let sid = initialize(srv.port);
    let later = request(
        srv.port,
        "POST",
        &[("Mcp-Session-Id", &sid)],
        &rpc(2, "tools/list", json!({})),
    );
    assert_eq!(later.status, 401, "a session does not replace the bearer");
}

#[test]
fn h003_sessions_are_required_known_and_bound_to_their_token() {
    let srv = serve("003", &[]);
    let missing = request(
        srv.port,
        "POST",
        &[BEARER],
        &rpc(2, "tools/list", json!({})),
    );
    assert_eq!(
        missing.status, 400,
        "a non-initialize request without a session: {}",
        missing.body
    );
    let unknown = request(
        srv.port,
        "POST",
        &[
            BEARER,
            ("Mcp-Session-Id", "00ff00ff00ff00ff00ff00ff00ff00ff"),
        ],
        &rpc(2, "tools/list", json!({})),
    );
    assert_eq!(unknown.status, 404);
    let sid = initialize(srv.port);
    let other = request(
        srv.port,
        "POST",
        &[
            ("Authorization", "Bearer other-tok"),
            ("Mcp-Session-Id", &sid),
        ],
        &rpc(2, "tools/list", json!({})),
    );
    assert_eq!(
        other.status, 404,
        "another token's session must not be usable"
    );
}

#[test]
fn h004_delete_ends_the_session() {
    let srv = serve("004", &[]);
    let sid = initialize(srv.port);
    let del = request(srv.port, "DELETE", &[BEARER, ("Mcp-Session-Id", &sid)], "");
    assert!(
        del.status == 200 || del.status == 204,
        "DELETE: {}",
        del.status
    );
    let after = request(
        srv.port,
        "POST",
        &[BEARER, ("Mcp-Session-Id", &sid)],
        &rpc(2, "tools/list", json!({})),
    );
    assert_eq!(after.status, 404, "a deleted session is gone");
}

#[test]
fn h005_notifications_are_accepted_without_a_body() {
    let srv = serve("005", &[]);
    let sid = initialize(srv.port);
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
    let r = request(srv.port, "POST", &[BEARER, ("Mcp-Session-Id", &sid)], &note);
    assert_eq!(r.status, 202, "{}", r.body);
}

#[test]
fn h006_a_foreign_origin_is_refused() {
    let srv = serve("006", &[]);
    let init = rpc(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
    );
    let evil = request(
        srv.port,
        "POST",
        &[BEARER, ("Origin", "https://evil.example")],
        &init,
    );
    assert_eq!(evil.status, 403, "DNS-rebinding guard: {}", evil.body);
    let local = format!("http://localhost:{}", srv.port);
    let ok = request(srv.port, "POST", &[BEARER, ("Origin", &local)], &init);
    assert_eq!(ok.status, 200, "a loopback origin is fine: {}", ok.body);
}

#[test]
fn h007_get_is_method_not_allowed() {
    let srv = serve("007", &[]);
    let r = request(srv.port, "GET", &[BEARER], "");
    assert_eq!(r.status, 405);
}

#[test]
fn h008_bodies_are_read_whole_up_to_the_cap() {
    let srv = serve("008", &[]);
    let sid = initialize(srv.port);
    // ~200 KB — past the HTTP handler's single 64 KB read
    let big = "x".repeat(200_000);
    let put = tool(
        srv.port,
        &sid,
        2,
        "remember",
        json!({"subject": "admin", "type_name": "note", "properties": {"big": big}}),
    );
    let got = tool(
        srv.port,
        &sid,
        3,
        "get",
        json!({"subject": "admin", "koid": put["koid"]}),
    );
    assert_eq!(
        got["properties"]["big"].as_str().map(str::len),
        Some(200_000)
    );
    // over 1 MiB: refused before it is read
    let mut s = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let head = format!(
        "POST /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer http-tok\r\nMcp-Session-Id: {sid}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        2 * 1024 * 1024
    );
    s.write_all(head.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    assert_eq!(parse(&raw).status, 413);
}

#[test]
fn h009_the_endpoint_serves_over_https() {
    let srv = serve(
        "009",
        &[
            "--tls-cert",
            &fixture("server.pem"),
            "--tls-key",
            &fixture("server.key"),
        ],
    );
    let ca = std::fs::read(fixture("ca.pem")).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(&ca) {
        roots.add(c.unwrap()).unwrap();
    }
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let conn =
        rustls::ClientConnection::new(Arc::new(cfg), ServerName::try_from("127.0.0.1").unwrap())
            .unwrap();
    let sock = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut tls = rustls::StreamOwned::new(conn, sock);
    let r = request_on(
        &mut tls,
        "POST",
        srv.port,
        &[BEARER],
        &rpc(
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
        ),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.header("mcp-session-id").is_some());
}
