//! AG-12b: MCP sampling as the optional extraction path. With a client that
//! advertises `sampling`, summarize_conversation asks the client's model for
//! candidate facts against a fixed JSON schema, then judges them with the
//! same code as remember_facts (dedupe, contradiction → Conflict KO,
//! message-range provenance). Without the capability — or when the sampled
//! answer is unusable — the deterministic path answers as before.

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("sampling-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// A scripted stdio client that can answer sampling/createMessage requests
/// from the server (the debt ledger's Mcp harness copy — this one reads
/// both directions, which the plain harness cannot).
struct Sampler {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
    /// every sampling/createMessage request the server sent us
    asked: Vec<J>,
    /// the canned model answer (content text); None = no capability
    canned: Option<String>,
}

impl Drop for Sampler {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Sampler {
    fn serve(root: &Path, sampling: bool, canned: Option<String>) -> Self {
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
        let mut m = Sampler {
            child,
            stdin,
            stdout,
            next: 0,
            asked: vec![],
            canned,
        };
        let caps = if sampling { json!({"sampling": {}}) } else { json!({}) };
        m.call(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": caps}),
        );
        m
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
            if let Ok(frame) = serde_json::from_str::<J>(line.trim()) {
                // a server-initiated request: answer it with the canned model
                if frame.get("method").and_then(J::as_str) == Some("sampling/createMessage") {
                    self.asked.push(frame.clone());
                    let text = self.canned.clone().unwrap_or_else(|| "[]".into());
                    writeln!(
                        self.stdin,
                        "{}",
                        json!({
                            "jsonrpc": "2.0",
                            "id": frame["id"],
                            "result": {"role": "assistant", "content": {"type": "text", "text": text},
                                       "model": "scripted", "stopReason": "endTurn"}
                        })
                    )
                    .unwrap();
                    self.stdin.flush().unwrap();
                    continue;
                }
                if frame["id"].as_u64() == Some(id) {
                    return frame;
                }
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

fn summarize_args() -> J {
    json!({
        "conversation_id": "s-conv",
        "messages": [
            {"speaker": "user", "ts_ms": 1, "text": "My name is Ada and I live in Berlin."},
            {"speaker": "assistant", "ts_ms": 2, "text": "Nice to meet you, Ada!"},
            {"speaker": "user", "ts_ms": 3, "text": "I am moving to Lisbon next month."}
        ],
        "evidence": [
            {"source_artifact": "conversation:s-conv", "method": "llm_inference", "location": "messages 1-3"}
        ]
    })
}

/// The model's candidate facts: the second contradicts the first (same key,
/// another value) — the server, not the model, must record that conflict.
fn canned_facts() -> String {
    json!([
        {"statement": "Ada lives in Berlin", "key": "person.ada.city", "value": "Berlin",
         "speaker": "user", "messages": [1, 1]},
        {"statement": "Ada is moving to Lisbon", "key": "person.ada.city", "value": "Lisbon",
         "speaker": "user", "messages": [3, 3]}
    ])
    .to_string()
}

fn deterministic_keys(r: &J) -> Vec<String> {
    let mut keys: Vec<String> = r
        .as_object()
        .unwrap_or_else(|| panic!("not an object: {r}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

#[test]
fn s001_a_sampling_client_is_asked_then_its_facts_are_judged() {
    let root = tmp("s001");
    let mut m = Sampler::serve(&root, true, Some(canned_facts()));
    let r = m.tool("summarize_conversation", summarize_args());
    assert_eq!(m.asked.len(), 1, "the server must ask exactly once — RED: it never asks");
    let params = &m.asked[0]["params"];
    assert_eq!(
        params["messages"].as_array().map(|a| a.len()),
        Some(3),
        "the request carries the conversation"
    );
    assert!(
        params["systemPrompt"]
            .as_str()
            .is_some_and(|p| p.contains("JSON")),
        "the prompt pins the extraction schema: {}",
        params["systemPrompt"]
    );
    assert_eq!(r["sampling"], json!(true), "{r}");
    let facts = r["facts"]
        .as_array()
        .unwrap_or_else(|| panic!("judged facts: {r}"));
    let statuses: Vec<String> = facts
        .iter()
        .map(|f| f["status"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        statuses,
        vec!["created".to_string(), "conflict".to_string()],
        "the server judges: the second fact contradicts the first — {r}"
    );
    // provenance lands on the stored fact: the message range the model named
    let created = facts[0]["koid"].as_str().unwrap_or_else(|| panic!("{r}"));
    let ko = m.tool("get", json!({"koid": created}));
    let loc = ko["extensions"]["evidence"][0]["location"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert_eq!(loc, "messages 1-1", "{ko}");
}

#[test]
fn s002_without_sampling_the_deterministic_shape_is_unchanged() {
    let root = tmp("s002");
    let mut m = Sampler::serve(&root, false, None);
    let r = m.tool("summarize_conversation", summarize_args());
    assert!(m.asked.is_empty(), "no capability: no sampling request");
    assert_eq!(
        deterministic_keys(&r),
        vec!["koid".to_string(), "message_count".to_string(), "type_name".to_string(), "version".to_string()],
        "the old response shape, byte-for-byte: {r}"
    );
}

#[test]
fn s003_an_unusable_sampled_answer_falls_back_to_deterministic() {
    let root = tmp("s003");
    let mut m = Sampler::serve(&root, true, Some("not the facts schema at all".into()));
    let r = m.tool("summarize_conversation", summarize_args());
    assert_eq!(m.asked.len(), 1, "sampling was attempted");
    assert_eq!(
        deterministic_keys(&r),
        vec!["koid".to_string(), "message_count".to_string(), "type_name".to_string(), "version".to_string()],
        "a garbage answer degrades to the deterministic shape: {r}"
    );
}
