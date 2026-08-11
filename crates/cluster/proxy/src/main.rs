//! Aikoql Cluster Proxy — hash-based MCP request router.
//!
//! Usage:
//!   aikoql-proxy --listen 127.0.0.1:8080 127.0.0.1:9091 127.0.0.1:9092
//!
//! Routes writes by KOID hash, broadcasts reads, merges results. Maintains
//! persistent connections to shards with retry on failure.

use serde_json::{json, Value as J};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const WRITE_TOOLS: &[&str] = &["remember", "forget", "evolve", "relate"];
const READ_TOOLS: &[&str] = &[
    "get",
    "find_similar",
    "trace",
    "explain",
    "prove",
    "aikoql",
    "traverse",
    "eval_recall",
    "eval_staleness",
    "eval_contradictions",
    "verify",
    "metrics",
];
const MAX_RETRIES: usize = 3;

// ---------------------------------------------------------------------------
// Shard connection pool
// ---------------------------------------------------------------------------

struct Shard {
    addr: String,
    conn: Mutex<Option<TcpStream>>,
}

impl Shard {
    fn new(addr: &str) -> Self {
        Shard {
            addr: addr.into(),
            conn: Mutex::new(None),
        }
    }

    /// Send a JSON-RPC request and return the response. Retries with reconnect
    /// on failure.
    fn send(&self, request: &J) -> J {
        let mut last_err = "unknown".to_string();
        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                thread::sleep(Duration::from_millis(50 * (1 << attempt) as u64));
            }
            match self.try_send(request) {
                Ok(resp) => return resp,
                Err(e) => {
                    last_err = e;
                    // Drop the dead connection so the next attempt reconnects.
                    *self.conn.lock().unwrap() = None;
                }
            }
        }
        json!({"error": {"code": -32000, "message": format!("shard {} unreachable after {} retries: {}", self.addr, MAX_RETRIES, last_err)}})
    }

    fn try_send(&self, request: &J) -> Result<J, String> {
        let mut guard = self.conn.lock().unwrap();
        if guard.is_none() {
            let stream = TcpStream::connect(&self.addr)
                .map_err(|e| format!("connect {}: {}", self.addr, e))?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            *guard = Some(stream);
        }
        let stream = guard.as_mut().unwrap();

        let req_str = request.to_string();
        writeln!(stream, "{}", req_str).map_err(|e| format!("write: {}", e))?;
        stream.flush().map_err(|e| format!("flush: {}", e))?;

        let mut reader = BufReader::new(&*stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| format!("read: {}", e))?;

        serde_json::from_str(line.trim()).map_err(|e| format!("parse: {}", e))
    }
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

fn shard_for_koid(koid: &str, num_shards: usize) -> usize {
    let mut h = DefaultHasher::new();
    koid.hash(&mut h);
    h.finish() as usize % num_shards
}

fn extract_koid(_tool_name: &str, args: &J) -> String {
    args.get("koid")
        .or_else(|| args.get("from"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            let subj = args
                .get("subject")
                .and_then(|s| s.as_str())
                .unwrap_or("default");
            let mut h = DefaultHasher::new();
            subj.hash(&mut h);
            format!("{:x}", h.finish())
        })
}

fn route_tool_call(msg: &J, tool_name: &str, args: &J, shards: &[Arc<Shard>]) -> J {
    let id = msg.get("id").cloned().unwrap_or(J::Null);

    if WRITE_TOOLS.contains(&tool_name) {
        let koid = extract_koid(tool_name, args);
        let shard = shard_for_koid(&koid, shards.len());
        return shards[shard].send(msg);
    }

    if READ_TOOLS.contains(&tool_name) {
        let mut all_results: Vec<J> = Vec::new();
        let mut first_payload: Option<J> = None;
        let mut errors: Vec<String> = Vec::new();
        for shard in shards {
            let resp = shard.send(msg);
            if resp.get("error").is_some() {
                errors.push(shard.addr.clone());
                continue;
            }
            if let Some(result) = resp.get("result") {
                if let Some(content) = result.get("content") {
                    if let Some(text) = content[0].get("text") {
                        if let Ok(payload) =
                            serde_json::from_str::<J>(text.as_str().unwrap_or("{}"))
                        {
                            if let Some(arr) = payload.get("results").and_then(|r| r.as_array()) {
                                all_results.extend(arr.iter().cloned());
                            }
                            if first_payload.is_none() && payload.get("results").is_none() {
                                first_payload = Some(payload);
                            }
                        }
                    }
                }
            }
        }
        // If all shards errored, return the first error.
        if errors.len() == shards.len() {
            return json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":format!("all shards unreachable: {:?}", errors)}});
        }
        let merged = if let Some(p) = first_payload {
            json!({"content": [{"type": "text", "text": p.to_string()}], "isError": false})
        } else {
            json!({"content": [{"type": "text", "text": json!({"results": all_results}).to_string()}], "isError": false})
        };
        return json!({"jsonrpc":"2.0","id":id,"result":merged});
    }

    // Unknown tool — forward to first shard.
    shards[0].send(msg)
}

// ---------------------------------------------------------------------------
// Client handler
// ---------------------------------------------------------------------------

fn handle_client(mut stream: TcpStream, shards: Arc<Vec<Arc<Shard>>>) {
    let reader = BufReader::new(stream.try_clone().expect("clone stream"));
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: J = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let resp = json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":format!("parse error: {}", e)}});
                writeln!(stream, "{}", resp).ok();
                continue;
            }
        };

        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let response = match method {
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(J::Null);
                let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(J::Null);
                route_tool_call(&msg, tool_name, &args, &shards)
            }
            "initialize" => json!({
                "jsonrpc":"2.0",
                "id": msg["id"],
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "aikoql-proxy", "version": "0.1"}
                }
            }),
            "tools/list" => {
                // Forward to first available shard.
                shards[0].send(&msg)
            }
            "ping" => json!({"jsonrpc":"2.0","id":msg["id"],"result":{}}),
            _ => json!({
                "jsonrpc":"2.0",
                "id": msg.get("id").cloned().unwrap_or(J::Null),
                "error": {"code": -32601, "message": format!("unknown method: {}", method)}
            }),
        };

        writeln!(stream, "{}", response).ok();
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut listen_addr = "127.0.0.1:8080".to_string();
    let mut shard_addrs: Vec<String> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--listen" => {
                listen_addr = args.get(i + 1).cloned().unwrap_or(listen_addr);
                i += 2;
            }
            _ => {
                shard_addrs.push(args[i].clone());
                i += 1;
            }
        }
    }

    if shard_addrs.is_empty() {
        eprintln!("Usage: aikoql-proxy [--listen addr] <shard1> <shard2> ...");
        std::process::exit(1);
    }

    let shards: Arc<Vec<Arc<Shard>>> = Arc::new(
        shard_addrs
            .iter()
            .map(|a| Arc::new(Shard::new(a)))
            .collect(),
    );
    eprintln!(
        "Proxy listening on {}, {} shards: {:?}",
        listen_addr,
        shards.len(),
        shard_addrs
    );

    // Health check: verify all shards respond to ping.
    let ping = json!({"jsonrpc":"2.0","id":1,"method":"ping","params":{}});
    for shard in shards.iter() {
        match shard.try_send(&ping) {
            Ok(_) => eprintln!("  shard {} OK", shard.addr),
            Err(e) => eprintln!("  shard {} WARN: {}", shard.addr, e),
        }
    }

    let listener = TcpListener::bind(&listen_addr).expect("bind proxy");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let shards = Arc::clone(&shards);
                thread::spawn(move || handle_client(stream, shards));
            }
            Err(e) => eprintln!("accept error: {}", e),
        }
    }
}
