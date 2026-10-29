//! The SDK over TLS, against the real server started with --tls-cert /
//! --tls-key (the test certificates in the MCP crate, SAN IP:127.0.0.1):
//! MCP JSON-RPC (dial_tls) and the native framed protocol
//! (connect_native_tls) both authenticate and call a tool; a server whose
//! certificate the trust anchors do not cover is refused at the dial.
//! Skips unless AIKOQL_MCP_BIN names the server binary (as native.rs).

use aikoql_sdk::{Client, Tls};
use serde_json::json;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn fixture(name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../services/api/mcp/tests/fixtures/tls")
        .join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn fixture_path(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../services/api/mcp/tests/fixtures/tls")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn bin() -> Option<String> {
    match std::env::var_os("AIKOQL_MCP_BIN") {
        Some(b) => Some(b.to_string_lossy().into_owned()),
        None => {
            eprintln!("AIKOQL_MCP_BIN not set — TLS real-server legs skipped");
            None
        }
    }
}

/// A free port, never the same one twice in this process: tests run in
/// parallel and each server binds only after the probe released its port,
/// so two probes handed one port would point one test at the other's
/// server — "connection refused" once that test's server is gone.
fn free_port() -> u16 {
    static GIVEN: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());
    loop {
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("probe port")
            .local_addr()
            .expect("probe addr")
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
    dir: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A TLS server on `flag` (--listen or --native-port); its address.
fn spawn(bin: &str, flag: &str, tag: &str) -> (Server, String) {
    let addr = format!("127.0.0.1:{}", free_port());
    let dir = std::env::temp_dir().join(format!("sdk-tls-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let child = Command::new(bin)
        .arg("serve")
        .arg(dir.join("db"))
        .args([flag, &addr, "--tcp-token", "sdk-tls::admin"])
        .args(["--tls-cert", &fixture_path("server.pem")])
        .args(["--tls-key", &fixture_path("server.key")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn aikoql-mcp");
    let srv = Server { child, dir };
    let deadline = Instant::now() + Duration::from_secs(15);
    while TcpStream::connect(&addr).is_err() {
        assert!(
            Instant::now() < deadline,
            "server did not come up on {addr}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    (srv, addr)
}

async fn remember_and_get(c: &Client) {
    let put = c
        .call_tool(
            "remember",
            Some(json!({"subject": "admin", "type_name": "note", "properties": {"n": 7}})),
        )
        .await
        .expect("remember over TLS");
    let koid = put["koid"].as_str().expect("a koid").to_string();
    let got = c
        .call_tool("get", Some(json!({"subject": "admin", "koid": koid})))
        .await
        .expect("get over TLS");
    assert_eq!(got["properties"]["n"], json!(7));
}

#[tokio::test]
async fn mcp_over_tls() {
    let Some(bin) = bin() else { return };
    let (_srv, addr) = spawn(&bin, "--listen", "mcp");
    let tls = Tls::with_ca(&fixture("ca.pem")).expect("the test CA");
    let c = Client::dial_tls(&addr, &tls)
        .await
        .expect("dial over TLS")
        .with_token("sdk-tls");
    c.initialize().await.expect("initialize over TLS");
    remember_and_get(&c).await;
    c.close().await.unwrap();
}

#[tokio::test]
async fn native_over_tls() {
    let Some(bin) = bin() else { return };
    let (_srv, addr) = spawn(&bin, "--native-port", "native");
    let tls = Tls::with_ca(&fixture("ca.pem")).expect("the test CA");
    let c = Client::connect_native_tls(&addr, &tls)
        .await
        .expect("native HELLO over TLS")
        .with_token("sdk-tls");
    c.initialize().await.expect("native AUTH over TLS");
    remember_and_get(&c).await;
    c.close().await.unwrap();
}

/// A CA that did not sign the server's certificate: refused at the dial,
/// before any token is sent.
#[tokio::test]
async fn an_untrusted_server_is_refused() {
    let Some(bin) = bin() else { return };
    let (_srv, addr) = spawn(&bin, "--listen", "untrusted");
    let other = Tls::with_ca(&fixture("other-ca.pem")).expect("the other CA");
    assert!(
        Client::dial_tls(&addr, &other).await.is_err(),
        "a server outside the trust anchors was accepted"
    );
    assert!(
        Tls::with_ca(b"not a pem").is_err(),
        "a PEM with no CA must be refused"
    );
}
