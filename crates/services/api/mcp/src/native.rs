//! D-15: the native framed protocol listener (§5/§6, ECO-2). Same lifecycle
//! as the MCP TCP listener — nonblocking accept + shutdown flag + connection
//! cap + shared drain — but the wire is `aikoql-native` frames instead of
//! JSON-RPC lines. Every message class funnels into the existing MCP seams
//! (call_tool / execute_stream_query / tool_session_init), so there is
//! exactly ONE implementation of the database surface; the SDK API does not
//! change when the transport changes.
//!
//! The behavior matrix is frozen by `crates/sdk/rust/tests/native.rs` — the
//! D-15 wire oracle: HELLO version gate, auth-before-ops (PING refused with
//! NOT_AUTHENTICATED, a bad token is AUTHENTICATION_FAILED but the
//! connection SURVIVES), unknown types fail safely, oversized/truncated/
//! corrupt frames drop the connection fast, and the stream pump observes a
//! per-connection cancel flag between chunks.

use crate::audit::audit_log;
use crate::session::*;
use crate::tools::TxnRegistry;
use crate::transport::{
    drain_listener, run_with_timeout, ACTIVE_CONNECTIONS, CLIENT_STREAMS, SHUTDOWN_FLAG, STREAM_ID,
};
use crate::{
    error, info, json, thread, warn, Arc, HashMap, Kernel, Mutex, Ordering, Read, TcpListener,
    TcpStream, Write, J,
};
use aikoql_native as nat;
use aikoql_storage_v2::engine::StorageAdminApi;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Sends one response frame: FLAG_RESPONSE + the request's id echoed back
/// (§6 invariant 2 — every response references exactly one request).
fn write_native_frame(w: &mut impl Write, request_id: u64, msg_type: u16, payload: &J) {
    let bytes = serde_json::to_vec(payload).unwrap_or_else(|_| b"null".to_vec());
    if bytes.len() > nat::MAX_PAYLOAD {
        // Defensive: every call site keeps its payload under the cap (the
        // non-stream QUERY arm checks and answers FRAME_TOO_LARGE instead).
        warn!(
            bytes = bytes.len(),
            "native response exceeds the frame cap — dropped"
        );
        return;
    }
    let header = nat::header_bytes(nat::FLAG_RESPONSE, request_id, msg_type, bytes.len() as u32);
    let mut buf = Vec::with_capacity(nat::HEADER_LEN + bytes.len() + 4);
    buf.extend_from_slice(&header);
    buf.extend_from_slice(&bytes);
    let crc = nat::crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    let _ = w.write_all(&buf);
}

fn respond(writer: &Arc<Mutex<TcpStream>>, request_id: u64, msg_type: u16, payload: &J) {
    let mut w = writer.lock().unwrap(); // justified: Mutex poison is unrecoverable
    write_native_frame(&mut *w, request_id, msg_type, payload);
}

/// Reads one full frame with bounded, exact-size reads. `Ok(None)` is a
/// clean EOF before any byte of the frame; any decode failure (truncated,
/// corrupt, oversized — §6 invariants 4/5) closes the connection.
fn read_native_frame(reader: &mut TcpStream) -> Result<Option<(nat::Header, Vec<u8>)>, String> {
    // D-20 (sv013): a blocked read is only woken by the drain's timeout (on
    // Windows, shutdown() on a duplicated socket handle does not wake a
    // pending recv on another handle). WouldBlock/TimedOut => the timeout
    // fired: check the shutdown flag, else keep waiting.
    fn wake(e: &std::io::Error) -> Result<(), Option<String>> {
        match e.kind() {
            std::io::ErrorKind::Interrupted => Ok(()),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                if SHUTDOWN_FLAG.load(Ordering::Relaxed) {
                    Err(None)
                } else {
                    Ok(())
                }
            }
            _ => Err(Some(e.to_string())),
        }
    }
    let mut header = [0u8; nat::HEADER_LEN];
    let mut n = 0;
    while n < header.len() {
        match reader.read(&mut header[n..]) {
            Ok(0) => return Ok(None),
            Ok(k) => n += k,
            Err(e) => match wake(&e) {
                Ok(()) => continue,
                Err(None) => return Ok(None), // shutdown timeout — clean close
                Err(Some(m)) => return Err(format!("header read: {m}")),
            },
        }
    }
    let h = nat::parse_header(&header).map_err(|e| e.to_string())?;
    let mut payload = vec![0u8; h.payload_len];
    let mut n = 0;
    while n < payload.len() {
        match reader.read(&mut payload[n..]) {
            Ok(0) => return Err("truncated payload".into()),
            Ok(k) => n += k,
            Err(e) => match wake(&e) {
                Ok(()) => continue,
                Err(None) => return Ok(None),
                Err(Some(m)) => return Err(format!("payload read: {m}")),
            },
        }
    }
    let mut crc = [0u8; 4];
    let mut n = 0;
    while n < crc.len() {
        match reader.read(&mut crc[n..]) {
            Ok(0) => return Err("truncated checksum".into()),
            Ok(k) => n += k,
            Err(e) => match wake(&e) {
                Ok(()) => continue,
                Err(None) => return Ok(None),
                Err(Some(m)) => return Err(format!("checksum read: {m}")),
            },
        }
    }
    if !nat::verify(&header, &payload, u32::from_le_bytes(crc)) {
        return Err("checksum mismatch".into());
    }
    Ok(Some((h, payload)))
}

/// Pumps the remaining chunks of one stream query, checking the per-
/// connection cancel flag between chunks (§6 invariant 10 — cancellation is
/// observable: QUERY_END records cancelled + the chunks actually sent).
fn pump_native_chunks(
    writer: Arc<Mutex<TcpStream>>,
    request_id: u64,
    stream_id: String,
    remaining: Vec<J>,
    total_chunks: usize,
    cancel: Arc<AtomicBool>,
) {
    let mut seen = 0usize;
    for (i, chunk) in remaining.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let idx = i + 1;
        let done = idx + 1 == total_chunks;
        let payload = json!({"stream_id": stream_id, "chunk": idx, "done": done, "results": chunk});
        let mut w = writer.lock().unwrap(); // justified: Mutex poison is unrecoverable
        write_native_frame(&mut *w, request_id, nat::QUERY_CHUNK, &payload);
        seen += 1;
    }
    let mut w = writer.lock().unwrap(); // justified: Mutex poison is unrecoverable
    write_native_frame(
        &mut *w,
        request_id,
        nat::QUERY_END,
        &json!({
            "stream_id": stream_id,
            "cancelled": cancel.load(Ordering::Relaxed),
            "total_chunks": total_chunks,
            "chunks_seen": seen,
        }),
    );
}

pub(crate) fn handle_native_client(
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
    // P1-19 CAS admission — the same slot-reservation pattern as the MCP
    // listener, so the shared ACTIVE_CONNECTIONS counter stays exact.
    let admitted = ACTIVE_CONNECTIONS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
            (cur < max_connections).then_some(cur + 1)
        })
        .is_ok();
    if !admitted {
        let mut s = stream;
        write_native_frame(
            &mut s,
            0,
            nat::ERROR,
            &json!({
                "code": "CONNECTION_LIMIT",
                "message": format!("server connection limit reached ({max_connections})"),
            }),
        );
        warn!(%peer, "native client rejected: connection limit reached ({max_connections})");
        return;
    }
    info!(%peer, "native client connected");
    // Register for the shutdown drain (active close wakes blocked reads).
    let sid = STREAM_ID.fetch_add(1, Ordering::Relaxed);
    if let Ok(reg) = stream.try_clone() {
        CLIENT_STREAMS.lock().unwrap().push((sid, reg)); // justified: Mutex poison is unrecoverable
    }
    let Ok(mut reader) = stream.try_clone() else {
        eprintln!("clone stream failed — dropping native connection");
        // The slot was reserved above — release it or the cap leaks.
        ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
        return;
    };
    // D-20 (sv013): the read timeout is the shutdown wake — on Windows the
    // drain's shutdown() on the registered clone does NOT wake a blocked
    // recv on this duplicate handle, so without it an idle connection
    // stalls the drain for its whole deadline (read_native_frame checks
    // SHUTDOWN_FLAG on each timeout).
    let _ = reader.set_read_timeout(Some(Duration::from_millis(200)));
    let writer = Arc::new(Mutex::new(stream));
    // PRR-2: identity comes exclusively from a verified --tcp-token.
    let mut session = McpSession {
        trust_mode: TrustMode::Tcp,
        ..Default::default()
    };
    // P5-M11: connection-scoped transaction handles (sv011) — a BEGIN frame
    // and an EXECUTE txn_* tool call on this connection share one registry.
    let txns: TxnRegistry = Mutex::new(HashMap::new());
    let mut authenticated = false;
    let mut session_id: Option<String> = None;
    // The active-stream cancel flag: CANCEL sets it, the pump observes it.
    let cancel = Arc::new(AtomicBool::new(false));
    'conn: loop {
        let (header, payload) = match read_native_frame(&mut reader) {
            Ok(Some(f)) => f,
            Ok(None) => break 'conn,
            Err(e) => {
                warn!(%peer, %e, "native read failed — closing connection");
                break 'conn;
            }
        };
        // HELLO is always allowed — it is the version gate (§6 invariants
        // 7 + 8: mismatch is explicit, negotiation precedes everything).
        if header.msg_type == nat::HELLO {
            let hello: J = serde_json::from_slice(&payload).unwrap_or(J::Null);
            if hello.get("protocol_version").and_then(|v| v.as_u64())
                != Some(nat::PROTOCOL_VERSION as u64)
            {
                respond(
                    &writer,
                    header.request_id,
                    nat::ERROR,
                    &json!({"code": "VERSION_MISMATCH", "message": "unsupported protocol version"}),
                );
                break 'conn;
            }
            session_id.get_or_insert_with(|| format!("native-{sid}"));
            respond(
                &writer,
                header.request_id,
                nat::HELLO,
                &json!({
                    "protocol_version": nat::PROTOCOL_VERSION,
                    "server_version": env!("CARGO_PKG_VERSION"),
                    "capabilities": [],
                    "session_id": session_id.clone().unwrap(),
                }),
            );
            continue;
        }
        // PRR-2 auth gate: only AUTH before authentication; everything else
        // is refused (§6 invariant 9). Unlike the MCP listener, a bad token
        // does NOT drop the connection — the oracle pins a retry.
        if !authenticated {
            if header.msg_type == nat::AUTH {
                let token = serde_json::from_slice::<J>(&payload)
                    .unwrap_or(J::Null)
                    .get("token")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                match auth.lookup(&token) {
                    Some(ident) => {
                        session.agent_id = "tcp-agent".into();
                        session.tenant = ident.tenant.clone();
                        session.roles = ident.roles.clone();
                        authenticated = true;
                        respond(&writer, header.request_id, nat::AUTH, &json!({"ok": true}));
                        info!(%peer, roles = %session.roles.join(","), "native client authenticated");
                    }
                    None => {
                        respond(
                            &writer,
                            header.request_id,
                            nat::ERROR,
                            &json!({"code": "AUTHENTICATION_FAILED", "message": "invalid or missing token"}),
                        );
                        warn!(%peer, "native client rejected: invalid token");
                    }
                }
                continue;
            }
            respond(
                &writer,
                header.request_id,
                nat::ERROR,
                &json!({"code": "NOT_AUTHENTICATED", "message": "authentication required"}),
            );
            continue;
        }
        match header.msg_type {
            nat::PING => respond(
                &writer,
                header.request_id,
                nat::PING,
                &json!({"pong": true}),
            ),
            nat::CLOSE => {
                respond(&writer, header.request_id, nat::CLOSE, &json!({"ok": true}));
                break 'conn;
            }
            nat::CANCEL => {
                // Fire-and-forget (§6): no response frame. The payload
                // carries the target request_id; the per-connection flag
                // cancels the active stream pump.
                cancel.store(true, Ordering::Relaxed);
            }
            nat::EXECUTE => {
                let exec: J = serde_json::from_slice(&payload).unwrap_or(J::Null);
                let tool = exec
                    .get("tool")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = exec.get("args").cloned().unwrap_or(J::Null);
                // R5: ONE rate limiter, process-shared, keyed by principal
                // (mirrors the dispatcher's tools/call gate).
                let key = format!(
                    "{}:{}",
                    session.agent_id,
                    session.tenant.as_deref().unwrap_or("")
                );
                if let Err(max) = rate_limit.lock().unwrap().check(&key) {
                    audit_log(
                        db_path.as_ref(),
                        &session.agent_id,
                        &tool,
                        "denied:rate",
                        &format!("rate limit exceeded (max {max} calls/min)"),
                    );
                    respond(
                        &writer,
                        header.request_id,
                        nat::ERROR,
                        &json!({"code": "RATE_LIMITED", "message": format!("rate limit exceeded (max {max} calls/min)")}),
                    );
                    continue;
                }
                let args = inject_for_session(&args, &session);
                let response = match crate::tool_registry::call_tool(
                    kernel,
                    &tool,
                    &args,
                    db_path.as_ref(),
                    &mut session,
                    admin.as_deref(),
                    &txns,
                ) {
                    // call_tool wraps the envelope: success text = the data
                    // payload, failure text = {ok:false, error:{…}}.
                    Ok(wrapped) => {
                        let text = wrapped["content"][0]["text"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        let data: J = serde_json::from_str(&text).unwrap_or(J::Null);
                        if wrapped.get("isError") == Some(&json!(true)) {
                            data
                        } else {
                            json!({"ok": true, "data": data})
                        }
                    }
                    Err((code, message)) => json!({
                        "ok": false,
                        "error": {"code": code.to_string(), "message": message},
                    }),
                };
                respond(&writer, header.request_id, nat::EXECUTE, &response);
            }
            nat::PREPARE => {
                let prep: J = serde_json::from_slice(&payload).unwrap_or(J::Null);
                let query = prep
                    .get("aikoql")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Validation-only: compile scoped, never execute.
                let roles: Vec<String> = session.roles.clone();
                let response = match aikoql_compiler::parser::compile_scoped(
                    &query,
                    &session.agent_id,
                    &roles,
                    session.tenant.as_deref(),
                ) {
                    Ok(_) => json!({"prepared": true}),
                    Err(e) => json!({"prepared": false, "error": e.to_string()}),
                };
                respond(&writer, header.request_id, nat::PREPARE, &response);
            }
            nat::QUERY => {
                let args = inject_for_session(
                    &serde_json::from_slice::<J>(&payload).unwrap_or(J::Null),
                    &session,
                );
                let query = args
                    .get("query")
                    .and_then(|q| q.as_str())
                    .unwrap_or("")
                    .to_string();
                let stream = args
                    .get("stream")
                    .and_then(|s| s.as_bool())
                    .unwrap_or(false);
                let subject = args
                    .get("subject")
                    .and_then(|s| s.as_str())
                    .unwrap_or("tcp-agent")
                    .to_string();
                let roles: Vec<String> = session.roles.clone();
                let tenant = session.tenant.clone();
                cancel.store(false, Ordering::Relaxed);
                let k = kernel.clone();
                let request_id = header.request_id;
                // P5-M11: the same wall-clock deadline wrapper as the MCP
                // koql/query path (the synchronous interpreter cannot be
                // force-killed — see run_with_timeout).
                let result = run_with_timeout(request_timeout_secs, move |_token| {
                    crate::tools::query::execute_stream_query(
                        &k,
                        &query,
                        &subject,
                        &roles,
                        tenant.as_deref(),
                    )
                    .map_err(|e| (-32603, e))
                });
                match result {
                    Ok((chunks, stream_id)) => {
                        let total_chunks = chunks.len();
                        let mut it = chunks.into_iter();
                        let head_rows = it.next().unwrap_or_else(|| json!([]));
                        if !stream {
                            // One frame, every row (§6 non-stream shape) —
                            // the head is NOT sent separately here. Chunks
                            // are row arrays; splice them into one list.
                            let mut all: Vec<J> = Vec::new();
                            for chunk in std::iter::once(head_rows).chain(it) {
                                if let Some(rows) = chunk.as_array() {
                                    all.extend(rows.iter().cloned());
                                }
                            }
                            let payload = json!({"results": all});
                            if serde_json::to_vec(&payload).map(|b| b.len()).unwrap_or(0)
                                > nat::MAX_PAYLOAD
                            {
                                respond(
                                    &writer,
                                    request_id,
                                    nat::ERROR,
                                    &json!({"code": "FRAME_TOO_LARGE", "message": "result exceeds the 1 MiB frame cap — use stream:true"}),
                                );
                            } else {
                                respond(&writer, request_id, nat::QUERY, &payload);
                            }
                        } else {
                            let head = json!({
                                "stream_id": stream_id,
                                "chunk": 0,
                                "total_chunks": total_chunks,
                                "results": head_rows,
                            });
                            respond(&writer, request_id, nat::QUERY, &head);
                            if total_chunks > 1 {
                                let remaining: Vec<J> = it.collect();
                                let w2 = writer.clone();
                                let cancel2 = cancel.clone();
                                thread::spawn(move || {
                                    pump_native_chunks(
                                        w2,
                                        request_id,
                                        stream_id,
                                        remaining,
                                        total_chunks,
                                        cancel2,
                                    )
                                });
                            } else {
                                // One-chunk stream: the head IS the data; the
                                // end marker still arrives so the client's read
                                // loop terminates.
                                respond(
                                    &writer,
                                    request_id,
                                    nat::QUERY_END,
                                    &json!({"stream_id": stream_id, "cancelled": false, "total_chunks": total_chunks, "chunks_seen": 0}),
                                );
                            }
                        }
                    }
                    Err((_code, message)) => {
                        respond(
                            &writer,
                            request_id,
                            nat::ERROR,
                            &json!({"code": "QUERY_FAILED", "message": message}),
                        );
                    }
                }
            }
            nat::BEGIN | nat::COMMIT | nat::ROLLBACK => {
                // The §6 txn classes funnel into txn_dispatch through the
                // same call_tool seam the SDKs use — one txn implementation.
                let tool = match header.msg_type {
                    nat::BEGIN => "txn_begin",
                    nat::COMMIT => "txn_commit",
                    _ => "txn_rollback",
                };
                let args = inject_for_session(
                    &serde_json::from_slice::<J>(&payload).unwrap_or(J::Null),
                    &session,
                );
                match crate::tool_registry::call_tool(
                    kernel,
                    tool,
                    &args,
                    db_path.as_ref(),
                    &mut session,
                    admin.as_deref(),
                    &txns,
                ) {
                    Ok(wrapped) => {
                        let text = wrapped["content"][0]["text"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        let data: J = serde_json::from_str(&text).unwrap_or(J::Null);
                        if wrapped.get("isError") == Some(&json!(true)) {
                            // The oracle pins the ERROR frame code "INTERNAL".
                            let code = data["error"]["code"]
                                .as_str()
                                .unwrap_or("INTERNAL")
                                .to_string();
                            let message =
                                data["error"]["message"].as_str().unwrap_or("").to_string();
                            respond(
                                &writer,
                                header.request_id,
                                nat::ERROR,
                                &json!({"code": code, "message": message}),
                            );
                        } else {
                            // The pins sit at the TOP level (txn_id/
                            // snapshot_ts/results/…), plus ok:true.
                            let mut top = data.as_object().cloned().unwrap_or_default();
                            top.insert("ok".into(), json!(true));
                            respond(&writer, header.request_id, header.msg_type, &J::Object(top));
                        }
                    }
                    Err((code, message)) => respond(
                        &writer,
                        header.request_id,
                        nat::ERROR,
                        &json!({"code": code.to_string(), "message": message}),
                    ),
                }
            }
            other => {
                // §6 invariant 6: unknown types fail safely — the session
                // survives.
                respond(
                    &writer,
                    header.request_id,
                    nat::ERROR,
                    &json!({"code": "UNKNOWN_MESSAGE", "message": format!("unknown message type {other}")}),
                );
            }
        }
        // P5-M11: after a shutdown ack the handler closes its connection.
        if SHUTDOWN_FLAG.load(Ordering::Relaxed) {
            break 'conn;
        }
    }
    ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
    CLIENT_STREAMS.lock().unwrap().retain(|(id, _)| *id != sid); // justified: Mutex poison is unrecoverable
    info!(%peer, "native client disconnected");
}

pub(crate) fn run_native_listener(
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
        "aikoql-mcp native server ready (framed protocol, token auth required)"
    );
    // The same P5-M11 nonblocking accept loop as run_tcp_listener.
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    while !SHUTDOWN_FLAG.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let k = kernel.clone();
                let db = db_path.clone();
                let auth = auth.clone();
                let rl = rate_limit.clone();
                let admin = admin.clone();
                thread::spawn(move || {
                    handle_native_client(
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
            Err(e) => error!("native accept error: {}", e),
        }
    }
    drain_listener(request_timeout_secs);
    info!(
        connections = ACTIVE_CONNECTIONS.load(Ordering::Relaxed),
        "native listener drained and stopped"
    );
}
