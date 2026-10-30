//! TLS on the client listeners (`--listen`, `--native-port`): with
//! `--tls-cert/--tls-key` (or AIKOQL_TLS_CERT / AIKOQL_TLS_KEY) the bearer
//! token travels encrypted, so the listeners may bind any interface — what a
//! container needs for a published port. Without TLS the loopback-only rule
//! stands. Fixtures: tests/fixtures/tls (SAN IP:127.0.0.1, DNS:localhost).

use aikoql_native as nat;
use rustls::pki_types::{pem::PemObject, CertificateDer, ServerName};
use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_aikoql-mcp")
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tls")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// A free port, never the same one twice in this process: the tests run in
/// parallel and each server binds its port only after the probe released
/// it, so two probes handed one port would point one test at the other's
/// server (a TLS client at a plaintext one).
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
    let p = std::env::temp_dir().join(format!("tls-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn serve(db: &Path, args: &[&str], env: &[(&str, String)]) -> Server {
    let child = Command::new(bin())
        .arg("serve")
        .args(args)
        .args(["--tcp-token", "tls-tok::admin"])
        .arg(db)
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn aikoql-mcp serve");
    Server(child)
}

fn connect(port: u16) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            return s;
        }
        assert!(Instant::now() < deadline, "server never listened on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A client trusting exactly the test CA, verifying the name 127.0.0.1.
fn tls(port: u16) -> Tls {
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
    let name = ServerName::try_from("127.0.0.1").unwrap();
    let conn = rustls::ClientConnection::new(Arc::new(cfg), name).unwrap();
    rustls::StreamOwned::new(conn, connect(port))
}

fn call(s: &mut Tls, id: u64, method: &str, params: J) -> J {
    let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    writeln!(s, "{req}").unwrap();
    s.flush().unwrap();
    let mut r = BufReader::new(s);
    loop {
        let mut line = String::new();
        assert!(
            r.read_line(&mut line).unwrap() > 0,
            "connection closed awaiting {method}"
        );
        let msg: J = serde_json::from_str(line.trim()).unwrap();
        if msg["id"] == json!(id) {
            return msg;
        }
    }
}

fn init(s: &mut Tls) {
    let res = call(
        s,
        1,
        "initialize",
        json!({"protocolVersion": "2024-11-05", "capabilities": {}, "token": "tls-tok",
               "clientInfo": {"name": "tls", "version": "0"}}),
    );
    assert!(res.get("result").is_some(), "initialize over TLS: {res}");
}

fn cert_args() -> Vec<String> {
    vec![
        "--tls-cert".into(),
        fixture("server.pem"),
        "--tls-key".into(),
        fixture("server.key"),
    ]
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// A served MCP session over TLS on a non-loopback bind: the token
/// handshake, a write and a read back, several frames each way.
#[test]
fn tls001_mcp_over_tls_on_any_interface() {
    let (db, port) = (tmp("001"), free_port());
    let listen = format!("0.0.0.0:{port}");
    let _srv = serve(
        &db,
        &[&["--listen", &listen][..], &strs(&cert_args())].concat(),
        &[],
    );
    let mut s = tls(port);
    init(&mut s);
    let put = call(
        &mut s,
        2,
        "tools/call",
        json!({"name": "remember", "arguments": {"subject": "admin", "type_name": "note",
               "properties": {"body": "over tls"}}}),
    );
    let text = put["result"]["content"][0]["text"].as_str().unwrap();
    let koid = serde_json::from_str::<J>(text).unwrap()["koid"].clone();
    assert!(koid.is_string(), "remember over TLS: {put}");
    let got = call(
        &mut s,
        3,
        "tools/call",
        json!({"name": "get", "arguments": {"subject": "admin", "koid": koid}}),
    );
    let text = got["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<J>(text).unwrap()["properties"]["body"],
        json!("over tls")
    );
    let _ = std::fs::remove_dir_all(&db);
}

/// A plaintext client on the TLS port never gets a JSON-RPC answer.
#[test]
fn tls002_plaintext_client_is_not_served() {
    let (db, port) = (tmp("002"), free_port());
    let listen = format!("127.0.0.1:{port}");
    let _srv = serve(
        &db,
        &[&["--listen", &listen][..], &strs(&cert_args())].concat(),
        &[],
    );
    let mut s = connect(port);
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    writeln!(
        s,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"token": "tls-tok"}})
    )
    .unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    assert!(
        !String::from_utf8_lossy(&out).contains("\"result\""),
        "a plaintext client was served on the TLS port"
    );
    let _ = std::fs::remove_dir_all(&db);
}

/// Without TLS a non-loopback bind still fails closed (exit 2).
#[test]
fn tls003_plaintext_non_loopback_still_refused() {
    let db = tmp("003");
    let out = Command::new(bin())
        .args(["serve", "--listen", "0.0.0.0:0", "--tcp-token", "t::admin"])
        .arg(&db)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&db);
}

/// The framed native protocol rides the same TLS: HELLO answers HELLO.
#[test]
fn tls004_native_protocol_over_tls() {
    let (db, mcp, native) = (tmp("004"), free_port(), free_port());
    let (l, n) = (format!("127.0.0.1:{mcp}"), format!("0.0.0.0:{native}"));
    let _srv = serve(
        &db,
        &[
            &["--listen", &l, "--native-port", &n][..],
            &strs(&cert_args()),
        ]
        .concat(),
        &[],
    );
    let mut s = tls(native);
    let hello =
        br#"{"protocol_version":1,"capabilities":[],"client":{"name":"tls004","version":"0"}}"#;
    let mut frame = nat::header_bytes(0, 1, nat::HELLO, hello.len() as u32).to_vec();
    frame.extend_from_slice(hello);
    frame.extend_from_slice(&nat::crc32(&frame).to_le_bytes());
    s.write_all(&frame).unwrap();
    s.flush().unwrap();
    let mut h = [0u8; nat::HEADER_LEN];
    s.read_exact(&mut h).expect("the HELLO answer over TLS");
    let parsed = nat::parse_header(&h).expect("a valid response header");
    assert_eq!(parsed.msg_type, nat::HELLO);
    assert_ne!(parsed.flags & nat::FLAG_RESPONSE, 0);
    let _ = std::fs::remove_dir_all(&db);
}

/// The env form (the Docker path): AIKOQL_TLS_CERT / AIKOQL_TLS_KEY.
#[test]
fn tls005_tls_from_the_environment() {
    let (db, port) = (tmp("005"), free_port());
    let listen = format!("0.0.0.0:{port}");
    let env = [
        ("AIKOQL_TLS_CERT", fixture("server.pem")),
        ("AIKOQL_TLS_KEY", fixture("server.key")),
    ];
    let _srv = serve(&db, &["--listen", &listen], &env);
    let mut s = tls(port);
    init(&mut s);
    let _ = std::fs::remove_dir_all(&db);
}

/// A certificate without its key (or the reverse) is a startup error.
#[test]
fn tls006_cert_and_key_go_together() {
    let db = tmp("006");
    let out = Command::new(bin())
        .args([
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--tcp-token",
            "t::admin",
        ])
        .args(["--tls-cert", &fixture("server.pem")])
        .arg(&db)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&db);
}

/// The shutdown drain closes an idle TLS connection promptly (the reader
/// waits outside the TLS lock, so the drain's socket close wakes it).
#[test]
fn tls007_shutdown_drains_an_open_tls_connection() {
    let (db, port) = (tmp("007"), free_port());
    let listen = format!("127.0.0.1:{port}");
    let mut srv = serve(
        &db,
        &[&["--listen", &listen][..], &strs(&cert_args())].concat(),
        &[],
    );
    let mut idle = tls(port);
    init(&mut idle);
    let mut s = tls(port);
    init(&mut s);
    let t0 = Instant::now();
    let _ = call(&mut s, 2, "shutdown", json!({}));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(st) = srv.0.try_wait().unwrap() {
            assert!(st.success() || st.code().is_some(), "server exit: {st:?}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the drain stalled on an open TLS connection"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("tls007: drained in {:?}", t0.elapsed());
    drop(idle);
    let _ = std::fs::remove_dir_all(&db);
}

/// A 200 status line (the server answers HTTP/1.0).
fn ok(resp: &str) -> bool {
    resp.lines()
        .next()
        .is_some_and(|l| l.starts_with("HTTP/1.") && l.contains(" 200 "))
}

/// One HTTP/1.1 request over TLS; the whole response.
fn https(port: u16, req: &str) -> String {
    let mut s = tls(port);
    s.write_all(req.as_bytes()).unwrap();
    s.flush().unwrap();
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            // the response is complete once the server closes, cleanly or not
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The HTTP surface (login, query API, /health) over HTTPS with the same
/// certificate, on a non-loopback bind armed with allow_remote_http + a login.
#[test]
fn tls008_https_surface_on_any_interface() {
    let (db, port) = (tmp("008"), free_port());
    let metrics = format!("0.0.0.0:{port}");
    let env = [
        ("AIKOQL_ALLOW_REMOTE_HTTP", "1".to_string()),
        ("AIKOQL_ADMIN_PASSWORD", "tls-pw".to_string()),
    ];
    let _srv = serve(
        &db,
        &[&["--metrics-addr", &metrics][..], &strs(&cert_args())].concat(),
        &env,
    );
    let _ = connect(port); // listening
    let health = https(
        port,
        "GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    );
    assert!(ok(&health), "GET /health over HTTPS: {health:?}");
    let body = r#"{"username":"admin","password":"tls-pw"}"#;
    let login = https(
        port,
        &format!(
            "POST /api/login HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    );
    assert!(
        ok(&login) && login.contains("\"token\""),
        "login over HTTPS: {login:?}"
    );
    let _ = std::fs::remove_dir_all(&db);
}

/// Remote HTTP without TLS is refused even when armed: the login and the
/// session tokens must not cross the network in plaintext.
#[test]
fn tls009_remote_plaintext_http_refused() {
    let db = tmp("009");
    let mut child = Command::new(bin())
        .args(["serve", "--metrics-addr", "0.0.0.0:0"])
        .arg(&db)
        .env("AIKOQL_ALLOW_REMOTE_HTTP", "1")
        .env("AIKOQL_ADMIN_PASSWORD", "tls-pw")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // a server that starts would serve forever: bound the wait
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("remote plaintext HTTP was served (no refusal within 15 s)");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert_eq!(status.code(), Some(2), "{err}");
    assert!(
        err.contains("--tls-cert"),
        "the refusal names the fix: {err}"
    );
    let _ = std::fs::remove_dir_all(&db);
}

/// The same drain with an idle PLAINTEXT MCP connection open — the Windows
/// leg of tls007: a blocked read on a duplicated handle is not woken by the
/// drain's socket close there, only by the read-timeout wake.
#[test]
fn tls010_shutdown_drains_an_open_plain_connection() {
    let (db, port) = (tmp("010"), free_port());
    let listen = format!("127.0.0.1:{port}");
    let mut srv = serve(&db, &["--listen", &listen], &[]);
    let mut idle = connect(port);
    writeln!(
        idle,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "token": "tls-tok",
                          "clientInfo": {"name": "idle", "version": "0"}}})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(idle.try_clone().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.contains("\"result\""), "idle initialize: {line}");
    let mut s = connect(port);
    writeln!(
        s,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "token": "tls-tok",
                   "clientInfo": {"name": "s", "version": "0"}}})
    )
    .unwrap();
    writeln!(
        s,
        "{}",
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}})
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while srv.0.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "the drain stalled on an open plaintext connection"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(idle);
    let _ = std::fs::remove_dir_all(&db);
}
