//! AG-12a — remember_facts: the calling agent extracts facts from its
//! conversation (it is the model, and has the conversation in context); the
//! server does what an LLM-only memory cannot — dedupe against what is
//! known, turn a contradiction into a Conflict KO instead of an overwrite,
//! supersede on an explicit update, and record authority + provenance
//! (the conversation and message range) on every fact.

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("facts-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

// shortcut: another copy of the private stdio client (debt ledger:
// connectors/mod.rs:457), extract a shared one when an eighth appears
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Mcp {
    fn serve(tag: &str) -> Self {
        let root = tmp(tag);
        let mut child = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
            .arg("serve")
            .arg(root.join("db"))
            .env("HOME", root.join("home"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn aikoql-mcp serve");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            stdin,
            stdout,
            next: 0,
        };
        m.call(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
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

    fn facts(&mut self, conversation: &str, facts: J) -> Vec<J> {
        let r = self.tool(
            "remember_facts",
            json!({"conversation_id": conversation, "facts": facts}),
        );
        r["results"]
            .as_array()
            .unwrap_or_else(|| panic!("remember_facts: {r}"))
            .clone()
    }
}

#[test]
fn f01_new_facts_land_with_provenance_and_are_recalled() {
    let mut m = Mcp::serve("new");
    let r = m.facts("chat-1", json!([
        {"statement": "The team deploys on Thursdays", "key": "deploy.day", "value": "thursday", "messages": [3, 4]},
        {"statement": "Priya owns the billing service", "speaker": "user"}
    ]));
    assert_eq!(r.len(), 2);
    assert!(r.iter().all(|f| f["status"] == "created"), "{r:?}");
    let koid = r[0]["koid"].as_str().unwrap();
    let ko = m.tool("explain", json!({"koid": koid}));
    let text = ko.to_string();
    assert!(
        text.contains("conversation:chat-1"),
        "the conversation is the evidence: {ko}"
    );
    assert!(
        text.contains("messages 3-4"),
        "with the message range: {ko}"
    );
    assert!(
        text.contains("llm_inferred"),
        "an extracted fact's default authority: {ko}"
    );
    let hits = m.tool(
        "recall",
        json!({"query": "when do we deploy", "wait_for_freshness_ms": 5000}),
    );
    assert!(hits.to_string().contains(koid), "recall finds it: {hits}");
}

#[test]
fn f02_a_repeated_fact_is_a_duplicate_not_a_second_object() {
    let mut m = Mcp::serve("dup");
    let first = m.facts("chat-1", json!([{"statement": "The standup is at 9:30"}]));
    let again = m.facts(
        "chat-2",
        json!([{"statement": "  the standup is at 9:30. "}]),
    );
    assert_eq!(again[0]["status"], "duplicate", "{again:?}");
    assert_eq!(
        again[0]["koid"], first[0]["koid"],
        "points at the existing fact"
    );
}

#[test]
fn f03_a_contradiction_becomes_a_conflict_never_an_overwrite() {
    let mut m = Mcp::serve("conflict");
    let first = m.facts("chat-1", json!([{"statement": "The API limit is 100 per minute", "key": "api.limit", "value": "100"}]));
    let old = first[0]["koid"].as_str().unwrap().to_string();
    let r = m.facts("chat-2", json!([{"statement": "The API limit is 300 per minute", "key": "api.limit", "value": "300"}]));
    assert_eq!(r[0]["status"], "conflict", "{r:?}");
    let conflict = r[0]["conflict"]
        .as_str()
        .unwrap_or_else(|| panic!("the Conflict KOID: {r:?}"));
    let original = m.tool("get", json!({"koid": old}));
    assert!(
        original.to_string().contains("100 per minute"),
        "the original is untouched: {original}"
    );
    let hits = m.tool(
        "recall",
        json!({"query": "API limit per minute", "wait_for_freshness_ms": 5000}),
    );
    let item = hits["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["koid"] == old)
        .unwrap_or_else(|| panic!("{hits}"));
    assert_eq!(
        item["conflicts"],
        json!([conflict]),
        "recall shows the open conflict: {item}"
    );
}

#[test]
fn f04_an_explicit_update_supersedes() {
    let mut m = Mcp::serve("supersede");
    let first = m.facts(
        "chat-1",
        json!([{"statement": "The office is in Berlin", "key": "office.city", "value": "berlin"}]),
    );
    let old = first[0]["koid"].as_str().unwrap().to_string();
    let r = m.facts("chat-2", json!([{"statement": "The office moved to Lisbon", "key": "office.city", "value": "lisbon", "supersedes": old}]));
    assert_eq!(r[0]["status"], "superseded", "{r:?}");
    let hits = m.tool(
        "recall",
        json!({"query": "which city is the office in", "wait_for_freshness_ms": 5000}),
    );
    let s = hits.to_string();
    assert!(
        s.contains("Lisbon") && !s.contains(&old),
        "only the successor is current: {hits}"
    );
}

#[test]
fn f05_a_bad_fact_fails_alone() {
    let mut m = Mcp::serve("bad");
    let r = m.facts(
        "chat-1",
        json!([{"key": "x"}, {"statement": "Lunch is at noon"}]),
    );
    assert_eq!(r[0]["status"], "error", "{r:?}");
    assert!(
        r[0]["error"]
            .as_str()
            .is_some_and(|e| e.contains("statement")),
        "{r:?}"
    );
    assert_eq!(
        r[1]["status"], "created",
        "the next fact still lands: {r:?}"
    );
}
