//! P5-M17b (ND-14) — the `index_create` tool: declaration through the MCP
//! surface, then a KOQL query through the tool path. RED: the tool does not
//! exist yet (the call answers "unknown tool: index_create").
//!
//! The binary must be built first: `cargo build --bin aikoql-mcp`
//! (cargo test does NOT build bins).

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

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

fn tmp_db(name: &str) -> PathBuf {
    let mut db = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    db.push(format!(
        "aikoql_m17b_{name}_{}_{}",
        std::process::id(),
        stamp
    ));
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
    db
}

/// The cli_contract stdio client (raw frames — tool errors visible).
struct StdioClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl StdioClient {
    fn start(db: &Path) -> Self {
        let cfg = db.with_file_name("aikoql-rate.toml");
        std::fs::write(&cfg, "[rate_limit]\nmax_calls_per_minute = 100000\n").unwrap();
        let mut child = Command::new(server_bin())
            .arg("--config")
            .arg(&cfg)
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn aikoql-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        StdioClient {
            child,
            stdin,
            stdout,
            next_id: 0,
        }
    }

    fn request_raw(&mut self, method: &str, params: J) -> J {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{}", frame).unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            let resp: J = serde_json::from_str(line.trim()).expect("valid json-rpc frame");
            if resp.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return resp;
            }
        }
    }

    fn call_raw(&mut self, name: &str, args: J) -> J {
        let r = self.request_raw("tools/call", json!({"name": name, "arguments": args}));
        let result = r.get("result").cloned().unwrap_or(J::Null);
        assert_eq!(
            result.get("isError").and_then(|b| b.as_bool()),
            Some(false),
            "tool error: {result}"
        );
        let text = result["content"][0]["text"].as_str().unwrap().to_string();
        serde_json::from_str(&text).expect("tool payload is json")
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn index_create_declares_then_koql_answers_through_the_tool_path() {
    let db = tmp_db("idx_tool");
    let mut c = StdioClient::start(&db);
    for (topic, body) in [("pet", "a"), ("wild", "b"), ("pet", "c"), ("wild", "d")] {
        let r = c.call_raw(
            "remember",
            json!({"subject": "alice", "type_name": "note",
                   "properties": {"topic": topic, "body": body}}),
        );
        assert!(r.get("koid").is_some(), "remember must succeed: {r}");
    }

    // RED: the declaration surface does not exist yet.
    let declared = c.call_raw(
        "index_create",
        json!({"name": "by_topic", "type_name": "note", "properties": ["topic"]}),
    );
    assert_eq!(
        declared.get("name").and_then(|v| v.as_str()),
        Some("by_topic"),
        "index_create must answer the declaration: {declared}"
    );

    let out = c.call_raw(
        "aikoql",
        json!({"subject": "alice", "query": "MATCH note WHERE topic == \"pet\" RETURN *"}),
    );
    let rows = out["results"].as_array().expect("query results");
    assert_eq!(
        rows.len(),
        2,
        "the declared index must not change answers: {out}"
    );
}
