//! MCP over Streamable HTTP (`POST /mcp` on the HTTP surface): what remote
//! MCP clients — claude.ai connectors, `claude mcp add --transport http` —
//! speak. One JSON-RPC message per POST, answered as application/json;
//! `initialize` opens a session (`Mcp-Session-Id`), every request carries
//! `Authorization: Bearer <--tcp-token>`; DELETE ends the session; GET is the
//! session's event stream. Foreign `Origin`s are refused (DNS rebinding),
//! bodies are capped at 1 MiB.

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

/// GET is the session's event stream (h012-h016): without a session it is
/// 400; methods other than GET, POST and DELETE are 405.
#[test]
fn h007_get_needs_a_session_and_other_methods_are_not_allowed() {
    let srv = serve("007", &[]);
    assert_eq!(request(srv.port, "GET", &[BEARER], "").status, 400);
    let put = request(srv.port, "PUT", &[BEARER], "");
    assert_eq!(put.status, 405);
    assert!(
        put.header("allow").is_some_and(|a| a.contains("GET")),
        "Allow names GET, POST, DELETE"
    );
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

fn negotiated(port: u16, requested: &str) -> String {
    let r = request(
        port,
        "POST",
        &[BEARER],
        &rpc(
            1,
            "initialize",
            json!({"protocolVersion": requested, "capabilities": {}}),
        ),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()["result"]["protocolVersion"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The MCP version negotiation (the dispatcher's, shared by every
/// transport): a supported version is answered as requested; anything else
/// gets the newest supported one — the server no longer answers 2024-11-05
/// (which predates Streamable HTTP) to a client asking for a newer version.
#[test]
fn h010_initialize_negotiates_the_protocol_version() {
    let srv = serve("010", &[]);
    for v in ["2024-11-05", "2025-03-26", "2025-06-18"] {
        assert_eq!(negotiated(srv.port, v), v, "a supported version is echoed");
    }
    assert_eq!(
        negotiated(srv.port, "2099-01-01"),
        "2025-06-18",
        "an unknown one gets the newest supported"
    );
}

/// 2025-06-18: after initialize, an HTTP request carries
/// MCP-Protocol-Version; an unsupported value is 400.
#[test]
fn h011_an_unsupported_protocol_version_header_is_refused() {
    let srv = serve("011", &[]);
    let sid = initialize(srv.port);
    let list = rpc(2, "tools/list", json!({}));
    let ok = request(
        srv.port,
        "POST",
        &[
            BEARER,
            ("Mcp-Session-Id", &sid),
            ("MCP-Protocol-Version", "2025-06-18"),
        ],
        &list,
    );
    assert_eq!(ok.status, 200, "{}", ok.body);
    let bad = request(
        srv.port,
        "POST",
        &[
            BEARER,
            ("Mcp-Session-Id", &sid),
            ("MCP-Protocol-Version", "1999-01-01"),
        ],
        &list,
    );
    assert_eq!(bad.status, 400, "{}", bad.body);
}

/// An open GET /mcp event stream: SSE events as (id, JSON data).
struct Events {
    reader: std::io::BufReader<TcpStream>,
    status: u16,
}

fn open_stream(port: u16, sid: &str, last_event_id: Option<&str>) -> Events {
    use std::io::BufRead;
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut req = format!(
        "GET /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\
         Authorization: Bearer http-tok\r\nMcp-Session-Id: {sid}\r\n"
    );
    if let Some(id) = last_event_id {
        req.push_str(&format!("Last-Event-ID: {id}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut reader = std::io::BufReader::new(s);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break; // end of the headers
        }
    }
    Events { reader, status }
}

impl Events {
    /// The next event (keep-alive comments skipped); None at EOF.
    fn next(&mut self) -> Option<(String, J)> {
        use std::io::BufRead;
        let (mut id, mut data) = (String::new(), String::new());
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let l = line.trim_end_matches(['\r', '\n']);
            if l.is_empty() {
                if !data.is_empty() {
                    return Some((id, serde_json::from_str(&data).unwrap()));
                }
                continue;
            }
            if let Some(v) = l.strip_prefix("id: ") {
                id = v.to_string();
            } else if let Some(v) = l.strip_prefix("data: ") {
                data.push_str(v);
            }
        }
    }

    /// The next event whose method is `method`.
    fn next_method(&mut self, method: &str) -> (String, J) {
        loop {
            let (id, ev) = self
                .next()
                .unwrap_or_else(|| panic!("the stream ended before a {method}"));
            if ev["method"] == json!(method) {
                return (id, ev);
            }
        }
    }
}

fn remember_n(port: u16, sid: &str, n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            tool(
                port,
                sid,
                100 + i as u64,
                "remember",
                json!({"subject": "admin", "type_name": "note", "properties": {"n": i}}),
            )["koid"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

/// A subscription's events reach the session's GET stream (they were
/// written into a per-request buffer and lost before).
#[test]
fn h012_subscription_events_arrive_on_the_get_stream() {
    let srv = serve("012", &[]);
    let sid = initialize(srv.port);
    let mut events = open_stream(srv.port, &sid, None);
    assert_eq!(events.status, 200, "GET /mcp opens the stream");
    let sub = request(
        srv.port,
        "POST",
        &[BEARER, ("Mcp-Session-Id", &sid)],
        &rpc(2, "notifications/subscribe", json!({"id": "sub-1"})),
    );
    assert_eq!(sub.status, 200, "{}", sub.body);
    assert_eq!(
        sub.json()["result"]["subscribed"],
        json!(true),
        "{}",
        sub.body
    );
    let koids = remember_n(srv.port, &sid, 1);
    let (_, ev) = events.next_method("notifications/notify");
    assert_eq!(ev["params"]["id"], json!("sub-1"));
    assert!(
        ev.to_string().contains(&koids[0]),
        "the event names the new object: {ev}"
    );
}

/// A streamed query's chunks after the first arrive on the GET stream.
#[test]
fn h013_stream_query_chunks_arrive_on_the_get_stream() {
    let srv = serve("013", &[]);
    let sid = initialize(srv.port);
    remember_n(srv.port, &sid, 130); // CHUNK_SIZE is 100: two chunks
    let mut events = open_stream(srv.port, &sid, None);
    let r = request(
        srv.port,
        "POST",
        &[BEARER, ("Mcp-Session-Id", &sid)],
        &rpc(
            3,
            "aikoql/stream",
            json!({"query": "MATCH note RETURN *", "subject": "admin"}),
        ),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let first = r.json();
    assert_eq!(first["result"]["chunk"], json!(0), "{first}");
    assert_eq!(first["result"]["total_chunks"], json!(2), "{first}");
    let (_, ev) = events.next_method("notifications/notify");
    assert_eq!(ev["params"]["chunk"], json!(1), "{ev}");
    assert_eq!(ev["params"]["done"], json!(true), "{ev}");
}

/// One stream per session: a newer GET takes the session over and the
/// older stream ends (a reconnecting client is never locked out by its own
/// half-dead connection, which the server only notices on its next write);
/// it needs a known session.
#[test]
fn h014_a_newer_stream_supersedes_the_older_one() {
    let srv = serve("014", &[]);
    let sid = initialize(srv.port);
    let mut first = open_stream(srv.port, &sid, None);
    assert_eq!(first.status, 200);
    let second = open_stream(srv.port, &sid, None);
    assert_eq!(second.status, 200, "a newer stream opens");
    assert!(first.next().is_none(), "and the older one ends");
    assert_eq!(
        open_stream(srv.port, "00ff00ff00ff00ff00ff00ff00ff00ff", None).status,
        404
    );
}

/// DELETE ends the session's stream.
#[test]
fn h015_delete_ends_the_stream() {
    let srv = serve("015", &[]);
    let sid = initialize(srv.port);
    let mut events = open_stream(srv.port, &sid, None);
    assert_eq!(events.status, 200, "the stream opened");
    let del = request(srv.port, "DELETE", &[BEARER, ("Mcp-Session-Id", &sid)], "");
    assert!(del.status == 200 || del.status == 204);
    assert!(
        events.next().is_none(),
        "the stream closes when its session ends"
    );
}

/// Last-Event-ID resumes after the last seen event: nothing repeats,
/// nothing sent while away is lost.
#[test]
fn h016_last_event_id_resumes_the_stream() {
    let srv = serve("016", &[]);
    let sid = initialize(srv.port);
    let sub = request(
        srv.port,
        "POST",
        &[BEARER, ("Mcp-Session-Id", &sid)],
        &rpc(2, "notifications/subscribe", json!({"id": "sub-r"})),
    );
    assert_eq!(sub.status, 200, "{}", sub.body);
    let mut events = open_stream(srv.port, &sid, None);
    let first = remember_n(srv.port, &sid, 1);
    let (seen, ev) = events.next_method("notifications/notify");
    assert!(ev.to_string().contains(&first[0]));
    drop(events); // the client goes away
    std::thread::sleep(Duration::from_millis(300));
    let later = tool(
        srv.port,
        &sid,
        50,
        "remember",
        json!({"subject": "admin", "type_name": "note", "properties": {"n": 99}}),
    )["koid"]
        .as_str()
        .unwrap()
        .to_string();
    let mut resumed = open_stream(srv.port, &sid, Some(&seen));
    assert_eq!(resumed.status, 200);
    let (id2, ev2) = resumed.next_method("notifications/notify");
    assert_ne!(id2, seen, "the last seen event is not repeated");
    assert!(
        ev2.to_string().contains(&later),
        "the event sent while away arrives: {ev2}"
    );
}

/// h017 — headers split across TCP reads (a slow client, or headers past
/// one segment, e.g. a long bearer token): Content-Length is in the
/// second piece, and the body must still be read whole.
#[test]
fn h017_headers_split_across_reads_still_carry_the_body() {
    let srv = serve("017", &[]);
    let body = rpc(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
         "clientInfo": {"name": "http-test", "version": "0"}}),
    );
    let mut s = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
    s.set_nodelay(true).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s.write_all(b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthor")
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let rest = format!(
        "ization: Bearer http-tok\r\nAccept: application/json, text/event-stream\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(rest.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let r = parse(&raw);
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(
        r.json()["result"]["protocolVersion"].is_string(),
        "{}",
        r.body
    );
}

/// h018 — a TLS handshake on the plain-HTTP port (the image's healthcheck
/// tries https first, then http, within 3 s) is closed at once: reading on
/// for the end of headers would hold it until the read timeout.
#[test]
fn h018_a_tls_hello_on_the_plain_port_is_closed_at_once() {
    let srv = serve("018", &[]);
    let mut s = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    // the first bytes of a TLS 1.2/1.3 ClientHello record
    s.write_all(&[
        0x16, 0x03, 0x01, 0x02, 0x00, 0x01, 0x00, 0x01, 0xfc, 0x03, 0x03,
    ])
    .unwrap();
    let mut raw = Vec::new();
    let closed = s.read_to_end(&mut raw);
    assert!(
        closed.is_ok(),
        "the server held the connection open: {closed:?}"
    );
}
