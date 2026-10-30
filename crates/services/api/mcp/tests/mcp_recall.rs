//! AG-02 — one `recall` tool: retrieval as a single good call. Hybrid
//! search (text, plus the query's embedding when a model is loaded) over
//! current truth — superseded generations stay out — with each item's
//! epistemic status, authority, a provenance one-liner and the open
//! conflicts touching it, inside a token budget.

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("recall-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

// shortcut: a fourth copy of the private stdio client (see the debt ledger
// row for connectors/mod.rs:457), extract a shared one when a fifth appears
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    db: PathBuf,
    next: u64,
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.db);
    }
}

impl Mcp {
    fn serve(tag: &str, extra: &[&str]) -> Self {
        let db = tmp(tag);
        let mut child = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
            .arg("serve")
            .args(extra)
            .arg(&db)
            // no embedding model: recall must answer text-only
            .env("HOME", db.with_extension("home"))
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
            db,
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

    fn note(&mut self, body: &str) -> String {
        let r = self.tool(
            "remember",
            json!({"subject": "admin", "type_name": "note", "properties": {"body": body}}),
        );
        r["koid"]
            .as_str()
            .unwrap_or_else(|| panic!("remember: {r}"))
            .to_string()
    }

    fn recall(&mut self, args: J) -> J {
        let mut args = args;
        args["subject"] = json!("admin");
        args["wait_for_freshness_ms"] = json!(5000);
        self.tool("recall", args)
    }
}

const EVIDENCE: &str = r#"[{"source_artifact": "test.md", "method": "human_provided"}]"#;

fn items(r: &J) -> &Vec<J> {
    r["items"]
        .as_array()
        .unwrap_or_else(|| panic!("recall answered no items: {r}"))
}

fn find<'a>(r: &'a J, koid: &str) -> Option<&'a J> {
    items(r).iter().find(|i| i["koid"] == koid)
}

#[test]
fn r01_one_call_finds_by_text_with_status_and_provenance() {
    let mut m = Mcp::serve("find", &[]);
    let hit = m.note("the deploy window is Tuesday afternoon");
    m.note("lunch is at noon");
    let r = m.recall(json!({"query": "deploy window"}));
    let item = find(&r, &hit).unwrap_or_else(|| panic!("the matching note is recalled: {r}"));
    assert_eq!(item["type_name"], "note");
    assert!(item["status"].is_string(), "{item}");
    assert!(
        item["why"].as_str().is_some_and(|w| !w.is_empty()),
        "{item}"
    );
    assert!(
        item["snippet"]
            .as_str()
            .is_some_and(|s| s.contains("Tuesday")),
        "{item}"
    );
    assert_eq!(item["conflicts"], json!([]), "{item}");
}

#[test]
fn r02_a_superseded_generation_stays_out() {
    let mut m = Mcp::serve("supersede", &[]);
    let old = m.note("the office is in Berlin");
    let ev: J = serde_json::from_str(EVIDENCE).unwrap();
    let sup = m.tool(
        "supersede",
        json!({"subject": "admin", "old": old, "type_name": "note",
        "properties": {"body": "the office is in Lisbon"}, "evidence": ev, "reason": "moved"}),
    );
    let new = sup["new"]
        .as_str()
        .or(sup["koid"].as_str())
        .unwrap_or_else(|| panic!("supersede: {sup}"))
        .to_string();
    let r = m.recall(json!({"query": "office"}));
    assert!(
        find(&r, &old).is_none(),
        "the superseded generation is recalled: {r}"
    );
    assert!(find(&r, &new).is_some(), "the successor is recalled: {r}");
}

#[test]
fn r03_a_contested_claim_carries_its_open_conflict() {
    let mut m = Mcp::serve("contest", &[]);
    let claim = m.note("the API limit is 100 requests per minute");
    let ev: J = serde_json::from_str(EVIDENCE).unwrap();
    let c = m.tool(
        "contradict",
        json!({"subject": "admin", "claim": claim, "counter_type": "note",
        "properties": {"body": "the API limit is 300 requests per minute"}, "evidence": ev}),
    );
    let conflict = c["conflict"]
        .as_str()
        .unwrap_or_else(|| panic!("contradict: {c}"))
        .to_string();
    let r = m.recall(json!({"query": "API limit requests"}));
    let item = find(&r, &claim).unwrap_or_else(|| panic!("the contested claim is recalled: {r}"));
    assert_eq!(item["conflicts"], json!([conflict]), "{item}");
    assert!(
        items(&r)
            .iter()
            .all(|i| i["type_name"] != "aikoql:conflict"),
        "conflict records are not answers: {r}"
    );
}

#[test]
fn r04_the_answer_stays_inside_the_token_budget() {
    let mut m = Mcp::serve("budget", &[]);
    for i in 0..20 {
        m.note(&format!(
            "budget report {i}: {}",
            "quarterly spending detail ".repeat(80)
        ));
    }
    let r = m.recall(json!({"query": "budget report", "max_tokens": 600}));
    let n = items(&r).len();
    assert!(n >= 1, "{r}");
    let used = serde_json::to_string(&r["items"]).unwrap().len();
    // the budget is in tokens; ~4 bytes per token is the estimate recall uses
    assert!(
        used <= 600 * 4,
        "{used} bytes for a 600-token budget ({n} items)"
    );
    assert_eq!(r["truncated"], json!(true), "{r}");
}

#[test]
fn r05_recall_replaces_find_similar_in_the_core_profile() {
    let mut m = Mcp::serve("core", &["--tool-profile", "core"]);
    let tools = m.call("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"recall") && !names.contains(&"find_similar"),
        "{names:?}"
    );
}
