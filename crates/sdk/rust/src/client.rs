//! The remote client: MCP JSON-RPC over TCP. Mirrors the Go SDK's wire
//! layer (crates/sdk/go/aikoql.go) — newline frames, id correlation, the
//! tools/call envelope — and the frozen §3.3 semantics: a response with a
//! smaller id is skipped, a larger id is PROTOCOL_ERROR, deadline reads
//! map to the retryable TIMEOUT, and a call on a closed client is
//! UNAVAILABLE before the transport is touched. The transport is private
//! to the Client: the D-15 native protocol replacement touches only this
//! file (the canonical API does not change when the transport changes).

use crate::error::{Error, McpError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
                cfg: std::sync::Mutex::new(ClientConfig {
                    token: None,
                    name: "aikoql-rust-sdk".into(),
                    version: "0.2.0".into(),
                }),
            }),
        })
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
        self.inner.closed.store(true, Ordering::Relaxed);
        // Taking the reader drops the TcpStream, closing the socket.
        let mut guard = self.inner.io.lock().await;
        *guard = None;
        Ok(())
    }

    /// Sends one JSON-RPC request and returns its result frame, skipping
    /// pushed notifications by id correlation (§3.3).
    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, Error> {
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

    /// Performs the MCP handshake (protocol version, client info, token)
    /// and enforces the ND-12 version contract: a server older than
    /// MIN_SERVER_VERSION fails fast with VERSION_MISMATCH.
    pub async fn initialize(&self) -> Result<(), Error> {
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
