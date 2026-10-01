//! The remote client: MCP JSON-RPC over TCP — or the D-15 native framed
//! protocol (connect_native). Mirrors the Go SDK's wire layer
//! (crates/sdk/go/aikoql.go) — newline frames, id correlation, the
//! tools/call envelope — and the frozen §3.3 semantics: a response with a
//! smaller id is skipped, a larger id is PROTOCOL_ERROR, deadline reads
//! map to the retryable TIMEOUT, and a call on a closed client is
//! UNAVAILABLE before the transport is touched. The transport is private
//! to the Client: the D-15 native protocol replacement touches only this
//! file (the canonical API does not change when the transport changes).

use crate::error::{Error, McpError};
use aikoql_native as nat;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;

/// The oldest aikoql-mcp server this SDK will talk to (the ND-12 version
/// contract, mirrored from the Go and Python SDKs).
pub const MIN_SERVER_VERSION: &str = "0.2.0";

const DIAL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize)]
struct RpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Deserialize)]
struct RpcResponse {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    code: Value,
    message: String,
}

impl RpcError {
    fn mcp_error(self) -> McpError {
        // The RPC-level error only carries code/message; string-encoded
        // codes and numbers both normalize to their string form.
        let code = match &self.code {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            other => other.to_string(),
        };
        let code = if code.is_empty() {
            "INTERNAL".into()
        } else {
            code
        };
        McpError {
            code,
            message: self.message,
            retryable: false,
            suggestion: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ClientConfig {
    token: Option<String>,
    name: String,
    version: String,
}

struct Inner {
    io: Mutex<Option<BufReader<TcpStream>>>,
    next_id: AtomicU64,
    closed: AtomicBool,
    /// Set once by connect_native — every call after that speaks §6 frames.
    native: AtomicBool,
    cfg: std::sync::Mutex<ClientConfig>,
}

/// One MCP JSON-RPC connection to an aikoql-mcp server. Clone shares the
/// connection; calls serialize over one mutex (one in-flight call per
/// connection — a stream holds it for its whole life).
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    /// Opens a TCP connection ("host:port"). The handshake is separate
    /// (initialize) so a pooled client can dial first and authenticate
    /// later; every aikoql-mcp TCP server requires the token.
    pub async fn dial(addr: &str) -> Result<Client, Error> {
        let stream = tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| McpError::deadline())?
            .map_err(Error::Io)?;
        Ok(Client {
            inner: Arc::new(Inner {
                io: Mutex::new(Some(BufReader::new(stream))),
                next_id: AtomicU64::new(0),
                closed: AtomicBool::new(false),
                native: AtomicBool::new(false),
                cfg: std::sync::Mutex::new(ClientConfig {
                    token: None,
                    name: "aikoql-rust-sdk".into(),
                    version: "0.2.0".into(),
                }),
            }),
        })
    }

    /// Opens a connection on the D-15 native framed protocol ("host:port")
    /// and performs the HELLO handshake: the protocol version and the
    /// ND-12 server version are both enforced here, before any call. The
    /// token still arrives at initialize (the AUTH frame) — the API surface
    /// is exactly dial + initialize, only the transport differs.
    pub async fn connect_native(addr: &str) -> Result<Client, Error> {
        let client = Self::dial(addr).await?;
        let (name, version) = {
            let cfg = client.inner.cfg.lock().unwrap();
            (cfg.name.clone(), cfg.version.clone())
        };
        let mut guard = client.inner.io.lock().await;
        let io = guard
            .as_mut()
            .ok_or_else(|| Error::Mcp(McpError::unavailable()))?;
        let id = client.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let hello = serde_json::to_vec(&serde_json::json!({
            "protocol_version": nat::PROTOCOL_VERSION,
            "capabilities": [],
            "client": {"name": name, "version": version},
        }))
        .map_err(Error::Json)?;
        native_send(io, id, nat::HELLO, &hello).await?;
        let (mt, rid, payload) = native_recv(io).await?;
        if rid != id {
            return Err(Error::Mcp(McpError::protocol_error(id, rid)));
        }
        if mt == nat::ERROR {
            return Err(Error::Mcp(native_error(&payload)));
        }
        if mt != nat::HELLO {
            return Err(Error::Mcp(McpError::protocol_error(id, mt as u64)));
        }
        let hello: Value = serde_json::from_slice(&payload).map_err(Error::Json)?;
        if hello.get("protocol_version").and_then(|v| v.as_u64())
            != Some(nat::PROTOCOL_VERSION as u64)
        {
            return Err(Error::Mcp(McpError::version_mismatch(
                &hello
                    .get("protocol_version")
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            )));
        }
        let server = hello
            .get("server_version")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if version_less(&parse_version(server), &parse_version(MIN_SERVER_VERSION)) {
            return Err(Error::Mcp(McpError::version_mismatch(server)));
        }
        drop(guard);
        client.inner.native.store(true, Ordering::Relaxed);
        Ok(client)
    }

    /// Sends the --tcp-token credential in the initialize handshake
    /// (required by every TCP server since P3-M1).
    pub fn with_token(self, token: impl Into<String>) -> Self {
        self.inner.cfg.lock().unwrap().token = Some(token.into());
        self
    }

    /// Sets the MCP client identity advertised at initialize.
    pub fn with_client_info(self, name: impl Into<String>, version: impl Into<String>) -> Self {
        let mut cfg = self.inner.cfg.lock().unwrap();
        cfg.name = name.into();
        cfg.version = version.into();
        drop(cfg);
        self
    }

    /// Closes the connection. Safe to call more than once.
    pub async fn close(&self) -> Result<(), Error> {
        if self.inner.native.load(Ordering::Relaxed) {
            // A best-effort CLOSE frame before dropping the socket — the
            // server acks it and closes; a dead peer must not fail the
            // close itself.
            let mut guard = self.inner.io.lock().await;
            if !self.inner.closed.swap(true, Ordering::Relaxed) {
                if let Some(io) = guard.as_mut() {
                    let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
                    let _ = native_send(io, id, nat::CLOSE, b"{}").await;
                }
            }
            // Taking the reader drops the TcpStream, closing the socket.
            *guard = None;
            return Ok(());
        }
        self.inner.closed.store(true, Ordering::Relaxed);
        // Taking the reader drops the TcpStream, closing the socket.
        let mut guard = self.inner.io.lock().await;
        *guard = None;
        Ok(())
    }

    /// Sends one JSON-RPC request and returns its result frame, skipping
    /// pushed notifications by id correlation (§3.3).
    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, Error> {
        if self.inner.native.load(Ordering::Relaxed) {
            return self.native_request(method, params).await;
        }
        let mut guard = self.inner.io.lock().await;
        if self.inner.closed.load(Ordering::Relaxed) {
            return Err(Error::Mcp(McpError::unavailable()));
        }
        let io = guard
            .as_mut()
            .ok_or_else(|| Error::Mcp(McpError::unavailable()))?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let frame = serde_json::to_string(&RpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        })
        .map_err(Error::Json)?;
        io.write_all(frame.as_bytes()).await.map_err(Error::Io)?;
        io.write_all(b"\n").await.map_err(Error::Io)?;
        io.flush().await.map_err(Error::Io)?;
        let mut line = String::new();
        loop {
            line.clear();
            let n = io.read_line(&mut line).await.map_err(Error::Io)?;
            if n == 0 {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed by the server",
                )));
            }
            let resp: RpcResponse = match serde_json::from_str(line.trim()) {
                Ok(r) => r,
                Err(_) => continue, // tolerate non-JSON noise frames
            };
            let rid = resp.id.unwrap_or(0);
            if rid < id {
                continue; // id-less, notification, duplicate, or late — never an error
            }
            if rid > id {
                return Err(Error::Mcp(McpError::protocol_error(id, rid)));
            }
            if let Some(e) = resp.error {
                return Err(Error::Mcp(e.mcp_error()));
            }
            return Ok(resp.result.unwrap_or(Value::Null));
        }
    }

    /// The native transport for one call: initialize maps to the AUTH
    /// frame, tools/call unwraps {name, arguments}, anything else is an
    /// EXECUTE with the method as the tool. The response is the {ok, data,
    /// error} envelope — an ERROR frame or ok:false is an McpError, and
    /// everything else returns data (the whole payload when there is no
    /// data field).
    async fn native_request(&self, method: &str, params: Option<Value>) -> Result<Value, Error> {
        let (msg_type, payload) = if method == "initialize" {
            let token = self
                .inner
                .cfg
                .lock()
                .unwrap()
                .token
                .clone()
                .unwrap_or_default();
            (
                nat::AUTH,
                serde_json::to_vec(&serde_json::json!({"token": token})).map_err(Error::Json)?,
            )
        } else if method == "tools/call" {
            let p = params.unwrap_or(Value::Null);
            (
                nat::EXECUTE,
                serde_json::to_vec(&serde_json::json!({
                    "tool": p.get("name").cloned().unwrap_or(Value::Null),
                    "args": p.get("arguments").cloned().unwrap_or(Value::Null),
                }))
                .map_err(Error::Json)?,
            )
        } else {
            (
                nat::EXECUTE,
                serde_json::to_vec(&serde_json::json!({
                    "tool": method,
                    "args": params.unwrap_or(Value::Null),
                }))
                .map_err(Error::Json)?,
            )
        };
        let mut guard = self.inner.io.lock().await;
        if self.inner.closed.load(Ordering::Relaxed) {
            return Err(Error::Mcp(McpError::unavailable()));
        }
        let io = guard
            .as_mut()
            .ok_or_else(|| Error::Mcp(McpError::unavailable()))?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        if let Err(e) = native_send(io, id, msg_type, &payload).await {
            self.inner.closed.store(true, Ordering::Relaxed);
            return Err(e);
        }
        loop {
            let (mt, rid, payload) = match native_recv(io).await {
                Ok(f) => f,
                Err(e) => {
                    // A transport or integrity failure on a live connection
                    // (mid-frame EOF, checksum mismatch, over-cap frame):
                    // the stream cannot be trusted again — latch closed so
                    // subsequent calls fail fast with UNAVAILABLE instead
                    // of touching a poisoned socket (§18 close/truncate/
                    // corrupt/half-close).
                    self.inner.closed.store(true, Ordering::Relaxed);
                    return Err(e);
                }
            };
            if rid < id {
                continue; // a late frame from a dropped stream — never an error
            }
            if rid > id {
                return Err(Error::Mcp(McpError::protocol_error(id, rid)));
            }
            if mt == nat::ERROR {
                return Err(Error::Mcp(native_error(&payload)));
            }
            let env: Value = serde_json::from_slice(&payload).map_err(Error::Json)?;
            if env.get("ok") == Some(&serde_json::json!(false)) {
                if let Some(e) = env.get("error") {
                    let m: McpError = serde_json::from_value(e.clone()).map_err(Error::Json)?;
                    return Err(Error::Mcp(m));
                }
                return Err(Error::Mcp(McpError {
                    code: "INTERNAL".into(),
                    message: format!("tool {method} failed without an error envelope"),
                    retryable: false,
                    suggestion: String::new(),
                }));
            }
            return Ok(env.get("data").cloned().unwrap_or(env));
        }
    }

    /// Performs the MCP handshake (protocol version, client info, token)
    /// and enforces the ND-12 version contract: a server older than
    /// MIN_SERVER_VERSION fails fast with VERSION_MISMATCH.
    pub async fn initialize(&self) -> Result<(), Error> {
        if self.inner.native.load(Ordering::Relaxed) {
            // The version contract was already enforced at connect_native
            // (the HELLO response); initialize is the AUTH frame carrying
            // the token.
            self.request("initialize", None).await?;
            return Ok(());
        }
        let (name, version, token) = {
            let cfg = self.inner.cfg.lock().unwrap();
            (cfg.name.clone(), cfg.version.clone(), cfg.token.clone())
        };
        let mut params = Map::new();
        params.insert("protocolVersion".into(), Value::String("2024-11-05".into()));
        params.insert("capabilities".into(), Value::Object(Map::new()));
        params.insert(
            "clientInfo".into(),
            serde_json::json!({
                "name": name,
                "version": version,
            }),
        );
        if let Some(token) = token {
            params.insert("token".into(), Value::String(token));
        }
        let raw = self
            .request("initialize", Some(Value::Object(params)))
            .await?;
        let res: InitResponse = serde_json::from_value(raw).map_err(Error::Json)?;
        let server = &res.server_info.version;
        if version_less(&parse_version(server), &parse_version(MIN_SERVER_VERSION)) {
            return Err(Error::Mcp(McpError::version_mismatch(server)));
        }
        Ok(())
    }

    /// Establishes session identity (MRFC-0040); subsequent calls inherit
    /// it. On TCP the identity is server-assigned by --tcp-token, so
    /// agent_id must be omitted there — only run_id is per-session.
    pub async fn session_init(&self, p: SessionParams) -> Result<(), Error> {
        self.request(
            "session/init",
            Some(serde_json::to_value(p).map_err(Error::Json)?),
        )
        .await?;
        Ok(())
    }

    /// Calls any registered MCP tool by name and returns its data payload —
    /// the escape hatch for tools without a typed wrapper here.
    pub async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<Value, Error> {
        let mut params = Map::new();
        params.insert("name".into(), Value::String(name.into()));
        if let Some(a) = arguments {
            params.insert("arguments".into(), a);
        }
        let raw = self
            .request("tools/call", Some(Value::Object(params)))
            .await?;
        if self.inner.native.load(Ordering::Relaxed) {
            // request() already unwrapped the {ok:true, data} envelope.
            return Ok(raw);
        }
        let env: ToolEnvelope = serde_json::from_value(raw).map_err(Error::Json)?;
        let text = env
            .content
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_default();
        let payload: ToolPayload = serde_json::from_str(&text).map_err(Error::Json)?;
        // Mirrors the Python SDK: an absent "ok" means success; the "data"
        // field, when present, wraps the payload.
        if payload.ok == Some(false) {
            if let Some(e) = payload.error {
                return Err(Error::Mcp(e));
            }
            return Err(Error::Mcp(McpError {
                code: "INTERNAL".into(),
                message: format!("tool {name} failed without an error envelope"),
                retryable: false,
                suggestion: String::new(),
            }));
        }
        if let Some(data) = payload.data {
            return Ok(data);
        }
        serde_json::from_str(&text).map_err(Error::Json)
    }

    /// Runs a streaming query. Yields the response frame (the first data
    /// chunk — for total_chunks == 1 there is no notify at all), then each
    /// notify chunk until its done flag. Dropping the stream cancels the
    /// read: the connection is released for the next call. The Client must
    /// not be shared while the stream is open (clones serialize on the
    /// same mutex) — the Go SDK's caveat, inherited.
    pub async fn query_stream(
        &self,
        query: &str,
        subject: &str,
    ) -> Result<ReceiverStream<Result<Value, Error>>, Error> {
        if self.inner.native.load(Ordering::Relaxed) {
            return self.native_stream(query, subject).await;
        }
        let mut params = Map::new();
        params.insert("query".into(), Value::String(query.into()));
        if !subject.is_empty() {
            params.insert("subject".into(), Value::String(subject.into()));
        }
        let (tx, rx) = mpsc::channel(16);
        let inner = self.inner.clone();
        tokio::spawn(async move {
            if let Err(e) = stream_call(inner, Value::Object(params), &tx).await {
                let _ = tx.send(Err(e)).await;
            }
        });
        Ok(ReceiverStream::new(rx))
    }

    /// The native streaming query: a QUERY frame with stream:true, then the
    /// head chunk (QUERY) and each QUERY_CHUNK yielded as they arrive,
    /// ending on QUERY_END. Dropping the stream cancels the read — the
    /// connection is released, and the server's pump observes the cancel on
    /// its next send (§6 invariant 10).
    async fn native_stream(
        &self,
        query: &str,
        subject: &str,
    ) -> Result<ReceiverStream<Result<Value, Error>>, Error> {
        let mut params = Map::new();
        params.insert("query".into(), Value::String(query.into()));
        if !subject.is_empty() {
            params.insert("subject".into(), Value::String(subject.into()));
        }
        params.insert("stream".into(), serde_json::json!(true));
        let payload = serde_json::to_vec(&Value::Object(params)).map_err(Error::Json)?;
        // A 2-slot channel bounds the read-ahead: the pump can buffer at most
        // 2 frames ahead of the consumer, so backpressure stalls it while the
        // server is mid-stream — a disconnect or cancel surfaces within a
        // couple of chunks instead of draining the whole stream first
        // (worst-case buffered memory is 2 × MAX_PAYLOAD).
        let (tx, rx) = mpsc::channel(2);
        let inner = self.inner.clone();
        tokio::spawn(async move {
            if let Err(e) = native_stream_pump(inner, payload, &tx).await {
                let _ = tx.send(Err(e)).await;
            }
        });
        Ok(ReceiverStream::new(rx))
    }
}

/// Writes one native request frame — header, payload, checksum (§6).
async fn native_send(
    io: &mut BufReader<TcpStream>,
    request_id: u64,
    msg_type: u16,
    payload: &[u8],
) -> Result<(), Error> {
    if payload.len() > nat::MAX_PAYLOAD {
        return Err(Error::Mcp(McpError {
            code: "FRAME_TOO_LARGE".into(),
            message: format!(
                "payload of {} bytes exceeds the {} byte frame cap — use a stream",
                payload.len(),
                nat::MAX_PAYLOAD
            ),
            retryable: false,
            suggestion: String::new(),
        }));
    }
    let header = nat::header_bytes(0, request_id, msg_type, payload.len() as u32);
    let mut buf = Vec::with_capacity(nat::HEADER_LEN + payload.len() + 4);
    buf.extend_from_slice(&header);
    buf.extend_from_slice(payload);
    let crc = nat::crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    io.write_all(&buf).await.map_err(Error::Io)?;
    io.flush().await.map_err(Error::Io)?;
    Ok(())
}

/// Reads the next native response frame. A mid-frame EOF is
/// UnexpectedEof (the same "connection closed by the server" shape the
/// MCP path reports), a decode failure is InvalidData — §3.3 transport
/// errors. An over-cap frame is rejected from its header, before a byte
/// of payload is read, as the classified FRAME_TOO_LARGE (§19 — a
/// malicious server cannot make the client allocate what it claims).
/// A well-formed frame without the response flag is skipped (the §6 wire
/// has no notification class; MCP parity — CI-16's "call() skips
/// non-response frames").
async fn native_recv(io: &mut BufReader<TcpStream>) -> Result<(u16, u64, Vec<u8>), Error> {
    loop {
        let mut header = [0u8; nat::HEADER_LEN];
        io.read_exact(&mut header).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed by the server",
                ))
            } else {
                Error::Io(e)
            }
        })?;
        let h = nat::parse_header(&header).map_err(|e| match e {
            nat::DecodeError::Oversized { claimed } => Error::Mcp(McpError {
                code: "FRAME_TOO_LARGE".into(),
                message: format!(
                    "server claimed a {claimed} byte payload — exceeds the {} byte frame cap",
                    nat::MAX_PAYLOAD
                ),
                retryable: false,
                suggestion: String::new(),
            }),
            other => Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                other.to_string(),
            )),
        })?;
        if h.flags & nat::FLAG_RESPONSE == 0 {
            // Not a response: read the rest of the frame so the stream
            // stays aligned, then keep looking for the actual response.
            let mut rest = vec![0u8; h.payload_len + 4];
            io.read_exact(&mut rest).await.map_err(Error::Io)?;
            continue;
        }
        let mut payload = vec![0u8; h.payload_len];
        io.read_exact(&mut payload).await.map_err(Error::Io)?;
        let mut crc = [0u8; 4];
        io.read_exact(&mut crc).await.map_err(Error::Io)?;
        if !nat::verify(&header, &payload, u32::from_le_bytes(crc)) {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "native frame checksum mismatch",
            )));
        }
        return Ok((h.msg_type, h.request_id, payload));
    }
}

/// An ERROR frame → McpError. The server sends the native codes directly;
/// the legacy "-32001" spelling (the MCP auth vector's frozen expectation)
/// maps to AUTHENTICATION_FAILED.
fn native_error(payload: &[u8]) -> McpError {
    let env: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
    let code = env
        .get("code")
        .and_then(|c| c.as_str())
        .map(|c| {
            if c == "-32001" {
                "AUTHENTICATION_FAILED"
            } else {
                c
            }
        })
        .unwrap_or("INTERNAL")
        .to_string();
    let message = env
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    McpError {
        code,
        message,
        retryable: false,
        suggestion: String::new(),
    }
}

/// The native stream read loop (the §6 mirror of stream_call): QUERY head →
/// yield, QUERY_CHUNK → yield, QUERY_END → done. Holds the connection
/// mutex for the stream's whole life; dropping the receiver cancels the
/// read and releases the connection.
async fn native_stream_pump(
    inner: Arc<Inner>,
    payload: Vec<u8>,
    tx: &mpsc::Sender<Result<Value, Error>>,
) -> Result<(), Error> {
    let mut guard = inner.io.lock().await;
    if inner.closed.load(Ordering::Relaxed) {
        return Err(Error::Mcp(McpError::unavailable()));
    }
    let io = guard
        .as_mut()
        .ok_or_else(|| Error::Mcp(McpError::unavailable()))?;
    let id = inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    native_send(io, id, nat::QUERY, &payload).await?;
    loop {
        let (mt, rid, payload) = tokio::select! {
            r = native_recv(io) => r?,
            _ = tx.closed() => return Ok(()),
        };
        if rid < id {
            continue; // a late frame from a dropped stream — never an error
        }
        if rid > id {
            return Err(Error::Mcp(McpError::protocol_error(id, rid)));
        }
        match mt {
            nat::QUERY => {
                let head: Value = serde_json::from_slice(&payload).map_err(Error::Json)?;
                let _ = tx.send(Ok(head)).await;
            }
            nat::QUERY_CHUNK => {
                let chunk: Value = serde_json::from_slice(&payload).map_err(Error::Json)?;
                let _ = tx.send(Ok(chunk)).await;
            }
            nat::QUERY_END => return Ok(()),
            nat::ERROR => return Err(Error::Mcp(native_error(&payload))),
            _ => continue, // unrelated frames mid-stream
        }
    }
}

/// The stream read loop, holding the connection mutex for the stream's
/// whole life.
async fn stream_call(
    inner: Arc<Inner>,
    params: Value,
    tx: &mpsc::Sender<Result<Value, Error>>,
) -> Result<(), Error> {
    let mut guard = inner.io.lock().await;
    if inner.closed.load(Ordering::Relaxed) {
        return Err(Error::Mcp(McpError::unavailable()));
    }
    let io = guard
        .as_mut()
        .ok_or_else(|| Error::Mcp(McpError::unavailable()))?;
    let id = inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    let frame = serde_json::to_string(&RpcRequest {
        jsonrpc: "2.0",
        id,
        method: "aikoql/stream".into(),
        params: Some(params),
    })
    .map_err(Error::Json)?;
    io.write_all(frame.as_bytes()).await.map_err(Error::Io)?;
    io.write_all(b"\n").await.map_err(Error::Io)?;
    io.flush().await.map_err(Error::Io)?;
    let mut stream_id: Option<String> = None;
    let (mut total, mut received) = (0usize, 0usize);
    let mut line = String::new();
    loop {
        line.clear();
        // Dropping the receiver closes the channel: the read unblocks and
        // the connection is released (real cancellation, §4.3).
        let n = tokio::select! {
            n = io.read_line(&mut line) => n.map_err(Error::Io)?,
            _ = tx.closed() => return Ok(()),
        };
        if n == 0 {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed by the server",
            )));
        }
        let resp: RpcResponse = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rid = resp.id.unwrap_or(0);
        if rid == id {
            if let Some(e) = resp.error {
                return Err(Error::Mcp(e.mcp_error()));
            }
            let head: StreamHead =
                serde_json::from_value(resp.result.clone().unwrap_or(Value::Null))
                    .map_err(Error::Json)?;
            stream_id = Some(head.stream_id.clone());
            total = head.total_chunks;
            // The response frame IS the first chunk (it carries the data;
            // for total_chunks == 1 there is no notify at all) — the Go
            // and Python SDKs yield it too.
            let _ = tx.send(Ok(resp.result.unwrap_or(Value::Null))).await;
            received += 1;
            if total > 0 && received >= total {
                return Ok(());
            }
            continue;
        }
        let Some(sid) = stream_id.as_ref() else {
            continue; // push before the response frame
        };
        if resp.method.as_deref() != Some("notifications/notify") {
            continue; // an unrelated event while streaming
        }
        let params = resp.params.unwrap_or(Value::Null);
        let p: NotifyChunk = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if p.stream_id != *sid {
            continue;
        }
        let _ = tx.send(Ok(params)).await;
        received += 1;
        if p.done || (total > 0 && received >= total) {
            return Ok(()); // the Go exit condition: done, or received == total_chunks
        }
    }
}

#[derive(Deserialize)]
struct InitResponse {
    #[serde(default, rename = "serverInfo")]
    server_info: ServerInfo,
}

#[derive(Deserialize, Default)]
struct ServerInfo {
    #[serde(default)]
    version: String,
}

#[derive(Deserialize)]
struct ToolEnvelope {
    #[serde(default)]
    content: Vec<ToolContent>,
}

#[derive(Deserialize)]
struct ToolContent {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct ToolPayload {
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    error: Option<McpError>,
}

#[derive(Deserialize)]
struct StreamHead {
    stream_id: String,
    #[serde(default)]
    total_chunks: usize,
}

#[derive(Deserialize)]
struct NotifyChunk {
    #[serde(default)]
    stream_id: String,
    #[serde(default)]
    done: bool,
}

/// SessionParams establishes session identity (MRFC-0040).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
}

/// Mirrors the Go SDK's dotted-int tuple: non-numeric segments become -1
/// (never >=).
fn parse_version(v: &str) -> Vec<i64> {
    v.split('.')
        .map(|seg| seg.parse::<i64>().unwrap_or(-1))
        .collect()
}

/// Compares two dotted version tuples segment by segment.
fn version_less(a: &[i64], b: &[i64]) -> bool {
    for i in 0..a.len().min(b.len()) {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
    }
    a.len() < b.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_stream::StreamExt;

    type Frame = (String, Duration);

    /// Accepts one client connection and drives `script`: for each
    /// incoming line, hands the parsed request to it and writes each
    /// returned frame back (after its delay). An empty frame list loops
    /// for the next request (a hung server).
    async fn scripted<F>(script: F) -> String
    where
        F: FnMut(Value) -> Vec<Frame> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let mut script = script;
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = tokio::io::split(stream);
            let mut lines = BufReader::new(r).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let req: Value = serde_json::from_str(&line).unwrap();
                for (frame, delay) in script(req) {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    let _ = w.write_all(format!("{frame}\n").as_bytes()).await;
                }
            }
        });
        addr
    }

    fn respond(id: &Value, version: &str) -> String {
        serde_json::json!({"id": id, "result": {"serverInfo": {"version": version}}}).to_string()
    }

    fn tool_result(id: &Value, data: Value) -> String {
        let text = serde_json::to_string(&serde_json::json!({"ok": true, "data": data})).unwrap();
        serde_json::json!({"id": id, "result": {"content": [{"text": text}], "isError": false}})
            .to_string()
    }

    async fn dial(addr: &str) -> Client {
        Client::dial(addr).await.unwrap()
    }

    #[tokio::test]
    async fn initialize_ok() {
        let reqs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = reqs.clone();
        let addr = scripted(move |req| {
            seen.lock().unwrap().push(req.clone());
            vec![(respond(&req["id"], "0.2.0"), Duration::ZERO)]
        })
        .await;
        let c = dial(&addr).await;
        c.initialize().await.unwrap();
        let reqs = reqs.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0]["method"], "initialize");
    }

    #[tokio::test]
    async fn skips_stale_and_idless_frames() {
        let addr = scripted(|req| {
            vec![
                (serde_json::json!({}).to_string(), Duration::ZERO), // id-less push
                (
                    serde_json::json!({"id": 0, "result": {}}).to_string(),
                    Duration::ZERO,
                ), // stale
                (respond(&req["id"], "0.2.0"), Duration::ZERO),
            ]
        })
        .await;
        let c = dial(&addr).await;
        c.initialize().await.unwrap();
    }

    #[tokio::test]
    async fn protocol_error_on_foreign_id() {
        let addr = scripted(|_req| {
            vec![(
                serde_json::json!({"id": 99, "result": {}}).to_string(),
                Duration::ZERO,
            )]
        })
        .await;
        let c = dial(&addr).await;
        let err = c.initialize().await.unwrap_err();
        match err {
            Error::Mcp(m) => assert_eq!(m.code, "PROTOCOL_ERROR"),
            other => panic!("expected Mcp(PROTOCOL_ERROR), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rpc_error_envelope() {
        let addr = scripted(|req| {
            vec![(
                serde_json::json!({"id": req["id"], "error": {"code": "NOT_FOUND", "message": "nope"}})
                    .to_string(),
                Duration::ZERO,
            )]
        })
        .await;
        let c = dial(&addr).await;
        let err = c.initialize().await.unwrap_err();
        match err {
            Error::Mcp(m) => assert_eq!(m.code, "NOT_FOUND"),
            other => panic!("expected Mcp(NOT_FOUND), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn noise_frames_skipped() {
        let addr = scripted(|req| {
            vec![
                ("not json".to_string(), Duration::ZERO),
                (respond(&req["id"], "0.2.0"), Duration::ZERO),
            ]
        })
        .await;
        let c = dial(&addr).await;
        c.initialize().await.unwrap();
    }

    #[tokio::test]
    async fn deadline_maps_to_retryable_timeout() {
        let addr = scripted(|_req| vec![]).await; // accepts, never answers
        let c = dial(&addr).await;
        let err = crate::with_deadline(Duration::from_millis(200), c.initialize())
            .await
            .unwrap_err();
        match err {
            Error::Mcp(m) => {
                assert_eq!(m.code, "TIMEOUT");
                assert!(m.retryable);
            }
            other => panic!("expected Mcp(TIMEOUT), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn closed_client_is_unavailable() {
        let addr = scripted(|_req| vec![]).await;
        let c = dial(&addr).await;
        c.close().await.unwrap();
        let err = c.call_tool("health", None).await.unwrap_err();
        match err {
            Error::Mcp(m) => assert_eq!(m.code, "UNAVAILABLE"),
            other => panic!("expected Mcp(UNAVAILABLE), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn version_mismatch_fails_fast() {
        let addr = scripted(|req| vec![(respond(&req["id"], "0.0.1"), Duration::ZERO)]).await;
        let c = dial(&addr).await;
        let err = c.initialize().await.unwrap_err();
        match err {
            Error::Mcp(m) => assert_eq!(m.code, "VERSION_MISMATCH"),
            other => panic!("expected Mcp(VERSION_MISMATCH), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_single_chunk_total_1() {
        let addr = scripted(|req| {
            vec![(
                serde_json::json!({
                    "id": req["id"],
                    "result": {"stream_id": "s1", "total_chunks": 1,
                                "results": [{"koid": "k1"}]},
                })
                .to_string(),
                Duration::ZERO,
            )]
        })
        .await;
        let c = dial(&addr).await;
        let stream = c.query_stream("MATCH p RETURN *", "").await.unwrap();
        let chunks: Vec<_> = stream.map(|r| r.unwrap()).collect::<Vec<_>>().await;
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0]["results"][0]["koid"], "k1");
    }

    #[tokio::test]
    async fn stream_multi_chunk_ends_on_done() {
        let addr = scripted(|req| {
            vec![
                (
                    serde_json::json!({
                        "id": req["id"],
                        "result": {"stream_id": "s1", "total_chunks": 2,
                                    "results": [{"koid": "k1"}]},
                    })
                    .to_string(),
                    Duration::ZERO,
                ),
                (
                    serde_json::json!({
                        "method": "notifications/notify",
                        "params": {"stream_id": "s1", "chunk": 2, "done": true,
                                    "results": [{"koid": "k2"}]},
                    })
                    .to_string(),
                    Duration::ZERO,
                ),
            ]
        })
        .await;
        let c = dial(&addr).await;
        let stream = c.query_stream("MATCH p RETURN *", "").await.unwrap();
        let chunks: Vec<_> = stream.map(|r| r.unwrap()).collect::<Vec<_>>().await;
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1]["results"][0]["koid"], "k2");
    }

    #[tokio::test]
    async fn dropped_stream_releases_the_connection() {
        let addr = scripted(|req| {
            if req["method"] == "aikoql/stream" {
                vec![(
                    serde_json::json!({
                        "id": req["id"],
                        "result": {"stream_id": "s1", "total_chunks": 2, "results": []},
                    })
                    .to_string(),
                    Duration::ZERO,
                )]
            } else {
                vec![(
                    tool_result(&req["id"], serde_json::json!({"status": "healthy"})),
                    Duration::ZERO,
                )]
            }
        })
        .await;
        let c = dial(&addr).await;
        let stream = c.query_stream("MATCH p RETURN *", "").await.unwrap();
        let mut stream = stream;
        let _first = stream.next().await.unwrap().unwrap();
        drop(stream); // cancel — the read unblocks, the mutex is released
        let health = c.call_tool("health", None).await.unwrap();
        assert_eq!(health["status"], "healthy");
    }

    #[tokio::test]
    async fn txn_done_guard_refuses_closed_handle() {
        let addr = scripted(|req| match req["params"]["name"].as_str() {
            Some("txn_begin") => {
                vec![(
                    tool_result(&req["id"], serde_json::json!({})),
                    Duration::ZERO,
                )]
            }
            Some("txn_commit") => vec![(
                tool_result(
                    &req["id"],
                    serde_json::json!({"results": [], "deduped": false}),
                ),
                Duration::ZERO,
            )],
            other => panic!("unexpected tool call {other:?}"),
        })
        .await;
        let c = dial(&addr).await;
        let mut tx = c.begin(None).await.unwrap();
        let res = tx.commit().await.unwrap();
        assert!(!res.deduped);
        let err = tx
            .execute(crate::tx::StagedOp {
                action: "create".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        match err {
            Error::Mcp(m) => assert_eq!(m.code, "INVALID_ARGUMENT"),
            other => panic!("expected Mcp(INVALID_ARGUMENT), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn late_response_after_timeout_is_skipped() {
        // The deadline kills call 1; its late frame arrives while call 2
        // waits and is skipped by id correlation (self-healing).
        let mut calls = 0;
        let addr = scripted(move |req| {
            calls += 1;
            let delay = if calls == 1 {
                Duration::from_millis(300)
            } else {
                Duration::ZERO
            };
            vec![(respond(&req["id"], "0.2.0"), delay)]
        })
        .await;
        let c = dial(&addr).await;
        let err = crate::with_deadline(Duration::from_millis(50), c.initialize())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Mcp(m) if m.code == "TIMEOUT"));
        // The late frame for request 1 arrives during this call and is
        // skipped (id 1 < id 2); the real answer for id 2 lands.
        c.initialize().await.unwrap();
    }
}
