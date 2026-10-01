//! D-15 native protocol RED (§5/§6, ECO-2): the wire oracle. Every leg
//! here speaks the frozen framed binary protocol by hand — magic, version,
//! flags, request id, message type, length, payload, checksum — so the
//! server and the SDK's native transport are pinned by an independent
//! implementation, not by each other. The SDK legs call
//! `Client::connect_native`, which does not exist yet: this file is RED
//! by construction until the transport lands.
//!
//! The frozen format (docs/NATIVE-PROTOCOL.md, written at GREEN):
//!   magic "AKQL" | version u16 BE | flags u16 BE (bit0 = response) |
//!   request_id u64 BE | msg_type u16 BE | payload_length u32 BE |
//!   payload (JSON UTF-8, <= 1 MiB) | crc32 (IEEE, LE, over everything
//!   before it). Header = 22 bytes.
//!
//! Message types: HELLO=1 AUTH=2 PING=3 BEGIN=4 COMMIT=5 ROLLBACK=6
//! PREPARE=7 EXECUTE=8 QUERY=9 QUERY_CHUNK=10 QUERY_END=11 CANCEL=12
//! CLOSE=13 ERROR=14.

use aikoql_sdk::tools::RememberParams;
use aikoql_sdk::{Client, Error, StagedOp};
use serde_json::{json, Map, Value};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_stream::StreamExt;

// ---- the frozen wire format (§6) -------------------------------------------

const MAGIC: &[u8; 4] = b"AKQL";
const PROTOCOL_VERSION: u16 = 1;
const FLAG_RESPONSE: u16 = 1;
const MAX_PAYLOAD: usize = 1024 * 1024; // mirrors MCP's MAX_FRAME_BYTES
const HEADER_LEN: usize = 22;

const HELLO: u16 = 1;
const AUTH: u16 = 2;
const PING: u16 = 3;
const BEGIN: u16 = 4;
const COMMIT: u16 = 5;
const ROLLBACK: u16 = 6;
const PREPARE: u16 = 7;
const EXECUTE: u16 = 8;
const QUERY: u16 = 9;
const QUERY_CHUNK: u16 = 10;
const QUERY_END: u16 = 11;
const CANCEL: u16 = 12;
const CLOSE: u16 = 13;
const ERROR: u16 = 14;

/// CRC-32 (IEEE 802.3, poly 0xEDB88320), table built at const-eval —
/// the wire checksum is an integrity fingerprint (2^-32), not authenticity
/// (that lives in the encrypted envelope, MRFC-0020).
const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in bytes {
        c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

fn frame_bytes(request_id: u64, msg_type: u16, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= MAX_PAYLOAD, "oracle payload too big");
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + 4);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // flags: request
    out.extend_from_slice(&request_id.to_be_bytes());
    out.extend_from_slice(&msg_type.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(&crc32(&out).to_le_bytes());
    out
}

struct Frame {
    request_id: u64,
    msg_type: u16,
    payload: Vec<u8>,
}

impl Frame {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.payload).expect("frame payload must be JSON")
    }
}

/// A raw framed connection — the oracle speaks the protocol itself, so the
/// legs never go through the SDK's (absent) native codec.
struct RawConn {
    s: TcpStream,
}

impl RawConn {
    fn connect(addr: &str) -> Self {
        let s = TcpStream::connect(addr).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        s.set_write_timeout(Some(Duration::from_secs(10)))
            .expect("write timeout");
        RawConn { s }
    }

    fn send(&mut self, request_id: u64, msg_type: u16, payload: Value) {
        let bytes = frame_bytes(
            request_id,
            msg_type,
            &serde_json::to_vec(&payload).expect("payload serializes"),
        );
        self.s.write_all(&bytes).expect("write frame");
    }

    fn send_bytes(&mut self, bytes: &[u8]) {
        self.s.write_all(bytes).expect("write bytes");
    }

    /// The next response frame, or None on a clean close. A closed
    /// connection mid-frame (truncated, timeout) also reads as None — the
    /// close-fast assertions below tell rejection apart from a hang.
    fn recv(&mut self) -> Option<Frame> {
        let mut hdr = [0u8; HEADER_LEN];
        let mut n = 0;
        while n < HEADER_LEN {
            match self.s.read(&mut hdr[n..]) {
                Ok(0) => return None,
                Ok(k) => n += k,
                Err(_) => return None, // timeout/reset — treated as close
            }
        }
        assert_eq!(&hdr[0..4], MAGIC, "response must carry the magic");
        let flags = u16::from_be_bytes([hdr[4], hdr[5]]);
        assert_ne!(flags & FLAG_RESPONSE, 0, "expected a response frame");
        let request_id = u64::from_be_bytes(hdr[6..14].try_into().unwrap());
        let msg_type = u16::from_be_bytes([hdr[14], hdr[15]]);
        let len = u32::from_be_bytes([hdr[16], hdr[17], hdr[18], hdr[19]]) as usize;
        assert!(len <= MAX_PAYLOAD, "server sent an oversized frame");
        let mut payload = vec![0u8; len];
        let mut n = 0;
        while n < len {
            match self.s.read(&mut payload[n..]) {
                Ok(0) => panic!("truncated response frame"),
                Ok(k) => n += k,
                Err(_) => panic!("read error in response frame"),
            }
        }
        let mut crc_buf = [0u8; 4];
        let mut n = 0;
        while n < 4 {
            match self.s.read(&mut crc_buf[n..]) {
                Ok(0) => panic!("truncated response checksum"),
                Ok(k) => n += k,
                Err(_) => panic!("read error in response checksum"),
            }
        }
        let mut whole = Vec::with_capacity(HEADER_LEN + len);
        whole.extend_from_slice(&hdr);
        whole.extend_from_slice(&payload);
        assert_eq!(
            crc32(&whole),
            u32::from_le_bytes(crc_buf),
            "response checksum mismatch"
        );
        Some(Frame {
            request_id,
            msg_type,
            payload,
        })
    }

    fn recv_expect(&mut self, msg_type: u16, request_id: u64) -> Frame {
        let f = self.recv().expect("expected a frame, got EOF");
        assert_eq!(f.msg_type, msg_type, "unexpected message type");
        assert_eq!(
            f.request_id, request_id,
            "response id must echo the request id"
        );
        f
    }

    /// HELLO (request 1) + AUTH (request 2) — the common prologue.
    fn hello_auth(&mut self) {
        self.send(
            1,
            HELLO,
            json!({"protocol_version": 1, "capabilities": [], "client": {"name": "oracle", "version": "0.1.0"}}),
        );
        self.recv_expect(HELLO, 1);
        self.send(2, AUTH, json!({"token": "conformance"}));
        let a = self.recv_expect(AUTH, 2).json();
        assert_eq!(a["ok"], true, "auth must succeed for the good token");
    }
}

// ---- the real server --------------------------------------------------------

/// The --native-port listener is the D-15 server; the process and its temp
/// dir are swept on drop, exactly like the conformance adapter's guard.
struct NativeServer {
    child: Option<Child>,
    addr: String,
    dir: PathBuf,
}

impl NativeServer {
    fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for NativeServer {
    fn drop(&mut self) {
        self.kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn spawn_native(bin: &str, token: &str) -> NativeServer {
    let probe = TcpListener::bind("127.0.0.1:0").expect("probe port");
    let port = probe.local_addr().expect("probe addr").port();
    drop(probe);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("native-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let db = dir.join("db.aikoql"); // does not exist → auto-create (aikoql-v2)
    let addr = format!("127.0.0.1:{port}");
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
            return NativeServer {
                child: Some(child),
                addr,
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

fn mcp_bin() -> Option<String> {
    match std::env::var_os("AIKOQL_MCP_BIN") {
        Some(b) => Some(b.to_string_lossy().into_owned()),
        None => {
            eprintln!("AIKOQL_MCP_BIN not set — native-protocol real-server legs skipped");
            None
        }
    }
}

// ---- the invariant legs -----------------------------------------------------

#[test]
fn hello_negotiates_version_and_capabilities() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let mut c = RawConn::connect(&srv.addr);
    c.send(
        1,
        HELLO,
        json!({"protocol_version": 1, "capabilities": [], "client": {"name": "oracle", "version": "0.1.0"}}),
    );
    let f = c.recv_expect(HELLO, 1).json();
    assert_eq!(f["protocol_version"], 1);
    assert_eq!(f["server_version"], "0.2.0");
    assert!(
        f["capabilities"].is_array(),
        "the server must list its capabilities explicitly"
    );
    assert!(
        f["session_id"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
        "the server must hand out a session id"
    );
}

#[test]
fn version_mismatch_is_explicit() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let mut c = RawConn::connect(&srv.addr);
    c.send(
        1,
        HELLO,
        json!({"protocol_version": 99, "capabilities": [], "client": {"name": "oracle", "version": "0.1.0"}}),
    );
    let e = c.recv_expect(ERROR, 1).json();
    assert_eq!(e["code"], "VERSION_MISMATCH");
    assert!(c.recv().is_none(), "the server must close after a mismatch");
}

#[test]
fn auth_gate_ping_refused_until_valid_token() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let mut c = RawConn::connect(&srv.addr);
    c.send(
        1,
        HELLO,
        json!({"protocol_version": 1, "capabilities": [], "client": {"name": "oracle", "version": "0.1.0"}}),
    );
    c.recv_expect(HELLO, 1);
    // Ops before AUTH are refused — auth is a gate, not a suggestion.
    c.send(2, PING, json!({}));
    let e = c.recv_expect(ERROR, 2).json();
    assert_eq!(e["code"], "NOT_AUTHENTICATED");
    // A wrong token is refused too.
    c.send(3, AUTH, json!({"token": "nope"}));
    let e2 = c.recv_expect(ERROR, 3).json();
    assert_eq!(e2["code"], "AUTHENTICATION_FAILED");
    // The good token opens the gate; the connection was not consumed by
    // either refusal.
    c.send(4, AUTH, json!({"token": "conformance"}));
    let a = c.recv_expect(AUTH, 4).json();
    assert_eq!(a["ok"], true);
    c.send(5, PING, json!({}));
    let p = c.recv_expect(PING, 5).json();
    assert_eq!(p["pong"], true);
}

#[test]
fn unknown_message_type_fails_safely() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let mut c = RawConn::connect(&srv.addr);
    c.hello_auth();
    c.send(1, 0xFFFF, json!({}));
    let e = c.recv_expect(ERROR, 1).json();
    assert_eq!(e["code"], "UNKNOWN_MESSAGE");
    // The session survives an unknown message type.
    c.send(2, PING, json!({}));
    assert_eq!(c.recv_expect(PING, 2).json()["pong"], true);
}

#[test]
fn oversized_frame_is_rejected_and_server_survives() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let mut c = RawConn::connect(&srv.addr);
    c.hello_auth();
    // A header claiming > 1 MiB, with no payload behind it — the length
    // check must fire before any payload read.
    let mut hdr = Vec::new();
    hdr.extend_from_slice(MAGIC);
    hdr.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    hdr.extend_from_slice(&0u16.to_be_bytes());
    hdr.extend_from_slice(&42u64.to_be_bytes());
    hdr.extend_from_slice(&PING.to_be_bytes());
    hdr.extend_from_slice(&(MAX_PAYLOAD as u32 + 1).to_be_bytes());
    c.send_bytes(&hdr);
    let t = Instant::now();
    assert!(c.recv().is_none(), "oversized frame must be rejected");
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "rejection must close fast, not time out"
    );
    // The SERVER survives: a fresh connection negotiates fine.
    let mut c2 = RawConn::connect(&srv.addr);
    c2.hello_auth();
}

#[test]
fn truncated_frame_is_rejected() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let mut c = RawConn::connect(&srv.addr);
    c.hello_auth();
    // A header promising 10 payload bytes, then only 4 before half-close.
    let mut hdr = Vec::new();
    hdr.extend_from_slice(MAGIC);
    hdr.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    hdr.extend_from_slice(&0u16.to_be_bytes());
    hdr.extend_from_slice(&42u64.to_be_bytes());
    hdr.extend_from_slice(&PING.to_be_bytes());
    hdr.extend_from_slice(&10u32.to_be_bytes());
    c.send_bytes(&hdr);
    c.send_bytes(b"abcd");
    c.s.shutdown(Shutdown::Write).expect("half-close");
    let t = Instant::now();
    assert!(c.recv().is_none(), "truncated frame must be rejected");
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "rejection must close fast, not time out"
    );
}

#[test]
fn bad_checksum_frame_is_rejected() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let mut c = RawConn::connect(&srv.addr);
    c.hello_auth();
    let mut bytes = frame_bytes(1, PING, b"{}");
    *bytes.last_mut().expect("checksum byte") ^= 0xFF; // corrupt the crc
    c.send_bytes(&bytes);
    let t = Instant::now();
    assert!(c.recv().is_none(), "a corrupt frame must be rejected");
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "rejection must close fast, not time out"
    );
}

#[test]
fn prepare_execute_and_query_surface() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let mut c = RawConn::connect(&srv.addr);
    c.hello_auth();
    // PREPARE is validation-only: a compiled query says prepared:true,
    // garbage says prepared:false with the compiler's reason.
    c.send(1, PREPARE, json!({"aikoql": "MATCH st_row RETURN *"}));
    let p = c.recv_expect(PREPARE, 1).json();
    assert_eq!(p["prepared"], true);
    c.send(2, PREPARE, json!({"aikoql": "definitely not aikoql"}));
    let p2 = c.recv_expect(PREPARE, 2).json();
    assert_eq!(p2["prepared"], false);
    // EXECUTE carries the whole tool surface in one message class; the
    // response is the {ok, data, error} envelope.
    c.send(
        3,
        EXECUTE,
        json!({"tool": "remember", "args": {"type": "st_row", "properties": {"name": "n1"}}}),
    );
    let r = c.recv_expect(EXECUTE, 3).json();
    assert_eq!(r["ok"], true);
    let koid = r["data"]["koid"].as_str().expect("remember returns a koid").to_string();
    c.send(4, EXECUTE, json!({"tool": "get", "args": {"koid": koid}}));
    let g = c.recv_expect(EXECUTE, 4).json();
    assert_eq!(g["ok"], true);
    assert_eq!(g["data"]["koid"], koid.as_str());
    // A non-streaming QUERY answers in one frame.
    c.send(5, QUERY, json!({"query": "MATCH st_row RETURN *", "stream": false}));
    let q = c.recv_expect(QUERY, 5).json();
    assert!(!q["results"].as_array().expect("results list").is_empty());
    // CLOSE is acknowledged, then the server drops the connection.
    c.send(6, CLOSE, json!({}));
    let cl = c.recv_expect(CLOSE, 6).json();
    assert_eq!(cl["ok"], true);
    assert!(c.recv().is_none(), "the server must close after CLOSE");
}

#[test]
fn raw_transaction_lifecycle() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let mut c = RawConn::connect(&srv.addr);
    c.hello_auth();
    // BEGIN → stage through the EXECUTE class → COMMIT (ECO-2's
    // begin/commit surface, driven by raw frames).
    c.send(1, BEGIN, json!({"txn_id": "txn9"}));
    let b = c.recv_expect(BEGIN, 1).json();
    assert_eq!(b["ok"], true);
    assert_eq!(b["txn_id"], "txn9");
    c.send(
        2,
        EXECUTE,
        json!({"tool": "txn_stage", "args": {
            "txn_id": "txn9",
            "op": {"action": "create", "type_name": "st_row", "properties": {"name": "raw-txn"}},
        }}),
    );
    assert_eq!(c.recv_expect(EXECUTE, 2).json()["ok"], true);
    c.send(3, COMMIT, json!({"txn_id": "txn9"}));
    let cm = c.recv_expect(COMMIT, 3).json();
    assert_eq!(cm["ok"], true);
    assert_eq!(cm["results"].as_array().expect("results list").len(), 1);
    // Committing a txn this connection never opened is an ERROR frame.
    c.send(4, COMMIT, json!({"txn_id": "ghost"}));
    let e = c.recv_expect(ERROR, 4).json();
    assert_eq!(e["code"], "INTERNAL");
    // ROLLBACK closes a handle without applying it.
    c.send(5, BEGIN, json!({"txn_id": "txn10"}));
    assert_eq!(c.recv_expect(BEGIN, 5).json()["ok"], true);
    c.send(6, ROLLBACK, json!({"txn_id": "txn10"}));
    let rb = c.recv_expect(ROLLBACK, 6).json();
    assert_eq!(rb["ok"], true);
    assert_eq!(rb["rolled_back"], true);
}

#[test]
fn cancellation_is_observable() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let mut c = RawConn::connect(&srv.addr);
    c.hello_auth();
    // Seed 1600 rows (4 batches of 400, 2 KiB each) so the stream is 16
    // chunks / ~3.2 MiB — far past any socket buffer, so the pump must
    // block while we CANCEL and drain.
    for batch in 0..4u64 {
        let ops: Vec<Value> = (0..400)
            .map(|i| {
                json!({"remember": {"type": "st_row", "properties": {
                    "name": format!("seed{}", batch * 400 + i),
                    "pad": "x".repeat(2000),
                }}})
            })
            .collect();
        c.send(10 + batch, EXECUTE, json!({"tool": "batch", "args": {"operations": ops}}));
        assert_eq!(c.recv_expect(EXECUTE, 10 + batch).json()["ok"], true);
    }
    c.send(100, QUERY, json!({"query": "MATCH st_row RETURN *", "stream": true}));
    let head = c.recv_expect(QUERY, 100).json();
    assert_eq!(head["total_chunks"], 16);
    assert_eq!(head["results"].as_array().expect("first chunk").len(), 100);
    let sid = head["stream_id"].as_str().expect("stream id").to_string();
    // CANCEL refers to the stream's request; the effect is observable on
    // the stream itself: chunks stop early and QUERY_END says cancelled.
    c.send(101, CANCEL, json!({"request_id": 100}));
    let mut chunks = 0usize;
    let end = loop {
        let f = c.recv().expect("stream ended without QUERY_END");
        match f.msg_type {
            QUERY_CHUNK => {
                chunks += 1;
                assert_eq!(f.json()["stream_id"], sid.as_str());
                if chunks > 16 {
                    panic!("too many chunks — the cancel was not applied");
                }
            }
            QUERY_END => break f.json(),
            other => panic!("unexpected frame type {other} mid-stream"),
        }
    };
    assert_eq!(end["cancelled"], true, "QUERY_END must record the cancel");
    assert!(
        chunks < end["total_chunks"].as_u64().expect("total") as usize,
        "the cancel must stop the stream before its natural end"
    );
}

// ---- the SDK legs (Client::connect_native — the RED anchor) -----------------

async fn native_client(addr: &str) -> Client {
    let c = Client::connect_native(addr)
        .await
        .expect("connect_native must dial + HELLO");
    let c = c.with_token("conformance".to_string());
    c.initialize().await.expect("initialize must AUTH");
    c
}

#[tokio::test]
async fn sdk_native_minimal_workflow() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let c = native_client(&srv.addr).await;
    let h = c.health().await.expect("health over native");
    assert_eq!(h["status"], "healthy");
    let mut props = Map::new();
    props.insert("name".into(), json!("eco2"));
    let rem = c
        .remember(RememberParams {
            type_name: "st_row".into(),
            properties: Some(props),
            ..Default::default()
        })
        .await
        .expect("remember over native");
    let got = c.get(&rem.koid, "").await.expect("get over native");
    assert_eq!(got.properties["name"], "eco2");
    let mut tx = c.begin(None).await.expect("begin over native");
    tx.execute(StagedOp {
        action: "create".into(),
        type_name: "st_row".into(),
        properties: {
            let mut p = Map::new();
            p.insert("name".into(), json!("txn-row"));
            Some(p)
        },
        ..Default::default()
    })
    .await
    .expect("stage over native");
    let res = tx.commit().await.expect("commit over native");
    assert_eq!(res.results.len(), 1);
    let mut tx2 = c.begin(None).await.expect("begin over native");
    tx2.rollback().await.expect("rollback over native");
    let stream = c
        .query_stream("MATCH st_row RETURN *", "")
        .await
        .expect("stream over native");
    let chunks: Vec<Value> = stream.map(|r| r.expect("chunk")).collect().await;
    assert!(!chunks.is_empty(), "the stream must yield the head chunk");
    assert!(chunks[0]["results"].is_array());
    c.close().await.expect("close over native");
}

#[tokio::test]
async fn transaction_state_is_per_connection() {
    let Some(bin) = mcp_bin() else { return };
    let srv = spawn_native(&bin, "conformance");
    let a = native_client(&srv.addr).await;
    let b = native_client(&srv.addr).await;
    let mut tx = a.begin(None).await.expect("begin on A");
    let tid = tx.id().to_string();
    // B's connection has no such open txn — the handle is connection-scoped.
    let err = b
        .call_tool(
            "txn_stage",
            Some(json!({"txn_id": tid, "op": {"action": "create", "type_name": "st_row", "properties": {"name": "x"}}})),
        )
        .await
        .expect_err("cross-connection stage must fail");
    assert!(matches!(err, Error::Mcp(_)), "expected a protocol error, got {err:?}");
    let res = tx.commit().await.expect("A's commit still works");
    assert_eq!(res.results.len(), 1);
    let mut t2 = b.begin(None).await.expect("begin on B");
    t2.rollback().await.expect("rollback on B");
}

#[tokio::test]
async fn server_disconnect_never_deadlocks_client() {
    let Some(bin) = mcp_bin() else { return };
    let mut srv = spawn_native(&bin, "conformance");
    let c = native_client(&srv.addr).await;
    for b in 0..2 {
        let ops: Vec<Value> = (0..400)
            .map(|i| {
                json!({"remember": {"type": "st_row", "properties": {
                    "name": format!("seed{}", b * 400 + i),
                    "pad": "x".repeat(2000),
                }}})
            })
            .collect();
        c.call_tool("batch", Some(json!({"operations": ops})))
            .await
            .expect("seed batch");
    }
    let stream = c
        .query_stream("MATCH st_row RETURN *", "")
        .await
        .expect("stream");
    let mut stream = stream;
    let _head = stream.next().await.expect("head").expect("head ok");
    srv.kill(); // the server dies mid-stream — the client must not hang
    let errs = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut errs = 0;
        while let Some(r) = stream.next().await {
            if r.is_err() {
                errs += 1;
            }
        }
        errs
    })
    .await
    .expect("the stream must not deadlock after the server dies");
    assert!(errs >= 1, "a dead server must surface as a stream error");
}

// ---- the conformance pin ----------------------------------------------------

/// The interpreter for the script. On Windows, plain "bash" resolves to
/// the WSL shim (System32) under CreateProcess even when a PATH walk
/// finds Git's bash first — and WSL bash has no cargo. where.exe does the
/// plain PATH walk, so prefer its Git hit.
#[cfg(windows)]
fn bash_exe() -> std::ffi::OsString {
    if let Ok(out) = Command::new("where.exe").arg("bash").output() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let hits: Vec<&str> = stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if let Some(git) = hits.iter().find(|h| h.to_lowercase().contains("git")) {
            return (*git).into();
        }
        if let Some(first) = hits.first() {
            return (*first).into();
        }
    }
    "bash".into()
}

#[cfg(not(windows))]
fn bash_exe() -> std::ffi::OsString {
    "bash".into()
}

#[test]
fn native_conformance_runner_pin() {
    let Some(_bin) = mcp_bin() else { return };
    // crates/sdk/rust (the test cwd) -> repo root is three levels up.
    let script = PathBuf::from("..")
        .join("..")
        .join("..")
        .join("scripts")
        .join("sdk-conformance.sh");
    if !script.exists() {
        panic!(
            "sdk-conformance runner missing at {} — D-15 RED",
            script.display()
        );
    }
    let script = script.to_string_lossy().replace('\\', "/");
    let out = Command::new(bash_exe())
        .arg(&script)
        .arg("--language")
        .arg("rust")
        .arg("--transport")
        .arg("native")
        .output()
        .expect("bash must be on PATH (the Go and Python pins use it too)");
    assert!(
        out.status.success(),
        "sdk-conformance --language rust --transport native failed: {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
