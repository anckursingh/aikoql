//! P5-M11 (ND-11) — standalone database server: lifecycle + KOQL protocol.
//! sv000–011.
//!
//! The server is the repo-built `aikoql-mcp` binary (a plain-TCP listener
//! speaking framed JSON-RPC). The contract (docs/server-contract.md):
//! KOQL methods ride the same framed transport as MCP behind the same
//! fail-closed token gate; `shutdown` drains in-flight requests (cancelling
//! stragglers) and exits zero; every acked write is durable across clean
//! shutdown AND hard kill (the per-batch group-commit fsync is the
//! durability mechanism — no separate flush call exists); framing errors
//! are answered with -32700 or a dropped connection, never a crash.
//!
//! Observable contract pinned here:
//! - sv000 the contract doc exists and names the whole surface
//! - sv001 clean shutdown: `shutdown` → EOF → exit 0, zero-loss reopen
//! - sv002 restart recovery: a second server process sees the first's writes
//! - sv003 multiple clients: concurrent sessions are isolated, and
//!   --max-connections rejects the overflow connection with -32000
//! - sv004 authentication: unauthenticated/bad-token fail-closed (-32001 +
//!   drop); serve without --tcp-token exits 2
//! - sv005 authorization: KOQL queries are tenant-scoped by the verified
//!   token — one tenant never sees another's objects
//! - sv006 malformed frames: parse garbage → -32700 and the connection
//!   survives; an oversized line drops the connection; the server stays up
//! - sv007 cancellation: client disconnect cancels the in-flight query (the
//!   P5-M4 token) and the server keeps serving
//! - sv008 backpressure: a slow client never stalls or bloats the server —
//!   concurrent clients complete, oversized frames are rejected
//! - sv009 graceful shutdown mid-query: drain cancels parked queries and
//!   exits zero; acked writes survive
//! - sv010 crash recovery: a hard-killed server loses nothing that was
//!   acked (child-kill, rule 5)
//! - sv011 MCP transaction tools (the P5-M10 deferral): begin/stage/commit/
//!   rollback through tools/call with a server-side handle registry
//!
//! The binary must be built first: `cargo build --bin aikoql-mcp`
//! (cargo test does NOT build bins — the txn_crasher trap).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aikoql_kernel::*;
use serde_json::{json, Value as J};

// --- helpers ------------------------------------------------------------------

static PORT_SEQ: AtomicU64 = AtomicU64::new(0);

fn server_bin() -> PathBuf {
    let mut exe = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    exe.push("../../../../target/debug/aikoql-mcp");
    #[cfg(windows)]
    exe.set_extension("exe");
    assert!(
        exe.exists(),
        "aikoql-mcp binary not built at {:?}; run `cargo build --bin aikoql-mcp` first",
        exe
    );
    exe
}

fn free_port() -> u16 {
    // A mostly-free port: bind :0, read the port, drop the listener. The
    // server child retries are bounded by the connect deadline below.
    let l = TcpListener::bind("127.0.0.1:0").expect("bind :0");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

fn tmp_db(name: &str) -> PathBuf {
    let mut db = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    db.push(format!(
        "aikoql_sv_{name}_{}_{}.redb",
        std::process::id(),
        stamp
    ));
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
    db
}

fn tmp_marker(name: &str) -> PathBuf {
    let mut m = std::env::temp_dir();
    let seq = PORT_SEQ.fetch_add(1, Ordering::Relaxed);
    m.push(format!(
        "aikoql_sv_{name}_{}_{}.marker",
        std::process::id(),
        seq
    ));
    let _ = std::fs::remove_file(&m);
    m
}

fn hard_kill(child: &Child) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-9", &child.id().to_string()])
            .status();
    }
}

/// A spawned server child. Drop hard-kills a still-running child — without
/// this a panicked test leaks the process, and on Windows the leaked child
/// keeps the harness pipe open (the suite then never finishes).
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn spawn(db: &std::path::Path, tokens: &[&str], extra: &[&str]) -> Self {
        let port = free_port();
        let mut cmd = Command::new(server_bin());
        cmd.arg("serve")
            .arg("--listen")
            .arg(format!("127.0.0.1:{port}"));
        for t in tokens {
            cmd.arg("--tcp-token").arg(t);
        }
        for e in extra {
            cmd.arg(e);
        }
        cmd.arg(db).stdout(Stdio::null()).stderr(Stdio::null());
        let child = cmd.spawn().expect("spawn aikoql-mcp serve");
        Server { child, port }
    }

    fn spawn_env(
        db: &std::path::Path,
        tokens: &[&str],
        extra: &[&str],
        envs: &[(&str, &str)],
    ) -> Self {
        let port = free_port();
        let mut cmd = Command::new(server_bin());
        cmd.arg("serve")
            .arg("--listen")
            .arg(format!("127.0.0.1:{port}"));
        for t in tokens {
            cmd.arg("--tcp-token").arg(t);
        }
        for e in extra {
            cmd.arg(e);
        }
        cmd.arg(db).stdout(Stdio::null()).stderr(Stdio::null());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawn aikoql-mcp serve");
        Server { child, port }
    }

    fn port(&self) -> u16 {
        self.port
    }

    /// Wait for a graceful exit and assert it was zero.
    fn stop(&mut self, what: &str) {
        let st = wait_exit(&mut self.child, what);
        assert!(st.success(), "{what} must exit 0, got {st}");
    }

    /// Hard-kill the child (crash windows — sv010).
    fn crash(&mut self, what: &str) {
        hard_kill(&self.child);
        let st = self.child.wait().expect("reap child");
        assert!(
            !st.success(),
            "{what}: child should have been killed, not exited"
        );
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            hard_kill(&self.child);
            let _ = self.child.wait();
        }
    }
}

/// Connect with a deadline — the server takes a beat to bind + accept.
/// Every stream carries a read timeout so a missing frame can never hang
/// the suite (a missing frame is the expected RED state).
fn connect(port: u16) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => {
                s.set_read_timeout(Some(Duration::from_secs(30)))
                    .expect("read timeout");
                return s;
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("server on {port} never accepted: {e}"),
        }
    }
}

struct Client {
    stream: TcpStream,
    reader: BufReader<std::net::TcpStream>,
    next_id: u64,
}

impl Client {
    fn new(port: u16, token: &str) -> Self {
        let stream = connect(port);
        let reader = BufReader::new(stream.try_clone().unwrap());
        let mut c = Client {
            stream,
            reader,
            next_id: 1,
        };
        c.init(token);
        c
    }

    fn send(&mut self, method: &str, params: J) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stream, "{frame}").expect("write frame");
        self.stream.flush().expect("flush frame");
        id
    }

    /// Read one response frame (a single line). None = EOF.
    fn recv(&mut self) -> Option<J> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => serde_json::from_str(line.trim()).ok(),
            Err(_) => None,
        }
    }

    /// Send + read the matching response frame.
    fn call(&mut self, method: &str, params: J) -> J {
        let id = self.send(method, params);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "no response to {method} in 30s");
            match self.recv() {
                Some(f) if f.get("id").and_then(|i| i.as_u64()) == Some(id) => return f,
                Some(_) => continue,
                None => panic!("connection EOF awaiting {method} response"),
            }
        }
    }

    fn init(&mut self, token: &str) {
        let r = self.call("initialize", json!({"token": token}));
        assert!(r.get("result").is_some(), "initialize failed: {r}");
    }
}

fn result_of(f: &J) -> &J {
    f.get("result")
        .unwrap_or_else(|| panic!("expected result frame, got {f}"))
}

fn error_code(f: &J) -> Option<i64> {
    f.get("error")
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_i64())
}

/// Poll the child until it exits; hard-kill if the deadline passes.
fn wait_exit(child: &mut Child, what: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(st) = child.try_wait().expect("try_wait") {
            return st;
        }
        assert!(Instant::now() < deadline, "{what} never exited");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_marker(marker: &std::path::Path, server: &mut Server, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if marker.exists() {
            return;
        }
        if Instant::now() > deadline {
            let died = server.child.try_wait().ok().flatten();
            server.crash(what);
            panic!(
                "{what} never reached its marker (marker {:?} absent); child status: {died:?}",
                marker
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Reopen the store after a hard kill (Windows may release the directory
/// lock a beat after taskkill — the txn_contract retry pattern).
fn reopen_after_kill(path: &std::path::Path) -> Kernel {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(engine) = aikoql_storage_v2::AikoqlStorageEngineV2::open(path) {
            if let Ok(k) = Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xBEEF) {
                return k;
            }
        }
        assert!(
            Instant::now() <= deadline,
            "store must reopen after a hard kill (fail-safe)"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

// The server's default backend on a fresh path is aikoql-v2 (a database
// DIRECTORY, not a redb file) — the reopen helper must match that format.
fn reopen(path: &std::path::Path) -> Kernel {
    let engine = aikoql_storage_v2::AikoqlStorageEngineV2::open(path).expect("reopen store");
    Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xBEEF).expect("reopen kernel")
}

fn node_count(k: &Kernel) -> usize {
    k.type_koids("Node").expect("type_koids").len()
}

fn create_node(c: &mut Client, i: i64) -> J {
    let r = c.call(
        "koql/execute",
        json!({"query": format!("CREATE Node {{i: {i}}}")}),
    );
    let res = result_of(&r);
    assert_eq!(res.get("version"), Some(&json!(1)), "create lost: {r}");
    res.clone()
}

fn query_nodes(c: &mut Client) -> usize {
    let r = c.call("koql/query", json!({"query": "MATCH Node RETURN *"}));
    let res = result_of(&r);
    res.get("results")
        .and_then(|a| a.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| panic!("expected results array, got {r}"))
}

// --- sv000 — the contract doc ---------------------------------------------------

#[test]
fn sv000_server_contract_doc() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let doc = std::fs::read_to_string(format!("{manifest}/../../../../docs/server-contract.md"))
        .expect("docs/server-contract.md");
    for needle in [
        "# Server Contract",
        "shutdown",
        "koql/query",
        "koql/execute",
        "request timeout",
        "max_connections",
        "graceful",
        "drain",
        "AIKOQL_QUERY_PARK",
    ] {
        assert!(
            doc.contains(needle),
            "server-contract.md must document {needle:?}"
        );
    }
}

// --- sv001 — clean shutdown, zero loss ------------------------------------------

#[test]
fn sv001_clean_shutdown_is_zero_loss() {
    let db = tmp_db("sv001");
    let mut server = Server::spawn(&db, &["tok1:tenA:user"], &[]);
    let mut c = Client::new(server.port(), "tok1");
    create_node(&mut c, 1);

    let r = c.call("shutdown", json!({}));
    assert_eq!(
        result_of(&r).get("shutting_down"),
        Some(&json!(true)),
        "{r}"
    );

    // EOF follows the shutdown ack, then the process exits zero. The
    // contract: the ack is the LAST frame — anything after it is a failure.
    // (recv's 30s read timeout bounds the wait; stop() below bounds exit.)
    match c.recv() {
        None => {}
        Some(f) => panic!("unexpected frame after shutdown: {f}"),
    }
    server.stop("server after shutdown");

    let k = reopen(&db);
    assert_eq!(node_count(&k), 1, "acked write lost across clean shutdown");
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
}

// --- sv002 — restart recovery ----------------------------------------------------

#[test]
fn sv002_restart_recovery_sees_previous_writes() {
    let db = tmp_db("sv002");
    let mut s1 = Server::spawn(&db, &["tok1:tenA:user"], &[]);
    {
        let mut c = Client::new(s1.port(), "tok1");
        create_node(&mut c, 2);
        c.call("shutdown", json!({}));
        let _ = c.recv(); // drain to EOF
    }
    s1.stop("sv002 first server");

    let mut s2 = Server::spawn(&db, &["tok1:tenA:user"], &[]);
    let mut c2 = Client::new(s2.port(), "tok1");
    assert_eq!(query_nodes(&mut c2), 1, "restart lost the write");
    c2.call("shutdown", json!({}));
    s2.stop("sv002 second server");
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
}

// --- sv003 — multiple clients + connection limit ---------------------------------

#[test]
fn sv003_concurrent_clients_are_isolated_and_limited() {
    let db = tmp_db("sv003");
    let mut server = Server::spawn(
        &db,
        &["tokA:tenA:user", "tokB:tenB:user"],
        &["--max-connections", "2"],
    );
    let mut a = Client::new(server.port(), "tokA");
    let mut b = Client::new(server.port(), "tokB");

    // Concurrent writes in two tenants…
    create_node(&mut a, 1);
    create_node(&mut b, 2);
    // …stay isolated: each tenant sees exactly its own object.
    assert_eq!(query_nodes(&mut a), 1);
    assert_eq!(query_nodes(&mut b), 1);

    // The third connection is refused at the connection limit with -32000.
    let stream = connect(server.port());
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    let n = reader.read_line(&mut line).unwrap_or(0);
    assert!(n > 0, "limit rejection must send a frame");
    let f: J = serde_json::from_str(line.trim()).expect("rejection frame is JSON");
    assert_eq!(error_code(&f), Some(-32000), "connection-limit frame: {f}");

    a.call("shutdown", json!({}));
    server.stop("sv003 server");
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
}

// --- sv004 — authentication --------------------------------------------------------

#[test]
fn sv004_authentication_fails_closed() {
    let db = tmp_db("sv004");
    let mut server = Server::spawn(&db, &["tok1:tenA:user"], &[]);

    // (a) An unauthenticated call is refused and the connection dropped.
    let stream = connect(server.port());
    let mut raw = Client {
        stream: stream.try_clone().unwrap(),
        reader: BufReader::new(stream),
        next_id: 1,
    };
    raw.send("koql/query", json!({"query": "MATCH Node RETURN *"}));
    let f = raw.recv().expect("fail-closed frame");
    assert_eq!(error_code(&f), Some(-32001), "{f}");
    // The connection is dropped — the next read is EOF.
    assert!(raw.recv().is_none(), "connection must be dropped");

    // (b) A bad token is refused the same way.
    let stream = connect(server.port());
    let mut raw = Client {
        stream: stream.try_clone().unwrap(),
        reader: BufReader::new(stream),
        next_id: 1,
    };
    raw.send("initialize", json!({"token": "wrong"}));
    let f = raw.recv().expect("bad-token frame");
    assert_eq!(error_code(&f), Some(-32001), "{f}");
    assert!(raw.recv().is_none(), "bad-token connection must be dropped");

    // (c) serve over TCP without any --tcp-token refuses to start (exit 2).
    let port = free_port();
    let db2 = tmp_db("sv004b");
    let out = Command::new(server_bin())
        .args(["serve", "--listen", &format!("127.0.0.1:{port}")])
        .arg(&db2)
        .output()
        .expect("spawn token-less serve");
    assert_eq!(
        out.status.code(),
        Some(2),
        "token-less TCP serve must exit 2"
    );
    let _ = std::fs::remove_file(&db2);

    // The real server is untouched by all of the above.
    let mut ok = Client::new(server.port(), "tok1");
    assert_eq!(query_nodes(&mut ok), 0);
    ok.call("shutdown", json!({}));
    server.stop("sv004 server");
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
}

// --- sv005 — authorization: tenant-scoped KOQL ------------------------------------

#[test]
fn sv005_koql_query_is_tenant_scoped_by_the_token() {
    let db = tmp_db("sv005");
    let mut server = Server::spawn(&db, &["ta:tenA:user", "tb:tenB:user"], &[]);
    let mut a = Client::new(server.port(), "ta");
    let mut b = Client::new(server.port(), "tb");

    create_node(&mut a, 1);
    assert_eq!(query_nodes(&mut a), 1, "tenA must see its own object");
    assert_eq!(query_nodes(&mut b), 0, "tenB must not see tenA's object");

    a.call("shutdown", json!({}));
    server.stop("sv005 server");
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
}

// --- sv006 — malformed frames never crash the server -------------------------------

#[test]
fn sv006_malformed_frames_never_crash() {
    let db = tmp_db("sv006");
    let mut server = Server::spawn(&db, &["tok1:tenA:user"], &[]);
    let mut c = Client::new(server.port(), "tok1");

    // Garbage line → -32700, and the connection survives. No request is in
    // flight, so the -32700 frame is deterministically the next one (the
    // stream's 30s read timeout bounds the wait).
    writeln!(c.stream, "this is not json").unwrap();
    c.stream.flush().unwrap();
    let f = match c.recv() {
        Some(f) if error_code(&f) == Some(-32700) => f,
        Some(f) => panic!("expected -32700, got {f}"),
        None => panic!("EOF after garbage — must not drop"),
    };
    assert!(f.get("error").unwrap().get("message").is_some());
    // The same connection still works.
    assert_eq!(query_nodes(&mut c), 0);

    // A JSON frame with no method → error, connection survives.
    writeln!(c.stream, "{{\"jsonrpc\":\"2.0\",\"id\":9}}").unwrap();
    c.stream.flush().unwrap();
    let f = c.call("ping", json!({}));
    let _ = result_of(&f);

    // An oversized line (over the frame cap) drops the connection — but the
    // server keeps serving others. The server drops mid-line, so on Windows
    // the write can fail with ConnectionReset (RST) — tolerated, the point
    // is the drop, not the delivery.
    let big = "x".repeat(2 * 1024 * 1024);
    let _ = writeln!(c.stream, "{big}");
    let _ = c.stream.flush();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if c.recv().is_none() {
            break; // dropped, as specified
        }
        if Instant::now() > deadline {
            panic!("oversized line must drop the connection");
        }
    }
    let mut d = Client::new(server.port(), "tok1");
    assert_eq!(
        query_nodes(&mut d),
        0,
        "server must still serve after malformed frames"
    );

    d.call("shutdown", json!({}));
    server.stop("sv006 server");
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
}

// --- sv007 — disconnect cancels the in-flight query ---------------------------------

#[test]
fn sv007_client_disconnect_cancels_the_query() {
    let db = tmp_db("sv007");
    let park = tmp_marker("sv007park");
    let exited = tmp_marker("sv007exit");
    let mut server = Server::spawn_env(
        &db,
        &["tok1:tenA:user"],
        &["--request-timeout-secs", "2"],
        &[
            ("AIKOQL_QUERY_PARK", "armed"),
            ("AIKOQL_QUERY_PARK_MARKER", park.to_str().unwrap()),
            ("AIKOQL_QUERY_EXIT_MARKER", exited.to_str().unwrap()),
        ],
    );

    let mut a = Client::new(server.port(), "tok1");
    create_node(&mut a, 1);
    // This query parks inside the server.
    a.send("koql/query", json!({"query": "MATCH Node RETURN *"}));
    wait_for_marker(&park, &mut server, "sv007 parked query");
    // Client disconnects without reading the response.
    drop(a);

    // Cancellation reaches the query thread (exit marker), and the server
    // keeps serving new clients.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !exited.exists() {
        assert!(
            Instant::now() < deadline,
            "query never cancelled after disconnect"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut b = Client::new(server.port(), "tok1");
    assert_eq!(
        query_nodes(&mut b),
        1,
        "server must keep serving after a cancelled query"
    );

    b.call("shutdown", json!({}));
    server.stop("sv007 server");
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
    let _ = std::fs::remove_file(&park);
    let _ = std::fs::remove_file(&exited);
}

// --- sv008 — backpressure: a slow client is bounded ----------------------------------

#[test]
fn sv008_slow_client_never_blocks_others() {
    let db = tmp_db("sv008");
    let mut server = Server::spawn(&db, &["tok1:tenA:user"], &[]);

    // A slow client fires 30 queries and never reads a byte.
    let mut slow = Client::new(server.port(), "tok1");
    for _ in 0..30 {
        slow.send("koql/query", json!({"query": "MATCH Node RETURN *"}));
    }

    // A concurrent client gets full service.
    let mut fast = Client::new(server.port(), "tok1");
    create_node(&mut fast, 1);
    assert_eq!(
        query_nodes(&mut fast),
        1,
        "slow client must not stall the server"
    );

    fast.call("shutdown", json!({}));
    server.stop("sv008 server");
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
}

// --- sv009 — graceful shutdown mid-query -----------------------------------------------

#[test]
fn sv009_shutdown_drains_and_cancels_mid_query() {
    let db = tmp_db("sv009");
    let park = tmp_marker("sv009park");
    let exited = tmp_marker("sv009exit");
    let mut server = Server::spawn_env(
        &db,
        &["tok1:tenA:user"],
        &["--request-timeout-secs", "3"],
        &[
            ("AIKOQL_QUERY_PARK", "armed"),
            ("AIKOQL_QUERY_PARK_MARKER", park.to_str().unwrap()),
            ("AIKOQL_QUERY_EXIT_MARKER", exited.to_str().unwrap()),
        ],
    );

    // Connection A writes (acked) and then parks a query.
    let mut a = Client::new(server.port(), "tok1");
    create_node(&mut a, 9);
    a.send("koql/query", json!({"query": "MATCH Node RETURN *"}));
    wait_for_marker(&park, &mut server, "sv009 parked query");

    // Connection B requests shutdown while A's query is in flight.
    let mut b = Client::new(server.port(), "tok1");
    b.call("shutdown", json!({}));

    // The drain cancels the parked query and the process exits zero.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !exited.exists() {
        assert!(
            Instant::now() < deadline,
            "drain never cancelled the parked query"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    server.stop("sv009 server");

    // The acked write survived.
    let k = reopen(&db);
    assert_eq!(
        node_count(&k),
        1,
        "acked write lost across drained shutdown"
    );
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
    let _ = std::fs::remove_file(&park);
    let _ = std::fs::remove_file(&exited);
}

// --- sv010 — crash recovery (child-kill, rule 5) ------------------------------------------

#[test]
fn sv010_hard_kill_loses_no_acked_write() {
    let db = tmp_db("sv010");
    let park = tmp_marker("sv010park");
    let mut server = Server::spawn_env(
        &db,
        &["tok1:tenA:user"],
        &[],
        &[
            ("AIKOQL_QUERY_PARK", "armed"),
            ("AIKOQL_QUERY_PARK_MARKER", park.to_str().unwrap()),
        ],
    );

    // Acked write, then park a query in flight.
    let mut a = Client::new(server.port(), "tok1");
    create_node(&mut a, 10);
    a.send("koql/query", json!({"query": "MATCH Node RETURN *"}));
    wait_for_marker(&park, &mut server, "sv010 parked query");

    server.crash("sv010");

    // The acked write is durable across the kill.
    let k = reopen_after_kill(&db);
    assert_eq!(node_count(&k), 1, "acked write lost across a hard kill");
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
    let _ = std::fs::remove_file(&park);
}

// --- sv011 — the MCP transaction tools (P5-M10 deferral) ---------------------------------

#[test]
fn sv011_mcp_transaction_tools_round_trip() {
    let db = tmp_db("sv011");
    let mut server = Server::spawn(&db, &["tok1:tenA:user"], &[]);
    let mut c = Client::new(server.port(), "tok1");

    // begin → stage a create → commit.
    let r = c.call(
        "tools/call",
        json!({"name": "txn_begin", "arguments": {"txn_id": "sv011-t1"}}),
    );
    let res = result_of(&r);
    assert_eq!(res.get("txn_id"), Some(&json!("sv011-t1")), "{r}");
    assert!(res.get("snapshot_ts").is_some(), "{r}");

    let r = c.call(
        "tools/call",
        json!({"name": "txn_stage", "arguments": {
            "txn_id": "sv011-t1",
            "op": {"action": "create", "type_name": "Node", "properties": {"i": 7}}
        }}),
    );
    assert!(result_of(&r).get("staged").is_some(), "{r}");

    // Nothing is visible before commit.
    assert_eq!(query_nodes(&mut c), 0, "staged writes must not be visible");

    let r = c.call(
        "tools/call",
        json!({"name": "txn_commit", "arguments": {"txn_id": "sv011-t1"}}),
    );
    let res = result_of(&r);
    let results = res
        .get("results")
        .and_then(|a| a.as_array())
        .expect("commit results");
    assert_eq!(results.len(), 1, "{r}");
    assert_eq!(res.get("deduped"), Some(&json!(false)), "{r}");
    assert_eq!(query_nodes(&mut c), 1, "committed write must be visible");

    // The idempotent retry: same txn id, SAME body (P5-M20: retry identity
    // is id + body) — recorded no-op.
    let r = c.call(
        "tools/call",
        json!({"name": "txn_begin", "arguments": {"txn_id": "sv011-t1"}}),
    );
    let _ = result_of(&r);
    let r = c.call(
        "tools/call",
        json!({"name": "txn_stage", "arguments": {
            "txn_id": "sv011-t1",
            "op": {"action": "create", "type_name": "Node", "properties": {"i": 7}}
        }}),
    );
    let _ = result_of(&r);
    let r = c.call(
        "tools/call",
        json!({"name": "txn_commit", "arguments": {"txn_id": "sv011-t1"}}),
    );
    let res = result_of(&r);
    assert_eq!(res.get("deduped"), Some(&json!(true)), "{r}");
    assert_eq!(query_nodes(&mut c), 1, "retry must not re-apply");

    // rollback is pure.
    let r = c.call(
        "tools/call",
        json!({"name": "txn_begin", "arguments": {"txn_id": "sv011-t2"}}),
    );
    let _ = result_of(&r);
    let r = c.call(
        "tools/call",
        json!({"name": "txn_stage", "arguments": {
            "txn_id": "sv011-t2",
            "op": {"action": "create", "type_name": "Node", "properties": {"i": 5}}
        }}),
    );
    let _ = result_of(&r);
    let r = c.call(
        "tools/call",
        json!({"name": "txn_rollback", "arguments": {"txn_id": "sv011-t2"}}),
    );
    assert_eq!(result_of(&r).get("rolled_back"), Some(&json!(true)), "{r}");
    assert_eq!(query_nodes(&mut c), 1, "rollback must leave no residue");

    c.call("shutdown", json!({}));
    server.stop("sv011 server");
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
}
