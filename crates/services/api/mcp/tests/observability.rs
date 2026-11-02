//! AG-50 e2e: every tool call leaves an OTLP trace on the collector, the
//! Prometheus scrape carries per-tool counters, and recall quality outcomes
//! are counted.
//!
//! The server runs in metrics-only mode (S-12: no stdin reads) with
//! `--metrics-addr` + `--tcp-token`, so the MCP session is Streamable HTTP
//! on the metrics surface (like tests/mcp_http.rs); `AIKOQL_OTEL_ENDPOINT`
//! points at an in-test OTLP/HTTP collector. The exported protobuf is
//! decoded with opentelemetry-proto, so the gate — a trace for every tool
//! call — is asserted on the wire, not on logs.

use prost::Message;
use serde_json::{json, Value as J};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("aikoql-ag50-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn free_port() -> u16 {
    static GIVEN: Mutex<Vec<u16>> = Mutex::new(Vec::new());
    loop {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
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

/// Minimal OTLP/HTTP collector: keeps every POST body for the test.
struct Collector {
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    port: u16,
}

impl Collector {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let bodies: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let b = bodies.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let b = b.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(s.try_clone().unwrap());
                    let mut line = String::new();
                    let mut len = 0usize;
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).unwrap() == 0 {
                            return;
                        }
                        if line.trim().is_empty() {
                            break;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0u8; len];
                    reader.read_exact(&mut body).unwrap();
                    b.lock().unwrap().push(body);
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
                });
            }
        });
        Collector { bodies, port }
    }

    /// All `tool_call` spans exported so far, keyed tool → count.
    fn tool_calls(&self) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for body in self.bodies.lock().unwrap().iter() {
            let Ok(req) =
                opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest::decode(
                    body.as_slice(),
                )
            else {
                continue;
            };
            for rs in &req.resource_spans {
                for ss in &rs.scope_spans {
                    for span in &ss.spans {
                        if span.name != "tool_call" {
                            continue;
                        }
                        let tool = span
                            .attributes
                            .iter()
                            .find(|kv| kv.key == "tool")
                            .and_then(|kv| kv.value.as_ref())
                            .and_then(|v| match v.value.as_ref() {
                                Some(opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s)) => Some(s.clone()),
                                _ => None,
                            });
                        if let Some(tool) = tool {
                            *counts.entry(tool).or_insert(0) += 1;
                        }
                    }
                }
            }
        }
        counts
    }
}

/// One HTTP response: status code, lower-cased headers, body (mcp_http.rs).
struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> J {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {:?}", self.body))
    }
}

fn parse(raw: &[u8]) -> Resp {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .map(|(h, b)| (h.to_string(), b.to_string()))
        .unwrap_or_else(|| (text.to_string(), String::new()));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Resp {
        status,
        headers,
        body,
    }
}

fn request(port: u16, headers: &[(&str, &str)], body: &str) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json, text/event-stream\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body.as_bytes()).unwrap();
    s.flush().unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    parse(&raw)
}

const BEARER: (&str, &str) = ("Authorization", "Bearer http-tok");

fn rpc(id: u64, method: &str, params: J) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// POST /mcp initialize with the bearer; returns the session id.
fn initialize(port: u16) -> String {
    let r = request(
        port,
        &[BEARER],
        &rpc(
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
             "clientInfo": {"name": "http-test", "version": "0"}}),
        ),
    );
    assert_eq!(r.status, 200, "initialize: {}", r.body);
    assert!(
        r.json()["result"]["protocolVersion"].is_string(),
        "{}",
        r.body
    );
    let sid = r
        .header("mcp-session-id")
        .expect("initialize assigns Mcp-Session-Id")
        .to_string();
    assert!(
        sid.len() >= 32 && sid.chars().all(|c| c.is_ascii_hexdigit()),
        "session id {sid:?}"
    );
    sid
}

fn tool(port: u16, sid: &str, id: u64, name: &str, args: J) -> J {
    let r = request(
        port,
        &[BEARER, ("Mcp-Session-Id", sid)],
        &rpc(id, "tools/call", json!({"name": name, "arguments": args})),
    );
    assert_eq!(r.status, 200, "{name}: {}", r.body);
    let text = r.json()["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    serde_json::from_str(&text).unwrap()
}

/// Raw GET, Connection: close (the scrape has no auth).
fn http_get(port: u16, path: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n"
    )
    .unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    buf.split("\r\n\r\n").nth(1).unwrap_or_default().to_string()
}

struct Server {
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn ag50_every_tool_call_leaves_a_trace_and_counts_on_the_scrape() {
    let collector = Collector::start();
    let root = tmp("obs");
    let metrics_port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
        .arg("serve")
        .args(["--metrics-addr", &format!("127.0.0.1:{metrics_port}")])
        .args(["--tcp-token", "http-tok::admin"])
        .env(
            "AIKOQL_OTEL_ENDPOINT",
            format!("http://127.0.0.1:{}", collector.port),
        )
        .env("HOME", root.join("home")) // no embedding model dir: text-only recall
        .arg(root.join("db"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn aikoql-mcp serve");
    let _server = Server { child };

    // Wait for the metrics listener before the first request.
    let deadline = Instant::now() + Duration::from_secs(20);
    while TcpStream::connect(("127.0.0.1", metrics_port)).is_err() {
        assert!(Instant::now() < deadline, "metrics listener never came up");
        std::thread::sleep(Duration::from_millis(50));
    }

    let sid = initialize(metrics_port);
    let _ = tool(
        metrics_port,
        &sid,
        2,
        "remember",
        json!({"subject": "mcp-agent", "type_name": "ObsThing", "properties": {"tag": "alpha"}}),
    );
    let _ = tool(
        metrics_port,
        &sid,
        3,
        "remember",
        json!({"subject": "mcp-agent", "type_name": "ObsThing", "properties": {"tag": "beta"}}),
    );
    let recall = tool(
        metrics_port,
        &sid,
        4,
        "recall",
        json!({"subject": "mcp-agent", "query": "zzz-no-such-token"}),
    );
    assert!(
        recall["items"].as_array().is_some_and(|a| a.is_empty()),
        "the recall must come back empty for the quality counter: {recall}"
    );

    // The batch exporter flushes on its schedule — poll until every call's
    // span has landed (the gate: a trace for EVERY tool call).
    let deadline = Instant::now() + Duration::from_secs(25);
    let counts = loop {
        let counts = collector.tool_calls();
        if counts.get("remember").copied().unwrap_or(0) >= 2
            && counts.get("recall").copied().unwrap_or(0) >= 1
        {
            break counts;
        }
        assert!(
            Instant::now() < deadline,
            "traces never arrived for every call: {counts:?}"
        );
        std::thread::sleep(Duration::from_millis(250));
    };
    assert!(
        counts.get("remember").copied().unwrap_or(0) >= 2,
        "one tool_call span per remember: {counts:?}"
    );

    // The scrape carries the per-tool counters and the recall outcome.
    let metrics = http_get(metrics_port, "/metrics");
    assert!(
        metrics.contains("aikoql_tool_calls_total{tool=\"remember\"} 2"),
        "per-tool call counter: {metrics}"
    );
    assert!(
        metrics.contains("aikoql_tool_errors_total{tool=\"remember\"} 0"),
        "per-tool error counter: {metrics}"
    );
    assert!(
        metrics.contains("aikoql_recall_quality_total{outcome=\"empty\"} 1"),
        "the empty recall must count: {metrics}"
    );
}
