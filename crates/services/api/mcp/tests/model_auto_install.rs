//! AIKOQL_MODEL_AUTO_INSTALL: opt-in download of the default embedding model
//! at serve start when it is not installed (PRR-3 keeps the default: the
//! runtime never downloads unless asked). The download host here is a closed
//! port, so the failure path runs offline: the server still serves (lexical
//! recall), and /health says what was tried.

use serde_json::Value as J;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("model-auto-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

struct Server(Child, PathBuf);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

/// /health's semantic {state, detail} from a server with an empty model
/// store and an empty Hugging Face cache pointed at a closed port.
fn semantic(tag: &str, auto_install: bool) -> (Server, J) {
    let dir = tmp(tag);
    let port = free_port();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"));
    cmd.arg("serve")
        .args(["--metrics-addr", &format!("127.0.0.1:{port}")])
        .args(["--model-dir", dir.join("models").to_str().unwrap()])
        .arg(dir.join("db"))
        .env("HF_HOME", dir.join("hf"))
        .env("HF_ENDPOINT", format!("http://127.0.0.1:{}", free_port()))
        .env_remove("AIKOQL_MODEL_AUTO_INSTALL")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if auto_install {
        cmd.env("AIKOQL_MODEL_AUTO_INSTALL", "1");
    }
    let srv = Server(cmd.spawn().expect("spawn aikoql-mcp serve"), dir);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut s = loop {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            break s;
        }
        assert!(Instant::now() < deadline, "server never listened");
        std::thread::sleep(Duration::from_millis(50));
    };
    write!(
        s,
        "GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    let _ = s.read_to_string(&mut raw);
    let body = raw.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    let health: J = serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {raw}"));
    (srv, health["semantic"].clone())
}

#[test]
fn ma001_without_the_flag_nothing_is_downloaded_and_the_hint_names_it() {
    let (_srv, sem) = semantic("off", false);
    assert_eq!(sem["state"], "unavailable");
    let detail = sem["detail"].as_str().unwrap();
    assert!(detail.contains("aikoql model install"), "{detail}");
    assert!(detail.contains("AIKOQL_MODEL_AUTO_INSTALL=1"), "{detail}");
}

#[test]
fn ma002_with_the_flag_a_failed_download_still_serves_and_says_so() {
    let (_srv, sem) = semantic("on", true);
    assert_eq!(sem["state"], "unavailable");
    let detail = sem["detail"].as_str().unwrap();
    assert!(detail.contains("auto-install failed"), "{detail}");
}
