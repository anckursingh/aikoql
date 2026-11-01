//! AG-50 e2e: every tool call leaves an OTLP trace on the collector, the
//! Prometheus scrape carries per-tool counters, and recall quality outcomes
//! are counted.
//!
//! The server runs in stdio (positional db) mode with `--metrics-addr` and
//! `AIKOQL_OTEL_ENDPOINT` pointing at an in-test OTLP/HTTP collector; the
//! exported protobuf is decoded with opentelemetry-proto, so the gate —
//! a trace for every tool call — is asserted on the wire, not on logs.

use prost::Message;
use serde_json::{json, Value as J};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
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

/// Stdio MCP client (pump thread), like tests/live_query.rs.
struct Client {
    child: Child,
    stdin: std::process::ChildStdin,
    frames: mpsc::Receiver<J>,
    next: u64,
}

impl Client {
    fn spawn(db: &PathBuf, otel_endpoint: &str, metrics_port: u16, home: &PathBuf) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
            .args(["--metrics-addr", &format!("127.0.0.1:{metrics_port}")])
            .env("AIKOQL_OTEL_ENDPOINT", otel_endpoint)
            .env("HOME", home) // no embedding model dir: text-only recall
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn aikoql-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                if stdout.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let frame: J = serde_json::from_str(line.trim())
                    .unwrap_or_else(|_| panic!("bad frame: {line:?}"));
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        let mut c = Client {
            child,
            stdin,
            frames: rx,
            next: 0,
        };
        c.call(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
        );
        c
    }

    fn call(&mut self, method: &str, params: J) -> J {
        self.next += 1;
        let id = self.next;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        loop {
            let frame = self
                .frames
                .recv_timeout(Duration::from_secs(30))
                .expect("no frame before deadline");
            if frame.get("id").is_none() {
                continue;
            }
            if frame["id"].as_u64() == Some(id) {
                return frame;
            }
        }
    }

    fn tool(&mut self, name: &str, args: J) -> J {
        let res = self.call("tools/call", json!({"name": name, "arguments": args}));
        let text = res["result"]["content"][0]["text"].as_str().unwrap().to_string();
        serde_json::from_str(&text).expect("tool payload is json")
    }
}

impl Drop for Client {
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
    let mut c = Client::spawn(
        &root.join("db"),
        &format!("http://127.0.0.1:{}", collector.port),
        metrics_port,
        &root.join("home"),
    );

    // Wait for the metrics listener before the first scrape.
    let deadline = Instant::now() + Duration::from_secs(20);
    while TcpStream::connect(("127.0.0.1", metrics_port)).is_err() {
        assert!(Instant::now() < deadline, "metrics listener never came up");
        std::thread::sleep(Duration::from_millis(50));
    }

    let _ = c.tool(
        "remember",
        json!({"subject": "mcp-agent", "type_name": "ObsThing", "properties": {"tag": "alpha"}}),
    );
    let _ = c.tool(
        "remember",
        json!({"subject": "mcp-agent", "type_name": "ObsThing", "properties": {"tag": "beta"}}),
    );
    let recall = c.tool(
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
