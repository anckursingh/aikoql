//! Shared POC-4 T-72 test hygiene: temp paths created by THIS test thread
//! are swept when the thread exits — including `{stem}.`-prefixed sidecars
//! like the `{db}.audit.log` the server writes next to every DB — so a panic
//! mid-test cannot leave scratch DBs or audit logs behind in the shared temp
//! dir. (Per-thread on purpose: the statics are not dropped on Windows MSVC,
//! TLS is; and a child server process must never delete paths it did not
//! create — a thread only ever sweeps what it registered.)

#![allow(dead_code)]

use std::path::{Path, PathBuf};

thread_local! {
    static TEMP_PATHS: std::cell::RefCell<TempSweeper> =
        const { std::cell::RefCell::new(TempSweeper { paths: Vec::new() }) };
}

pub struct TempSweeper {
    pub paths: Vec<PathBuf>,
}

impl Drop for TempSweeper {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = std::fs::remove_file(p);
            let _ = std::fs::remove_dir_all(p);
            // Sidecars created NEXT TO the registered stem (`{stem}.audit.log`):
            // the stem is pid+counter-unique, so a `{stem}.` prefix match is
            // own-files only.
            let Some(name) = p.file_name() else { continue };
            if let Ok(rd) = std::fs::read_dir(p.parent().unwrap_or(Path::new("."))) {
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

/// Register a path this thread created: it (and any `{stem}.`-prefixed
/// sidecar) is removed when the thread exits, even on panic.
pub fn register_temp_path(p: &Path) {
    TEMP_PATHS.with(|t| t.borrow_mut().paths.push(p.to_path_buf()));
}

// ---- spawned-server helpers (the license/users/oidc_sso integration tests) ----

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A port no other helper in this test process has handed out (the server
/// may bind it the moment it is returned).
pub fn free_port() -> u16 {
    static GIVEN: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());
    loop {
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("bind :0")
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

/// Sign a license payload with a fresh TEST keypair → (public key, envelope).
/// Never the production key — its private half is off-repo; the debug-only
/// AIKOQL_LICENSE_PUBLIC_KEY override makes the debug server verify against
/// this test pair.
pub fn sign(org: &str, expires: Option<String>, features: &[&str]) -> (Vec<u8>, Vec<u8>) {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("system RNG");
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("fresh PKCS#8");
    let payload = serde_json::json!({
        "ver": 1,
        "org": org,
        "issued_at": "0",
        "expires_at": expires,
        "features": features,
    });
    let json = serde_json::to_vec(&payload).unwrap();
    let sig = pair.sign(&json);
    let envelope = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(&json),
        URL_SAFE_NO_PAD.encode(sig.as_ref())
    );
    (pair.public_key().as_ref().to_vec(), envelope.into_bytes())
}

/// A spawned `aikoql-mcp serve`, hard-killed on drop (a panicked test must
/// not leak the process). `shutdown` kills it without touching the data dir
/// (a later boot reopens it).
pub struct Server {
    pub child: Child,
    pub port: u16,
    pub db: PathBuf,
    pub log: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// Kill and reap, keeping the data dir for a later boot.
    pub fn shutdown(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::mem::forget(self);
    }
}

/// Spawn `aikoql-mcp serve --metrics-addr 127.0.0.1:{port} [extra] <db>`
/// with a clean enterprise env (license/OIDC/admin vars are removed unless
/// `envs` re-adds them), stderr into `log`.
pub fn spawn_server(db: &Path, log: &Path, envs: &[(&str, &str)], extra: &[&str]) -> Server {
    let port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
        .arg("serve")
        .args(["--metrics-addr", &format!("127.0.0.1:{port}")])
        .args(extra)
        .arg(db)
        .env_remove("AIKOQL_LICENSE")
        .env_remove("AIKOQL_LICENSE_PUBLIC_KEY")
        .env_remove("AIKOQL_ADMIN_PASSWORD")
        .env_remove("AIKOQL_OIDC_MOCK")
        .env_remove("AIKOQL_OIDC_MOCK_CLAIMS")
        .env_remove("AIKOQL_OIDC_ISSUER")
        .env_remove("AIKOQL_OIDC_CLIENT_ID")
        .env_remove("AIKOQL_OIDC_CLIENT_SECRET")
        .env_remove("AIKOQL_OIDC_REDIRECT_URI")
        .env_remove("AIKOQL_OIDC_ROLES_CLAIM")
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::fs::File::create(log).expect("log file")))
        .spawn()
        .expect("spawn aikoql-mcp serve");
    Server {
        child,
        port,
        db: db.to_path_buf(),
        log: log.to_path_buf(),
    }
}

/// One raw HTTP request over plain TCP; the server closes after the
/// response.
pub fn http_req(port: u16, request: &str) -> Result<String, String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(30)))
        .expect("read timeout");
    s.write_all(request.as_bytes()).unwrap();
    s.flush().unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out); // EOF = the response is complete
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// GET /health until it answers.
pub fn http_get(port: u16) -> Result<String, String> {
    http_req(
        port,
        "GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
}

/// Poll /health until the server answers (panics past `deadline`).
pub fn wait_health(port: u16, deadline: Instant) -> String {
    loop {
        if let Ok(resp) = http_get(port) {
            return resp;
        }
        assert!(Instant::now() < deadline, "server never served /health");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll the child until it exits; panic if it is still serving.
pub fn wait_exit(child: &mut Child, deadline: Instant) -> std::process::ExitStatus {
    loop {
        if let Some(st) = child.try_wait().expect("try_wait") {
            return st;
        }
        assert!(Instant::now() < deadline, "server never exited");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll the log file until `needle` appears (panics past `deadline`).
pub fn wait_log(path: &Path, needle: &str, deadline: Instant) {
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no {needle:?} in the log: {text}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
