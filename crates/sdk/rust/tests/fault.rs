//! D-16 fault matrix RED (§18, testing plan §7): one fault proxy in front
//! of a real server — drop/delay/duplicate/reorder/truncate/corrupt/
//! inject-notify/stale/close/half-close/slow/oversized — and every SDK
//! must pass the same matrix. The proxy binary (`aikoql-fault-proxy`,
//! one instance = one fault mode) does not exist yet: every leg fails at
//! spawn. RED by construction until the proxy lands.
//!
//! Frame accounting (the native wire, docs/NATIVE-PROTOCOL.md): the
//! client's HELLO + its response are frames #1, initialize's AUTH + its
//! response are #2, the first health EXECUTE + its response are #3, and
//! the follow-up health is #4. Every leg runs the same shape — connect
//! and initialize clean, then the fault strikes the victim call (#3)
//! and the follow-up (#4) pins how the client survives.
//!
//! The contracts GREEN must make hold (proxy modes + client hardening):
//!   drop-request / drop-response → the victim gets the frozen retryable
//!     TIMEOUT and the follow-up succeeds (the connection survives)
//!   delay-response → tight deadline = TIMEOUT; generous deadline = Ok
//!   duplicate-response → the duplicate is skipped by id correlation,
//!     both calls succeed
//!   reorder-response → the victim times out (its response was held
//!     back) and the follow-up skips the out-of-order frame and succeeds
//!   truncate-frame → fast Io error, then the connection is poisoned
//!     (a mid-frame EOF cannot be resynchronized) → UNAVAILABLE
//!   corrupt-frame → fast checksum-mismatch (InvalidData) error, then
//!     poisoned → UNAVAILABLE
//!   inject-notification → a well-formed frame without the response flag
//!     is skipped (the §6 wire has no notification class; MCP-parity
//!     with the CI-16 "call() skips non-response frames" fix) — both
//!     calls succeed
//!   inject-stale-response → the replayed old response is skipped, both
//!     calls succeed
//!   close-connection / half-close → the follow-up fails fast (never
//!     TIMEOUT, never a hang) and the client latches closed →
//!     UNAVAILABLE from then on
//!   slow-server → tight deadline = TIMEOUT
//!   oversized-response → the client rejects the frame from its header
//!     BEFORE allocating the claimed payload — Mcp(FRAME_TOO_LARGE),
//!     never TIMEOUT (§19: a malicious server cannot cause unbounded
//!     client memory)

use aikoql_sdk::{Client, Error};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Kills the child on drop — every leg leaves no orphans behind.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Server {
    addr: String,
    _child: Killed,
    dir: std::path::PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn mcp_bin() -> Option<String> {
    match std::env::var_os("AIKOQL_MCP_BIN") {
        Some(b) => Some(b.to_string_lossy().into_owned()),
        None => {
            eprintln!("AIKOQL_MCP_BIN not set — fault-matrix real-server legs skipped");
            None
        }
    }
}

fn free_addr() -> String {
    let probe = TcpListener::bind("127.0.0.1:0").expect("probe port");
    let addr = probe.local_addr().expect("probe addr").to_string();
    drop(probe);
    addr
}

fn spawn_server(bin: &str, token: &str) -> Server {
    let addr = free_addr();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("fault-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let db = dir.join("db.aikoql"); // does not exist → auto-create (aikoql-v2)
    let child = Command::new(bin)
        .arg("serve")
        .arg(&db)
        .arg("--native-port")
        .arg(&addr)
        .arg("--tcp-token")
        .arg(format!("{token}::admin"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn aikoql-mcp with --native-port");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if TcpStream::connect(&addr).is_ok() {
            return Server {
                addr,
                _child: Killed(child),
                dir,
            };
        }
        assert!(
            Instant::now() < deadline,
            "server did not come up on {addr}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The D-16 fault proxy: one instance, one fault mode, one client
/// connection. Spawned with `--listen/--target/--mode ...`; RED by
/// construction — the binary does not exist until the GREEN lands.
fn spawn_proxy(server_addr: &str, args: &[&str]) -> (Killed, String) {
    let listen = free_addr();
    let bin = proxy_bin();
    if !bin.exists() {
        panic!(
            "aikoql-fault-proxy missing at {} — D-16 RED: the §18 fault proxy has not landed yet",
            bin.display()
        );
    }
    let child = Command::new(&bin)
        .arg("--listen")
        .arg(&listen)
        .arg("--target")
        .arg(server_addr)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn aikoql-fault-proxy");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if TcpStream::connect(&listen).is_ok() {
            return (Killed(child), listen);
        }
        assert!(
            Instant::now() < deadline,
            "proxy did not come up on {listen}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn proxy_bin() -> PathBuf {
    if let Some(b) = std::env::var_os("AIKOQL_FAULT_PROXY") {
        return PathBuf::from(b);
    }
    // crates/sdk/rust → the workspace root's target dir.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../../target/debug/aikoql-fault-proxy");
    if cfg!(windows) {
        p.set_extension("exe");
    }
    p
}

/// connect + initialize through the proxy: frames #1 (HELLO) and #2
/// (AUTH) flow clean; the fault legs target #3/#4.
async fn victim_client(addr: &str) -> Client {
    let c = Client::connect_native(addr)
        .await
        .expect("connect_native must dial + HELLO through the proxy");
    let c = c.with_token("conformance".to_string());
    c.initialize().await.expect("initialize must AUTH");
    c
}

fn is_timeout(e: &Error) -> bool {
    matches!(e, Error::Mcp(m) if m.code == "TIMEOUT")
}

fn is_unavailable(e: &Error) -> bool {
    matches!(e, Error::Mcp(m) if m.code == "UNAVAILABLE")
}

fn is_frame_too_large(e: &Error) -> bool {
    matches!(e, Error::Mcp(m) if m.code == "FRAME_TOO_LARGE")
}

// ---- the 13 §18 fault legs ------------------------------------------------

/// drop-request: the proxy drops the victim's request — the server never
/// sees it. The victim gets the frozen retryable TIMEOUT; the follow-up
/// proves the connection survived.
#[tokio::test]
async fn fault_drop_request() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) = spawn_proxy(&srv.addr, &["--mode", "drop-request", "--n", "3"]);
    let c = victim_client(&px_addr).await;
    let err = aikoql_sdk::with_deadline(Duration::from_secs(1), c.health())
        .await
        .expect_err("the dropped request must not be answered");
    assert!(is_timeout(&err), "expected TIMEOUT, got {err}");
    c.health().await.expect("the follow-up must succeed — only frame 3 was dropped");
}

/// drop-response: the server answers but the proxy eats the response.
/// TIMEOUT for the victim, success for the follow-up.
#[tokio::test]
async fn fault_drop_response() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) = spawn_proxy(&srv.addr, &["--mode", "drop-response", "--n", "3"]);
    let c = victim_client(&px_addr).await;
    let err = aikoql_sdk::with_deadline(Duration::from_secs(1), c.health())
        .await
        .expect_err("the dropped response must not satisfy the call");
    assert!(is_timeout(&err), "expected TIMEOUT, got {err}");
    c.health().await.expect("the follow-up must succeed — only frame 3 was dropped");
}

/// delay-response: a tight deadline times out; a generous one still gets
/// the answer (delay is not drop).
#[tokio::test]
async fn fault_delay_response() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) =
        spawn_proxy(&srv.addr, &["--mode", "delay-response", "--from", "3", "--delay-ms", "400"]);
    let c = victim_client(&px_addr).await;
    let err = aikoql_sdk::with_deadline(Duration::from_millis(100), c.health())
        .await
        .expect_err("a 400ms delay must beat a 100ms deadline");
    assert!(is_timeout(&err), "expected TIMEOUT, got {err}");
    // a fresh client, no deadline: the delayed response still arrives.
    let (_px2, px_addr2) =
        spawn_proxy(&srv.addr, &["--mode", "delay-response", "--from", "3", "--delay-ms", "400"]);
    let c2 = victim_client(&px_addr2).await;
    c2.health().await.expect("without a deadline the delayed response must still complete");
}

/// duplicate-response: the victim's response arrives twice. The follow-up
/// must skip the duplicate by id correlation and read its own response.
#[tokio::test]
async fn fault_duplicate_response() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) = spawn_proxy(&srv.addr, &["--mode", "duplicate-response", "--n", "3"]);
    let c = victim_client(&px_addr).await;
    c.health().await.expect("the victim gets the first copy");
    c.health().await.expect("the follow-up must skip the duplicate and read its own response");
}

/// reorder-response: the victim's response is held back until the next
/// request passes. The victim times out; the follow-up skips the
/// out-of-order frame (rid < id is never an error, §3.3) and succeeds.
#[tokio::test]
async fn fault_reorder_response() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) = spawn_proxy(&srv.addr, &["--mode", "reorder-response", "--n", "3"]);
    let c = victim_client(&px_addr).await;
    let err = aikoql_sdk::with_deadline(Duration::from_millis(500), c.health())
        .await
        .expect_err("the held-back response must not satisfy the victim");
    assert!(is_timeout(&err), "expected TIMEOUT, got {err}");
    c.health()
        .await
        .expect("the follow-up must skip the reordered frame and succeed");
}

/// truncate-frame: the victim's response loses its tail mid-frame. Fast
/// Io error (never a hang), then the connection is poisoned — a
/// mid-frame EOF cannot be resynchronized — and the next call fails
/// UNAVAILABLE.
#[tokio::test]
async fn fault_truncate_frame() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) =
        spawn_proxy(&srv.addr, &["--mode", "truncate-response", "--n", "3", "--bytes", "10"]);
    let c = victim_client(&px_addr).await;
    let err = aikoql_sdk::with_deadline(Duration::from_secs(2), c.health())
        .await
        .expect_err("a truncated frame must fail the victim");
    assert!(!is_timeout(&err), "a truncated frame must fail fast, not time out — got {err}");
    let err2 = aikoql_sdk::with_deadline(Duration::from_secs(2), c.health())
        .await
        .expect_err("the poisoned connection must not serve another call");
    assert!(
        is_unavailable(&err2),
        "the client must latch closed after a mid-frame EOF, got {err2}"
    );
}

/// corrupt-frame: one payload byte flipped — the checksum fails. Fast
/// InvalidData, then poisoned → UNAVAILABLE (an integrity failure is a
/// reason to distrust the connection, §20 no parser confusion).
#[tokio::test]
async fn fault_corrupt_frame() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) = spawn_proxy(&srv.addr, &["--mode", "corrupt-response", "--n", "3"]);
    let c = victim_client(&px_addr).await;
    let err = aikoql_sdk::with_deadline(Duration::from_secs(2), c.health())
        .await
        .expect_err("a corrupted frame must fail the victim");
    assert!(!is_timeout(&err), "a checksum mismatch must fail fast, not time out — got {err}");
    let err2 = aikoql_sdk::with_deadline(Duration::from_secs(2), c.health())
        .await
        .expect_err("the poisoned connection must not serve another call");
    assert!(
        is_unavailable(&err2),
        "the client must latch closed after an integrity failure, got {err2}"
    );
}

/// inject-notification: a well-formed frame without the response flag.
/// The §6 wire has no notification class, so the client must skip the
/// frame (MCP parity — CI-16: "call() skips non-response frames") and
/// both calls succeed.
#[tokio::test]
async fn fault_inject_notification() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) = spawn_proxy(&srv.addr, &["--mode", "inject-notification", "--after", "3"]);
    let c = victim_client(&px_addr).await;
    c.health().await.expect("the victim must not see the injected frame");
    c.health()
        .await
        .expect("the follow-up must skip the injected frame and succeed");
}

/// inject-stale-response: the HELLO response (#1) replayed after the
/// victim's response. The follow-up skips the stale id and succeeds.
#[tokio::test]
async fn fault_inject_stale_response() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) = spawn_proxy(&srv.addr, &["--mode", "inject-stale-response", "--after", "3"]);
    let c = victim_client(&px_addr).await;
    c.health().await.expect("the victim must succeed before the replay lands");
    c.health().await.expect("the follow-up must skip the stale id and succeed");
}

/// close-connection: the proxy closes the socket after the victim's
/// response. The follow-up fails fast (never TIMEOUT) and the client
/// latches closed → UNAVAILABLE.
#[tokio::test]
async fn fault_close_connection() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) = spawn_proxy(&srv.addr, &["--mode", "close-after", "--n", "3"]);
    let c = victim_client(&px_addr).await;
    c.health().await.expect("the victim must complete before the close");
    let err = aikoql_sdk::with_deadline(Duration::from_secs(2), c.health())
        .await
        .expect_err("the closed connection must fail the follow-up");
    assert!(!is_timeout(&err), "a closed connection must fail fast, not time out — got {err}");
    let err2 = aikoql_sdk::with_deadline(Duration::from_secs(2), c.health())
        .await
        .expect_err("the closed connection must not serve a third call");
    assert!(
        is_unavailable(&err2),
        "the client must latch closed after the connection drops, got {err2}"
    );
}

/// half-close: the proxy shuts its write side after the victim's
/// response — the client can still write but reads EOF. Same contract
/// as close: fast failure, then UNAVAILABLE.
#[tokio::test]
async fn fault_half_close() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) = spawn_proxy(&srv.addr, &["--mode", "half-close-after", "--n", "3"]);
    let c = victim_client(&px_addr).await;
    c.health().await.expect("the victim must complete before the half-close");
    let err = aikoql_sdk::with_deadline(Duration::from_secs(2), c.health())
        .await
        .expect_err("the half-closed connection must fail the follow-up");
    assert!(!is_timeout(&err), "a half-close must fail fast, not time out — got {err}");
    let err2 = aikoql_sdk::with_deadline(Duration::from_secs(2), c.health())
        .await
        .expect_err("the half-closed connection must not serve a third call");
    assert!(
        is_unavailable(&err2),
        "the client must latch closed after the EOF, got {err2}"
    );
}

/// slow-server: the response drips in at 16 bytes per 100ms. A tight
/// deadline must produce the frozen retryable TIMEOUT.
#[tokio::test]
async fn fault_slow_server() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) = spawn_proxy(
        &srv.addr,
        &["--mode", "slow-server", "--from", "3", "--bytes", "16", "--delay-ms", "100"],
    );
    let c = victim_client(&px_addr).await;
    let err = aikoql_sdk::with_deadline(Duration::from_millis(300), c.health())
        .await
        .expect_err("a 16B/100ms drip must beat a 300ms deadline");
    assert!(is_timeout(&err), "expected TIMEOUT, got {err}");
}

/// oversized-response: the proxy rewrites the response header to claim a
/// 64 MiB payload and sends nothing else. The client must reject the
/// frame from its header BEFORE allocating the claimed payload —
/// Mcp(FRAME_TOO_LARGE), never TIMEOUT (§19: a malicious server cannot
/// cause unbounded client memory).
#[tokio::test]
async fn fault_oversized_response() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_server(&bin, "fault");
    let (_px, px_addr) = spawn_proxy(
        &srv.addr,
        &["--mode", "oversized-response", "--n", "3", "--claim", "67108864"],
    );
    let c = victim_client(&px_addr).await;
    let err = aikoql_sdk::with_deadline(Duration::from_secs(5), c.health())
        .await
        .expect_err("an oversized response must be rejected");
    assert!(
        is_frame_too_large(&err),
        "the client must reject the frame from its header with FRAME_TOO_LARGE \
         before allocating 64 MiB — got {err}"
    );
}
