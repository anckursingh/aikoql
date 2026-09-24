//! Extracted verbatim from server.rs (PRR-7). P5-M11 (ND-11) adds the
//! server lifecycle: graceful shutdown (drain → cancel → exit), the
//! connection cap, capped frame reads, and the request-timeout wrapper.

use crate::session::*;
use crate::tools::TxnRegistry;
use crate::{
    error, info, thread, warn, Arc, AtomicU64, BufRead, BufReader, HashMap, HashSet, Kernel, Mutex,
    Ordering, TcpListener, TcpStream, J, PROTOCOL_VERSION,
};
// Test-only (the stdio client below) — unused in the bin target.
#[cfg(test)]
use crate::{json, SystemClock};

use crate::dispatcher::*;
use crate::protocol::*;
use aikoql_runtime::streaming::CancellationToken;
use aikoql_storage_v2::engine::StorageAdminApi;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub(crate) static ACTIVE_CONNECTIONS: AtomicU64 = AtomicU64::new(0);
pub(crate) static STREAM_ID: AtomicU64 = AtomicU64::new(0);
/// P5-M11: live client sockets, keyed by stream id — the shutdown drain
/// shutdown(Both)s them so idle handlers blocked in fill_buf wake and close
/// instead of holding the drain to its deadline. Handlers remove their entry
/// on exit, so the registry stays bounded.
static CLIENT_STREAMS: Mutex<Vec<(u64, TcpStream)>> = Mutex::new(Vec::new());
/// P5-M11: set by the `shutdown` method — stops the accept loop and makes
/// every handler close its connection after its current exchange.
pub(crate) static SHUTDOWN_FLAG: AtomicBool = AtomicBool::new(false);
/// P5-M11: tokens of in-flight koql requests — the drain cancels all of
/// them. `run_with_timeout` registers/unregisters; entries are per-query
/// and removed on completion, so the registry stays bounded.
static ACTIVE_QUERIES: Mutex<Vec<CancellationToken>> = Mutex::new(Vec::new());

/// P5-M11: run one request with a wall-clock deadline. On timeout the token
/// is cancelled and the caller gets -32002. The token is registered while
/// the request runs so the shutdown drain can cancel it.
///
/// ponytail: the synchronous interpreter cannot be force-killed mid-query —
/// a non-parking long query finishes on its own after the token is
/// cancelled; the caller is already gone (the rx side was dropped).
pub(crate) fn run_with_timeout<F, T>(request_timeout_secs: u64, f: F) -> Result<T, (i64, String)>
where
    F: FnOnce(CancellationToken) -> Result<T, (i64, String)> + Send + 'static,
    T: Send + 'static,
{
    let token = CancellationToken::new();
    {
        let mut active = ACTIVE_QUERIES.lock().unwrap(); // justified: Mutex poison is unrecoverable
        active.push(token.clone());
    }
    let (tx, rx) = std::sync::mpsc::channel::<Result<T, (i64, String)>>();
    let worker_token = token.clone();
    thread::spawn(move || {
        let r = f(worker_token);
        let _ = tx.send(r); // the waiter may already be gone (timeout)
    });
    match rx.recv_timeout(Duration::from_secs(request_timeout_secs)) {
        Ok(r) => {
            unregister(&token);
            r
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            token.cancel();
            unregister(&token);
            Err((
                -32002,
                format!("request timed out after {request_timeout_secs}s and was cancelled"),
            ))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            // The worker panicked or exited without sending.
            unregister(&token);
            Err((-32603, "request worker exited without a result".into()))
        }
    }
}

fn unregister(token: &CancellationToken) {
    let mut active = ACTIVE_QUERIES.lock().unwrap(); // justified: Mutex poison is unrecoverable
    active.retain(|t| !t.ptr_eq(token));
}
pub(crate) fn handle_tcp_client(
    kernel: &Arc<Kernel>,
    stream: TcpStream,
    db_path: Arc<String>,
    auth: &Arc<TcpAuthTable>,
    rate_limit: Arc<Mutex<crate::rate_limiter::RateLimiter>>,
    admin: Option<Arc<dyn StorageAdminApi>>,
    request_timeout_secs: u64,
    max_connections: u64,
) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        // justified: log-only cosmetic — unknown peer on failure
        .unwrap_or_default();
    // P1-19: CAS admission (the P0-04 pattern) — the slot is RESERVED
    // atomically here, so an accept burst can never push the served count
    // over the cap (sv012). Rejection keeps sv003's frame-before-drop order.
    let admitted = ACTIVE_CONNECTIONS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
            (cur < max_connections).then_some(cur + 1)
        })
        .is_ok();
    if !admitted {
        let mut s = stream;
        write_frame(
            &mut s,
            err_frame(
                &J::Null,
                -32000,
                &format!("server connection limit reached ({max_connections})"),
            ),
        );
        warn!(%peer, "client rejected: connection limit reached ({max_connections})");
        return;
    }
    info!(%peer, "client connected");
    // Register for the shutdown drain (active close wakes idle handlers).
    let sid = STREAM_ID.fetch_add(1, Ordering::Relaxed);
    if let Ok(reg_stream) = stream.try_clone() {
        CLIENT_STREAMS.lock().unwrap().push((sid, reg_stream)); // justified: Mutex poison is unrecoverable
    }
    let Ok(clone) = stream.try_clone() else {
        eprintln!("clone stream failed — dropping connection");
        // The slot was reserved above — release it or the cap leaks.
        ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
        return;
    };
    let mut reader = BufReader::new(clone);
    let writer = Arc::new(Mutex::new(stream));
    let mut sub_ids: HashSet<String> = HashSet::new();
    // R5 (review round 3): the limiter is process-shared and keyed by
    // principal in the dispatcher — not created per connection here.
    // PRR-2: TCP identity comes exclusively from a verified --tcp-token.
    let mut session = McpSession {
        trust_mode: TrustMode::Tcp,
        ..Default::default()
    };
    // P5-M11: connection-scoped transaction handles (sv011).
    let txns: TxnRegistry = Mutex::new(HashMap::new());
    let mut authenticated = false;
    // P5-M11: capped frame reads — an oversized line drops the connection
    // (bounded allocation, never a crash), and a trickling line can still
    // complete. `reader.lines()` reads unbounded, so this replaces it.
    const MAX_FRAME_BYTES: usize = 1024 * 1024;
    let mut line_buf: Vec<u8> = Vec::new();
    'conn: loop {
        line_buf.clear();
        loop {
            let available = match reader.fill_buf() {
                Ok(a) => a,
                Err(_) => break 'conn,
            };
            if available.is_empty() {
                break 'conn; // EOF
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    line_buf.extend_from_slice(&available[..pos]);
                    reader.consume(pos + 1);
                    break; // one complete line in line_buf
                }
                None => {
                    let len = available.len();
                    line_buf.extend_from_slice(available);
                    reader.consume(len);
                    if line_buf.len() > MAX_FRAME_BYTES {
                        warn!(%peer, bytes = line_buf.len(), "oversized frame — dropping connection");
                        break 'conn;
                    }
                    // partial line — re-fill (blocks for the next bytes)
                }
            }
        }
        let line = String::from_utf8_lossy(&line_buf);
        if line.trim().is_empty() {
            continue;
        }
        let msg: J = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(e) => {
                let mut out = writer.lock().unwrap(); // justified: Mutex poison is unrecoverable
                write_frame(
                    &mut *out,
                    err_frame(&J::Null, -32700, &format!("parse error: {}", e)),
                );
                continue;
            }
        };
        // PRR-2 auth gate: only initialize (token check) and ping are allowed
        // before authentication; everything else is rejected and the
        // connection dropped (fail-closed).
        if !authenticated {
            let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
            if method == "initialize" {
                let params = msg.get("params").cloned().unwrap_or(J::Null);
                let token = params.get("token").and_then(|t| t.as_str()).unwrap_or("");
                match auth.lookup(token) {
                    Some(ident) => {
                        session.agent_id = "tcp-agent".into();
                        session.tenant = ident.tenant.clone();
                        session.roles = ident.roles.clone();
                        authenticated = true;
                        info!(%peer, roles = %session.roles.join(","), "TCP client authenticated");
                    }
                    None => {
                        let mut out = writer.lock().unwrap(); // justified: Mutex poison is unrecoverable
                        if let Some(id) = msg.get("id").cloned() {
                            write_frame(
                                &mut *out,
                                err_frame(
                                    &id,
                                    -32001,
                                    "invalid or missing token — pass a --tcp-token value as params.token to initialize",
                                ),
                            );
                        }
                        warn!(%peer, "TCP client rejected: invalid token");
                        break 'conn;
                    }
                }
            } else if method != "ping" {
                let mut out = writer.lock().unwrap(); // justified: Mutex poison is unrecoverable
                if let Some(id) = msg.get("id").cloned() {
                    write_frame(
                        &mut *out,
                        err_frame(
                            &id,
                            -32001,
                            "authentication required — call initialize with a valid token first",
                        ),
                    );
                }
                warn!(%peer, method = %method, "TCP client rejected: unauthenticated");
                break 'conn;
            }
        }
        handle_message(
            kernel,
            &mut sub_ids,
            &writer,
            &rate_limit,
            &db_path,
            &mut session,
            msg,
            admin.as_deref(),
            request_timeout_secs,
            &txns,
        );
        // P5-M11: after a shutdown ack the handler closes its connection —
        // the caller sees EOF right after {"shutting_down": true}.
        if SHUTDOWN_FLAG.load(Ordering::Relaxed) {
            break 'conn;
        }
    }
    ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
    CLIENT_STREAMS.lock().unwrap().retain(|(id, _)| *id != sid); // justified: Mutex poison is unrecoverable
    info!(%peer, "client disconnected");
}
pub(crate) fn run_tcp_listener(
    kernel: Arc<Kernel>,
    listener: TcpListener,
    auth: Arc<TcpAuthTable>,
    db_path: Arc<String>,
    rate_limit: Arc<Mutex<crate::rate_limiter::RateLimiter>>,
    admin: Option<Arc<dyn StorageAdminApi>>,
    request_timeout_secs: u64,
    max_connections: u64,
) {
    info!(
        addr = %listener.local_addr().map(|a| a.to_string()).unwrap_or_default(),
        db = %db_path,
        "aikoql-mcp TCP server ready (token auth required)"
    );
    // P5-M11: nonblocking accept so the shutdown flag can stop the loop —
    // `listener.incoming()` blocks forever and would hang sv001's exit wait.
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    while !SHUTDOWN_FLAG.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                // Windows accepts inherit nonblocking from the listener —
                // the handler's reads must block, so reset it per socket.
                let _ = stream.set_nonblocking(false);
                // P5-M11 connection cap: the handler admits via CAS — the
                // -32000 frame (sv003) is sent from handle_tcp_client.
                let k = kernel.clone();
                let db = db_path.clone();
                let auth = auth.clone();
                let rl = rate_limit.clone();
                let admin = admin.clone();
                thread::spawn(move || {
                    handle_tcp_client(
                        &k,
                        stream,
                        db,
                        &auth,
                        rl,
                        admin,
                        request_timeout_secs,
                        max_connections,
                    )
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => error!("accept error: {}", e),
        }
    }
    // P5-M11 drain: cancel in-flight queries, actively close every remaining
    // socket (wakes idle handlers blocked in fill_buf), then wait for the
    // handlers to exit. The deadline is a backstop only — the handlers are
    // all unblocked now and close promptly.
    {
        let mut active = ACTIVE_QUERIES.lock().unwrap(); // justified: Mutex poison is unrecoverable
        for t in active.iter() {
            t.cancel();
        }
        active.clear();
    }
    {
        let streams = CLIENT_STREAMS.lock().unwrap(); // justified: Mutex poison is unrecoverable
        for (_, s) in streams.iter() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(request_timeout_secs);
    while ACTIVE_CONNECTIONS.load(Ordering::Relaxed) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    info!(
        connections = ACTIVE_CONNECTIONS.load(Ordering::Relaxed),
        "TCP server drained and stopped"
    );
}
pub(crate) fn run_stdio(
    kernel: &Arc<Kernel>,
    db_path: &Arc<String>,
    rate_limit: Arc<Mutex<crate::rate_limiter::RateLimiter>>,
    admin: Option<Arc<dyn StorageAdminApi>>,
    request_timeout_secs: u64,
) {
    info!(db = %db_path, protocol = PROTOCOL_VERSION, "aikoql-mcp ready");
    // A bare terminal run looks like a hang — this is a server, not a REPL.
    // stderr only: stdout carries MCP protocol frames.
    eprintln!(
        "waiting for an MCP client on stdin/stdout \
         (connect one, e.g. `claude mcp add aikoql -- aikoql-mcp serve <db>`; \
         for an interactive prompt run: aikoql-mcp shell)"
    );
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let mut sub_ids: HashSet<String> = HashSet::new();
    let mut session = McpSession::default();
    // P5-M11: the stdio connection gets its own txn handle registry.
    let txns: TxnRegistry = Mutex::new(HashMap::new());
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: J = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(e) => {
                let mut out = stdout.lock().unwrap(); // justified: Mutex poison is unrecoverable
                write_frame(
                    &mut *out,
                    err_frame(&J::Null, -32700, &format!("parse error: {}", e)),
                );
                continue;
            }
        };
        handle_message(
            kernel,
            &mut sub_ids,
            &stdout,
            &rate_limit,
            db_path,
            &mut session,
            msg,
            admin.as_deref(),
            request_timeout_secs,
            &txns,
        );
        // P5-M11: a shutdown ack ends the stdio loop too — the client's
        // stdin close (EOF) then exits the process cleanly.
        if SHUTDOWN_FLAG.load(Ordering::Relaxed) {
            break;
        }
    }
}

#[cfg(test)]
#[cfg(test)]
mod tcp_auth_tests {
    // PRR-2 acceptance matrix: unauthenticated → reject; wrong token → reject;
    // user token → server identity, no escalation; admin → privileged tools;
    // tenant A cannot read tenant B; per-call roles never elevate.
    use super::*;
    use crate::session::TcpAuthTable;
    use std::io::{BufRead, BufReader, Write};

    static DB_SEQ: AtomicU64 = AtomicU64::new(0);

    fn spawn_server(token_specs: &[&str]) -> std::net::SocketAddr {
        spawn_server_with_limit(token_specs, 1000)
    }

    fn spawn_server_with_limit(token_specs: &[&str], max_per_minute: u64) -> std::net::SocketAddr {
        // ponytail: this db stays open in the detached listener thread for
        // the process lifetime, so no sweeper can remove it (Windows locks
        // the dir) — a pid-unique dir per spawn is the accepted leak.
        let db = std::env::temp_dir().join(format!(
            "mcp-tcp-auth-{}-{}",
            std::process::id(),
            DB_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&db);
        let engine = aikoql_storage_v2::AikoqlStorageEngineV2::open(&db).expect("open engine");
        let kernel =
            Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xA9C9).expect("open kernel");
        let specs: Vec<String> = token_specs.iter().map(|s| s.to_string()).collect();
        let auth = Arc::new(TcpAuthTable::parse(&specs).expect("valid token specs"));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let db_path = Arc::new(db.to_str().unwrap().to_string());
        let rate_limit = Arc::new(Mutex::new(crate::rate_limiter::RateLimiter::new(
            true,
            max_per_minute,
        )));
        thread::spawn(move || {
            run_tcp_listener(
                Arc::new(kernel),
                listener,
                auth,
                db_path,
                rate_limit,
                None,
                30,
                1000,
            )
        });
        addr
    }

    struct TcpClient {
        stream: TcpStream,
        reader: BufReader<TcpStream>,
        next_id: u64,
    }

    impl TcpClient {
        fn connect(addr: std::net::SocketAddr) -> Self {
            let stream = TcpStream::connect(addr).expect("connect");
            let reader = BufReader::new(stream.try_clone().unwrap());
            TcpClient {
                stream,
                reader,
                next_id: 1,
            }
        }

        /// Send one request, read one frame. Panics if the connection closes.
        fn req(&mut self, method: &str, params: J) -> J {
            let id = self.next_id;
            self.next_id += 1;
            let line =
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string() + "\n";
            self.stream.write_all(line.as_bytes()).unwrap();
            self.stream.flush().unwrap();
            let mut resp = String::new();
            let n = self.reader.read_line(&mut resp).unwrap();
            if n == 0 {
                panic!("server closed connection before responding to {method}");
            }
            serde_json::from_str(&resp).unwrap()
        }

        fn init(&mut self, token: &str) -> J {
            self.req("initialize", json!({"token": token}))
        }

        fn call(&mut self, tool: &str, args: J) -> J {
            self.req("tools/call", json!({"name": tool, "arguments": args}))
        }

        /// Returns 0 on EOF (server dropped the connection).
        fn read_line_or_eof(&mut self) -> usize {
            let mut buf = String::new();
            self.reader.read_line(&mut buf).unwrap()
        }
    }

    #[test]
    fn tcp_rate_limit_rejects_excess_tool_calls() {
        let addr = spawn_server_with_limit(&["user1:acme:viewer"], 3);
        let mut c = TcpClient::connect(addr);
        let init = c.init("user1");
        assert!(init.get("error").is_none(), "expected auth ok, got {init}");
        for _ in 0..3 {
            let r = c.call("metrics", json!({}));
            assert!(
                r.get("error").is_none(),
                "call under limit must pass, got {r}"
            );
        }
        let r = c.call("metrics", json!({}));
        let msg = r
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or_default();
        assert!(msg.contains("rate limit exceeded"), "got {r}");
        // Error frame, not a drop — the next call is also rejected, not EOF.
        assert!(c.call("metrics", json!({})).get("error").is_some());
    }

    #[test]
    fn tcp_rate_limit_is_shared_across_connections() {
        // R5 (review round 3): the budget is per PRINCIPAL (agent_id:tenant),
        // not per connection — one principal on two sockets still gets one
        // budget, so a second connection cannot double the allowance.
        let addr = spawn_server_with_limit(&["user1:acme:viewer"], 3);
        let mut a = TcpClient::connect(addr);
        let mut b = TcpClient::connect(addr);
        for c in [&mut a, &mut b] {
            let init = c.init("user1");
            assert!(init.get("error").is_none(), "expected auth ok, got {init}");
        }
        for _ in 0..2 {
            assert!(
                a.call("metrics", json!({})).get("error").is_none(),
                "conn A under limit"
            );
        }
        // Conn B shares the same principal budget — one call fills it.
        assert!(
            b.call("metrics", json!({})).get("error").is_none(),
            "conn B shares the remaining budget"
        );
        // Budget exhausted: BOTH connections are now rejected.
        let msg = b
            .call("metrics", json!({}))
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string();
        assert!(msg.contains("rate limit exceeded"), "got {msg}");
        assert!(a.call("metrics", json!({})).get("error").is_some());
    }

    #[test]
    fn tcp_unauthenticated_rejected_and_dropped() {
        let addr = spawn_server(&["s3cret:acme:viewer"]);
        let mut c = TcpClient::connect(addr);
        let resp = c.req("tools/list", J::Null);
        assert!(
            resp.get("error").is_some(),
            "expected auth error, got {resp}"
        );
        assert_eq!(
            c.read_line_or_eof(),
            0,
            "connection must be dropped after rejection"
        );
    }

    #[test]
    fn tcp_wrong_token_rejected_and_dropped() {
        let addr = spawn_server(&["s3cret:acme:viewer"]);
        let mut c = TcpClient::connect(addr);
        let resp = c.init("wrong-token");
        assert!(
            resp.get("error").is_some(),
            "expected auth error, got {resp}"
        );
        assert_eq!(c.read_line_or_eof(), 0, "connection must be dropped");
    }

    #[test]
    fn tcp_user_token_gets_server_identity_and_cannot_elevate() {
        let addr = spawn_server(&["user1:acme:viewer"]);
        let mut c = TcpClient::connect(addr);
        // Correct token → initialize succeeds.
        let resp = c.init("user1");
        assert!(resp.get("result").is_some(), "expected success, got {resp}");
        // Ping still works after auth.
        assert!(c.req("ping", J::Null).get("result").is_some());
        // session/init cannot set tenant/roles (method path, raw params).
        let r = c.req(
            "session/init",
            json!({"agent_id": "mallory", "tenant": "other", "roles": ["admin"]}),
        );
        assert!(
            r.get("error").is_some(),
            "session/init must reject identity fields: {r}"
        );
        // Privileged tool (deploy_program → developer) denied for viewer.
        let r = c.call("deploy_program", json!({"name": "p", "body": "x"}));
        assert!(
            r.get("error").is_some(),
            "viewer must be denied deploy_program: {r}"
        );
        // Per-call roles:["admin"] in arguments must not elevate.
        let r = c.call(
            "deploy_program",
            json!({"name": "p", "body": "x", "roles": ["admin"]}),
        );
        assert!(
            r.get("error").is_some(),
            "per-call admin roles must not elevate: {r}"
        );
        // Non-privileged tool works.
        let r = c.call("metrics", json!({}));
        assert!(r.get("result").is_some(), "metrics should succeed: {r}");
    }

    #[test]
    fn tcp_admin_token_allows_privileged_tools() {
        let addr = spawn_server(&["boss::admin", "user1:acme:viewer"]);
        let mut c = TcpClient::connect(addr);
        let resp = c.init("boss");
        assert!(resp.get("result").is_some(), "expected success, got {resp}");
        // Admin (tenant-less token) may deploy programs.
        let r = c.call("deploy_program", json!({"name": "p", "body": "RETURN 1"}));
        assert!(
            r.get("result").is_some(),
            "admin deploy should succeed: {r}"
        );
        // session/init with only run_id is allowed in TCP mode.
        let r = c.req("session/init", json!({"run_id": "r42"}));
        assert!(
            r.get("result").is_some(),
            "run_id-only session/init should succeed: {r}"
        );
        assert_eq!(r["result"]["session"]["agent_id"], "tcp-agent");
        assert_eq!(r["result"]["session"]["roles"], json!(["admin"]));
    }

    #[test]
    fn tcp_tenant_isolation_across_tokens() {
        let addr = spawn_server(&["userA:tenantA:viewer", "userB:tenantB:viewer"]);
        // Tenant A creates a KO.
        let mut a = TcpClient::connect(addr);
        let resp = a.init("userA");
        assert!(resp.get("result").is_some());
        let r = a.call(
            "remember",
            json!({"type_name": "Note", "properties": {"body": "secret-a"}}),
        );
        assert!(r.get("result").is_some(), "remember should succeed: {r}");
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        let koid: J = serde_json::from_str(text).unwrap();
        let koid = koid["koid"]
            .as_str()
            .unwrap_or_else(|| panic!("remember payload has no koid — call failed: {koid}"))
            .to_string();
        // Tenant A can read it back.
        let r = a.call("get", json!({"koid": koid}));
        assert!(
            r.get("result").is_some(),
            "tenant A must read its own KO: {r}"
        );
        drop(a);
        // Tenant B cannot read it.
        let mut b = TcpClient::connect(addr);
        let resp = b.init("userB");
        assert!(resp.get("result").is_some());
        let r = b.call("get", json!({"koid": koid}));
        assert!(
            r.get("error").is_some() || r["result"]["isError"] == true,
            "tenant B must not read tenant A's KO: {r}"
        );
    }
}
