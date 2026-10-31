//! AG-03: MCP resources, subscriptions and prompts. The three spec resources
//! (`aikoql://ko/{koid}`, `aikoql://conflicts/open`,
//! `aikoql://session/{id}/context`) answer as text over `resources/read`;
//! `resources/subscribe` rides the kernel's event system and emits
//! `notifications/resources/updated`; the two prompts compile session
//! context and open-conflict reviews.

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("resprompt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// A scripted stdio client that keeps server-initiated notifications aside.
struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
    /// every server-initiated frame (notifications/resources/updated etc.)
    notes: Vec<J>,
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
            notes: vec![],
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
            if frame.get("id").is_none() {
                self.notes.push(frame);
                continue;
            }
            if frame["id"].as_u64() == Some(id) {
                return frame;
            }
        }
    }

    fn tool(&mut self, name: &str, args: J) -> J {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: {r}"));
        serde_json::from_str(text).unwrap_or_else(|_| json!(text))
    }
}

/// r001: resources/list advertises the three spec resources.
#[test]
fn r001_resources_list() {
    let root = tmp("list");
    let mut c = Client::serve(&root);
    let r = c.call("resources/list", json!({}));
    let uris: Vec<&str> = r["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["uri"].as_str().unwrap())
        .collect();
    assert_eq!(
        uris,
        vec![
            "aikoql://ko/{koid}",
            "aikoql://conflicts/open",
            "aikoql://session/{id}/context",
        ],
        "the three AG-03 resources, in the spec order: {r}"
    );
}

/// r002: aikoql://ko/{koid} reads the knowledge object as text.
#[test]
fn r002_read_ko() {
    let root = tmp("ko");
    let mut c = Client::serve(&root);
    let put = c.tool(
        "remember",
        json!({"subject": "mcp-agent", "type_name": "note", "properties": {"via": "resource"}}),
    );
    let koid = put["koid"].as_str().unwrap();
    let r = c.call(
        "resources/read",
        json!({"uri": format!("aikoql://ko/{koid}"), "subject": "mcp-agent"}),
    );
    let text = r["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains(koid), "the KO text names its koid: {r}");
    assert!(
        text.contains("resource"),
        "the KO text carries its properties: {r}"
    );
}

/// r003: aikoql://conflicts/open lists the unresolved conflicts.
#[test]
fn r003_read_open_conflicts() {
    let root = tmp("conflicts");
    let mut c = Client::serve(&root);
    let a = c.tool(
        "remember",
        json!({"subject": "mcp-agent", "type_name": "Claim", "properties": {"key": "x", "value": "a"}}),
    );
    let ct = c.tool(
        "contradict",
        json!({"subject": "mcp-agent", "claim": a["koid"], "counter_type": "Claim",
               "properties": {"key": "x", "value": "b"},
               "evidence": [{"source_artifact": "test", "method": "llm_inference"}]}),
    );
    assert!(ct["conflict"].is_string(), "contradict failed: {ct}");
    let r = c.call(
        "resources/read",
        json!({"uri": "aikoql://conflicts/open", "subject": "mcp-agent"}),
    );
    let text = r["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains("open"), "the conflict read says open: {r}");
    assert!(
        text.contains(a["koid"].as_str().unwrap()),
        "it names the conflicted claim: {r}"
    );
}

/// r004: aikoql://session/{id}/context answers for the session's run id and
/// refuses an unknown id.
#[test]
fn r004_read_session_context() {
    let root = tmp("session");
    let mut c = Client::serve(&root);
    let init = c.tool(
        "session_init",
        json!({"agent_id": "ada", "run_id": "run-7"}),
    );
    let run_id = init["session"]["run_id"].as_str().unwrap();
    let r = c.call(
        "resources/read",
        json!({"uri": format!("aikoql://session/{run_id}/context"), "subject": "ada"}),
    );
    let text = r["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains("ada"), "the context names the agent: {r}");
    assert!(text.contains("run-7"), "the context names the run: {r}");
    let bad = c.call(
        "resources/read",
        json!({"uri": "aikoql://session/run-other/context", "subject": "ada"}),
    );
    assert!(
        bad["error"].is_object(),
        "an unknown session id is an error: {bad}"
    );
}

/// r005: prompts/list names both prompts; prompts/get compiles them.
#[test]
fn r005_prompts() {
    let root = tmp("prompts");
    let mut c = Client::serve(&root);
    let r = c.call("prompts/list", json!({}));
    let names: Vec<&str> = r["result"]["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["aikoql-context", "aikoql-review-conflicts"],
        "{r}"
    );
    let g = c.call(
        "prompts/get",
        json!({"name": "aikoql-context", "arguments": {"task": "plan AG-03"}}),
    );
    let text = g["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("plan AG-03"),
        "the context prompt embeds the task: {g}"
    );
    let g = c.call(
        "prompts/get",
        json!({"name": "aikoql-review-conflicts", "arguments": {}}),
    );
    let text = g["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("conflict"),
        "the review prompt is about conflicts: {g}"
    );
}

/// r006: resources/subscribe emits notifications/resources/updated when a
/// conflict is created; resources/unsubscribe stops them.
#[test]
fn r006_subscribe_conflicts() {
    let root = tmp("subscribe");
    let mut c = Client::serve(&root);
    let a = c.tool(
        "remember",
        json!({"subject": "mcp-agent", "type_name": "Claim", "properties": {"key": "y", "value": "a"}}),
    );
    let sub = c.call(
        "resources/subscribe",
        json!({"uri": "aikoql://conflicts/open", "subject": "mcp-agent"}),
    );
    assert!(sub["result"].is_object(), "subscribe answers: {sub}");
    let ct = c.tool(
        "contradict",
        json!({"subject": "mcp-agent", "claim": a["koid"], "counter_type": "Claim",
               "properties": {"key": "y", "value": "b"},
               "evidence": [{"source_artifact": "test", "method": "llm_inference"}]}),
    );
    assert!(ct["conflict"].is_string(), "contradict failed: {ct}");
    // the notification rides the same stream as tools/list_changed — it may
    // already have arrived during the contradict call
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if c.notes
            .iter()
            .any(|n| n["method"] == "notifications/resources/updated")
        {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("no resources/updated after a new conflict: {:?}", c.notes);
        }
        let _ = c.call("ping", json!({})); // keep the reader busy
    }
    let updated = c
        .notes
        .iter()
        .find(|n| n["method"] == "notifications/resources/updated")
        .unwrap();
    assert_eq!(
        updated["params"]["uri"], "aikoql://conflicts/open",
        "{updated}"
    );
    let un = c.call(
        "resources/unsubscribe",
        json!({"uri": "aikoql://conflicts/open"}),
    );
    assert!(un["result"].is_object(), "unsubscribe answers: {un}");
}
