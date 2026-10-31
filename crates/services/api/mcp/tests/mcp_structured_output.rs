//! AG-04: structured tool output (MCP 2025-06-18). The core tools' defs
//! carry `outputSchema`; a successful call returns `structuredContent`
//! beside the text content (older clients ignore the extra field); an
//! error returns no structured content.

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("structured-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Client {
    fn serve(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
            .arg("serve")
            .arg(root.join("db"))
            .env("AIKOQL_MEMORY_DIR", root.join("memory"))
            .env("HOME", root.join("home")) // no embedding model: text recall
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn aikoql-mcp serve");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut c = Client {
            child,
            stdin,
            stdout,
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
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server closed stdout"
            );
            let frame: J =
                serde_json::from_str(line.trim()).unwrap_or_else(|_| panic!("bad frame: {line:?}"));
            if frame["id"].as_u64() == Some(id) {
                return frame;
            }
        }
    }

    /// The raw tools/call result (content + structuredContent, no unwrap).
    fn raw_call(&mut self, name: &str, args: J) -> J {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        r["result"].clone()
    }
}

fn def(c: &mut Client, name: &str) -> J {
    let list = c.call("tools/list", json!({}));
    list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("no tool {name} in tools/list"))
        .clone()
}

/// s001: core tools carry outputSchema; tools outside the core surface don't.
#[test]
fn s001_core_defs_carry_output_schema() {
    let root = tmp("defs");
    let mut c = Client::serve(&root);
    let remember = def(&mut c, "remember");
    let schema = remember
        .get("outputSchema")
        .unwrap_or_else(|| panic!("remember has no outputSchema: {remember}"));
    assert_eq!(schema["type"], "object", "{remember}");
    assert!(
        schema["required"]
            .as_array()
            .is_some_and(|r| r.contains(&json!("koid"))),
        "{remember}"
    );
    let recall = def(&mut c, "recall");
    let schema = recall
        .get("outputSchema")
        .unwrap_or_else(|| panic!("recall has no outputSchema: {recall}"));
    assert_eq!(schema["type"], "object", "{recall}");
    let backup = def(&mut c, "backup");
    assert!(
        backup.get("outputSchema").is_none(),
        "non-core tools stay unstructured: {backup}"
    );
}

/// s002: a successful call returns structuredContent matching the text.
#[test]
fn s002_structured_content_beside_text() {
    let root = tmp("content");
    let mut c = Client::serve(&root);
    let r = c.raw_call(
        "remember",
        json!({"subject": "mcp-agent", "type_name": "note", "properties": {"via": "ag04"}}),
    );
    let text = r["content"][0]["text"].as_str().unwrap();
    let parsed: J = serde_json::from_str(text).unwrap();
    let sc = r
        .get("structuredContent")
        .unwrap_or_else(|| panic!("no structuredContent: {r}"));
    assert!(sc.is_object(), "{r}");
    assert_eq!(sc["koid"], parsed["koid"], "structured == text: {r}");
    assert_eq!(sc["version"], parsed["version"], "structured == text: {r}");
}

/// s003: an error result carries no structuredContent.
#[test]
fn s003_errors_stay_unstructured() {
    let root = tmp("error");
    let mut c = Client::serve(&root);
    let r = c.raw_call(
        "get",
        json!({"subject": "mcp-agent", "koid": "ffffffffffffffffffffffffffffffff"}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        r.get("structuredContent").is_none(),
        "errors carry no structuredContent: {r}"
    );
}

/// s004: recall's structuredContent matches its declared schema (items
/// array present, truncated boolean).
#[test]
fn s004_recall_structured() {
    let root = tmp("recall");
    let mut c = Client::serve(&root);
    let r = c.raw_call(
        "recall",
        json!({"subject": "mcp-agent", "query": "anything"}),
    );
    let sc = r
        .get("structuredContent")
        .unwrap_or_else(|| panic!("no structuredContent: {r}"));
    assert!(sc["items"].is_array(), "{r}");
    assert!(sc["truncated"].is_boolean(), "{r}");
}
