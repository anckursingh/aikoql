//! MCP stdio integration suite — drives the real server binary over pipes.
//!
//! Includes the flagship acceptance scenario (VISION-AND-STRATEGY Phase 1):
//! an agent commits a claim through MCP, then asks "why do you believe this?"
//! and receives source + confidence + verification + evidence in one call.

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    pending: Vec<J>,
}

impl McpClient {
    fn start(db: &PathBuf) -> Self {
        let mut exe = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        exe.push("../../../../target/debug/mnemosyne-mcp");
        #[cfg(windows)]
        exe.set_extension("exe");
        assert!(
            exe.exists(),
            "mnemosyne-mcp not built at {:?}; run cargo build first",
            exe
        );
        let mut child = Command::new(&exe)
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mnemosyne-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        McpClient {
            child,
            stdin,
            stdout,
            next_id: 0,
            pending: Vec::new(),
        }
    }

    fn request(&mut self, method: &str, params: J) -> J {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{}", frame).unwrap();
        self.stdin.flush().unwrap();
        loop {
            if let Some(pos) = self
                .pending
                .iter()
                .position(|f| f.get("id").and_then(|i| i.as_u64()) == Some(id))
            {
                let resp = self.pending.remove(pos);
                if let Some(err) = resp.get("error") {
                    panic!("json-rpc error: {}", err);
                }
                return resp.get("result").cloned().unwrap_or(J::Null);
            }
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            let frame: J = serde_json::from_str(line.trim()).expect("valid json-rpc frame");
            if frame.get("id").and_then(|i| i.as_u64()) == Some(id) {
                if let Some(err) = frame.get("error") {
                    panic!("json-rpc error: {}", err);
                }
                return frame.get("result").cloned().unwrap_or(J::Null);
            }
            self.pending.push(frame);
        }
    }

    fn take_notifications(&mut self) -> Vec<J> {
        let mut out = Vec::new();
        self.pending.retain(|f| {
            if f.get("method").and_then(|m| m.as_str()) == Some("notifications/notify") {
                out.push(f.clone());
                false
            } else {
                true
            }
        });
        out
    }

    /// Wait for at least `n` notifications and return them. Issues ping
    /// requests to drain stdout. Times out after ~2s.
    fn wait_for_notifications(&mut self, n: usize) -> Vec<J> {
        for _ in 0..200 {
            let notes = self.take_notifications();
            if notes.len() >= n {
                return notes;
            }
            let _ = self.request("ping", json!({}));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Vec::new()
    }

    fn notify(&mut self, method: &str) {
        let frame = json!({"jsonrpc": "2.0", "method": method});
        writeln!(self.stdin, "{}", frame).unwrap();
        self.stdin.flush().unwrap();
    }

    fn call_tool(&mut self, name: &str, args: J) -> J {
        let res = self.request("tools/call", json!({"name": name, "arguments": args}));
        assert_eq!(
            res.get("isError").and_then(|b| b.as_bool()),
            Some(false),
            "tool error: {}",
            res
        );
        let text = res["content"][0]["text"].as_str().unwrap().to_string();
        serde_json::from_str(&text).expect("tool payload is json")
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tmp_db(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "mnemosyne_mcp_{}_{}.redb",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn m01_initialize_and_tools_list() {
    let db = tmp_db("handshake");
    let mut c = McpClient::start(&db);
    let init = c.request(
        "initialize",
        json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert_eq!(init["protocolVersion"], "2024-11-05");
    assert_eq!(init["serverInfo"]["name"], "mnemosyne-mcp");
    c.notify("notifications/initialized");

    let list = c.request("tools/list", json!({}));
    let names: Vec<&str> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "remember",
        "forget",
        "evolve",
        "verify",
        "get",
        "find_similar",
        "trace",
        "explain",
        "prove",
        "relate",
        "traverse",
        "eval_recall",
        "eval_staleness",
        "eval_contradictions",
        "aikoql",
        "backup",
        "restore",
        "list_backups",
        "metrics",
    ] {
        assert!(names.contains(&expected), "missing tool: {}", expected);
    }
}

#[test]
fn m02_flagship_why_did_the_agent_know_this() {
    let db = tmp_db("flagship");
    let mut c = McpClient::start(&db);
    c.request("initialize", json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "flagship", "version": "0"}}));
    c.notify("notifications/initialized");

    // 1. an agent commits a claim with provenance
    let evidence = c.call_tool(
        "remember",
        json!({
            "subject": "agent-researcher",
            "type_name": "evidence",
            "properties": {"title": "SEC 10-K FY2025"},
            "origin": "agent-researcher"
        }),
    );
    let claim = c.call_tool(
        "remember",
        json!({
            "subject": "agent-researcher",
            "type_name": "claim",
            "properties": {"revenue": "$4.2B", "period": "FY2025"},
            "semantic": {"source": "sec-10k-filing", "confidence": 0.99, "embedding_model": "bge-m3", "embedding": [0.5, 0.5]},
            "origin": "agent-researcher",
            "note": "extracted from filing"
        }),
    );
    let claim_koid = claim["koid"].as_str().unwrap().to_string();
    assert_eq!(claim["version"], 1);

    // 2. claim is verified through its lifecycle
    c.call_tool(
        "evolve",
        json!({"subject": "agent-researcher", "koid": claim_koid, "to": "active"}),
    );
    let v = c.call_tool(
        "evolve",
        json!({"subject": "agent-researcher", "koid": claim_koid, "to": "verified"}),
    );
    assert_eq!(v["state"], "verified");

    // 3. THE FLAGSHIP QUESTION: why do you believe this?
    let ex = c.call_tool(
        "explain",
        json!({"subject": "agent-researcher", "koid": claim_koid}),
    );
    assert_eq!(ex["source"], "sec-10k-filing");
    // confidence is stored f32; compare through JSON with tolerance
    let conf = ex["confidence"].as_f64().expect("confidence present");
    assert!((conf - 0.99).abs() < 1e-6, "confidence {} != 0.99", conf);
    assert_eq!(ex["verified"], true);
    assert!(
        ex["event_refs"].as_array().unwrap().len() >= 3,
        "must carry commit lineage"
    );

    // 4. the audit chain verifies
    let proof = c.call_tool(
        "prove",
        json!({"subject": "agent-researcher", "koid": claim_koid}),
    );
    assert_eq!(proof["chain_valid"], true);
    assert!(proof["events"].as_u64().unwrap() >= 4);

    // 5. recall finds it; lineage is complete
    let recall = c.call_tool(
        "find_similar",
        json!({"subject": "agent-researcher", "text": "revenue", "vector": [0.5, 0.5], "k": 5}),
    );
    let found: Vec<&str> = recall["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["koid"].as_str().unwrap())
        .collect();
    assert!(
        found.contains(&claim_koid.as_str()),
        "recall must find the claim"
    );

    let tr = c.call_tool(
        "trace",
        json!({"subject": "agent-researcher", "koid": claim_koid}),
    );
    assert_eq!(tr["versions"].as_array().unwrap().len(), 3);
    assert_eq!(tr["events"].as_array().unwrap().len(), 3);

    // 6. and it survives a server restart (durability through MCP)
    drop(c);
    let mut c2 = McpClient::start(&db);
    c2.request("initialize", json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "flagship", "version": "0"}}));
    let got = c2.call_tool(
        "get",
        json!({"subject": "agent-researcher", "koid": claim_koid}),
    );
    assert_eq!(got["state"], "verified");
    assert_eq!(got["semantic"]["source"], "sec-10k-filing");
    let proof2 = c2.call_tool(
        "prove",
        json!({"subject": "agent-researcher", "koid": claim_koid}),
    );
    assert_eq!(proof2["chain_valid"], true);
    assert_eq!(evidence["version"], 1);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn m03_acl_enforced_through_mcp() {
    let db = tmp_db("acl");
    let mut c = McpClient::start(&db);
    c.request("initialize", json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}));
    let r = c.call_tool(
        "remember",
        json!({"subject": "alice", "type_name": "secret"}),
    );
    let koid = r["koid"].as_str().unwrap();

    // bob may not read alice's object
    let res = c.request(
        "tools/call",
        json!({"name": "get", "arguments": {"subject": "bob", "koid": koid}}),
    );
    assert_eq!(res["isError"], true);
    let msg = res["content"][0]["text"].as_str().unwrap();
    assert!(
        msg.contains("ACCESS_DENIED"),
        "expected ACCESS_DENIED, got: {}",
        msg
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn m04_durable_notification_and_replay() {
    let db = tmp_db("cdc");
    let mut c = McpClient::start(&db);
    c.request(
        "initialize",
        json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "cdc", "version": "0"}}),
    );
    c.notify("notifications/initialized");

    let sub = c.request("notifications/subscribe", json!({"id": "s1"}));
    assert_eq!(sub["subscribed"], true);
    assert_eq!(sub["replayed"], 0);
    assert!(c.take_notifications().is_empty());

    let r = c.call_tool(
        "remember",
        json!({"subject": "alice", "type_name": "fact", "properties": {"x": 1}}),
    );
    let koid1 = r["koid"].as_str().unwrap().to_string();
    let notes = c.wait_for_notifications(1);
    assert!(!notes.is_empty(), "expected at least one live notification");
    let ev1 = &notes[0]["params"]["event"];
    assert_eq!(ev1["koid"], koid1);
    assert_eq!(ev1["kind"], "Created");
    let seq1 = ev1["seq"].as_u64().unwrap();

    c.request("notifications/ack", json!({"id": "s1", "seq": seq1}));

    let r2 = c.call_tool(
        "remember",
        json!({"subject": "alice", "type_name": "fact", "properties": {"x": 2}}),
    );
    let koid2 = r2["koid"].as_str().unwrap().to_string();
    let notes = c.wait_for_notifications(1);
    assert!(!notes.is_empty());
    let ev2 = &notes[0]["params"]["event"];
    assert_eq!(ev2["koid"], koid2);
    let seq2 = ev2["seq"].as_u64().unwrap();
    assert!(seq2 > seq1);

    // reconnect without acking seq2: the persisted subscription replays it
    drop(c);
    let mut c2 = McpClient::start(&db);
    c2.request(
        "initialize",
        json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "cdc", "version": "0"}}),
    );
    let sub2 = c2.request("notifications/subscribe", json!({"id": "s1"}));
    assert_eq!(sub2["subscribed"], true);
    assert_eq!(sub2["replayed"], 1);
    let notes = c2.take_notifications();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["params"]["event"]["seq"], seq2);

    let _ = std::fs::remove_file(&db);
}

#[test]
fn m05_cross_agent_acl_policy_and_role_inheritance() {
    let db = tmp_db("xacl");
    let mut c = McpClient::start(&db);
    c.request("initialize", json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "xacl", "version": "0"}}));
    c.notify("notifications/initialized");

    // admin bootstraps role hierarchy and a cross-agent read policy
    c.call_tool(
        "remember",
        json!({
            "subject": "admin",
            "roles": ["admin"],
            "type_name": "mnemosyne:role",
            "properties": {"name": "senior", "parents": []}
        }),
    );
    c.call_tool(
        "remember",
        json!({
            "subject": "admin",
            "roles": ["admin"],
            "type_name": "mnemosyne:role",
            "properties": {"name": "junior", "parents": ["senior"]}
        }),
    );
    c.call_tool(
        "remember",
        json!({
            "subject": "admin",
            "roles": ["admin"],
            "type_name": "mnemosyne:policy",
            "properties": {
                "target_type": "shared_note",
                "rules": [{"principal": "senior", "action": "read", "effect": "allow"}]
            }
        }),
    );

    // alice (junior) writes a shared note; bob (junior, inheriting senior) reads it
    let note = c.call_tool(
        "remember",
        json!({
            "subject": "alice",
            "roles": ["junior"],
            "type_name": "shared_note",
            "properties": {"body": "hello team"}
        }),
    );
    let koid = note["koid"].as_str().unwrap();

    let got = c.call_tool(
        "get",
        json!({"subject": "bob", "roles": ["junior"], "koid": koid}),
    );
    assert_eq!(got["type_name"], "shared_note");
    assert_eq!(got["properties"]["body"], "hello team");

    // carol has no role and is not the owner, so she is denied
    let res = c.request(
        "tools/call",
        json!({"name": "get", "arguments": {"subject": "carol", "koid": koid}}),
    );
    assert_eq!(res["isError"], true);
    let msg = res["content"][0]["text"].as_str().unwrap();
    assert!(
        msg.contains("ACCESS_DENIED"),
        "expected ACCESS_DENIED, got: {}",
        msg
    );

    let _ = std::fs::remove_file(&db);
}

#[test]
fn m06_memory_evals_over_mcp() {
    let db = tmp_db("evals");
    let mut c = McpClient::start(&db);
    c.request("initialize", json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "evals", "version": "0"}}));
    c.notify("notifications/initialized");

    let a = c.call_tool(
        "remember",
        json!({"subject": "eval", "type_name": "fact", "properties": {"body": "alpha"}, "semantic": {"embedding": [1.0, 0.0]}}),
    );
    let _b = c.call_tool(
        "remember",
        json!({"subject": "eval", "type_name": "fact", "properties": {"body": "beta"}, "semantic": {"embedding": [0.0, 1.0]}}),
    );
    let c_oid = c.call_tool(
        "remember",
        json!({"subject": "eval", "type_name": "fact", "properties": {"body": "alpha2"}, "semantic": {"embedding": [0.99, 0.01]}}),
    );

    let recall = c.call_tool(
        "eval_recall",
        json!({"subject": "eval", "type_name": "fact", "text": "alpha", "k": 5, "fusion": "text", "expected": [a["koid"].as_str().unwrap(), c_oid["koid"].as_str().unwrap()]}),
    );
    assert_eq!(recall["hits"].as_u64().unwrap(), 2);
    assert!((recall["recall"].as_f64().unwrap() - 1.0).abs() < 1e-6);

    let staleness = c.call_tool(
        "eval_staleness",
        json!({"subject": "eval", "type_name": "fact", "text": "alpha", "k": 5, "fusion": "text"}),
    );
    assert!(
        staleness["max_lag_ms"].as_u64().unwrap() >= staleness["mean_lag_ms"].as_u64().unwrap()
    );

    let yes = c.call_tool(
        "remember",
        json!({"subject": "eval", "type_name": "claim", "properties": {"claim": "AGI is possible", "answer": true}, "semantic": {"embedding": [1.0, 0.0]}}),
    );
    let no = c.call_tool(
        "remember",
        json!({"subject": "eval", "type_name": "claim", "properties": {"claim": "AGI is impossible", "answer": false}, "semantic": {"embedding": [0.99, 0.01]}}),
    );
    let contradictions = c.call_tool(
        "eval_contradictions",
        json!({"subject": "eval", "type_name": "claim", "property": "answer", "threshold": 0.9, "max_results": 10}),
    );
    let hits = contradictions["contradictions"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    let pair = &hits[0];
    let left = pair["left"].as_str().unwrap();
    let right = pair["right"].as_str().unwrap();
    assert!(
        (left == yes["koid"].as_str().unwrap() && right == no["koid"].as_str().unwrap())
            || (left == no["koid"].as_str().unwrap() && right == yes["koid"].as_str().unwrap())
    );

    let _ = std::fs::remove_file(&db);
}

#[test]
fn m07_graph_relate_and_traverse_over_mcp() {
    let db = tmp_db("graph");
    let mut c = McpClient::start(&db);
    c.request(
        "initialize",
        json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "graph", "version": "0"}}),
    );
    c.notify("notifications/initialized");

    let a = c.call_tool(
        "remember",
        json!({"subject": "alice", "type_name": "note", "properties": {"body": "A"}}),
    );
    let b = c.call_tool(
        "remember",
        json!({"subject": "alice", "type_name": "note", "properties": {"body": "B"}}),
    );
    let c_oid = c.call_tool(
        "remember",
        json!({"subject": "alice", "type_name": "note", "properties": {"body": "C"}}),
    );

    let a_koid = a["koid"].as_str().unwrap();
    let b_koid = b["koid"].as_str().unwrap();
    let c_koid = c_oid["koid"].as_str().unwrap();

    let rel = c.call_tool(
        "relate",
        json!({"subject": "alice", "from": a_koid, "to": b_koid, "rel_type": "references"}),
    );
    assert_eq!(rel["koid"], a_koid);
    assert_eq!(rel["version"], 2);

    let rel_c = c.call_tool(
        "relate",
        json!({"subject": "alice", "from": a_koid, "to": c_koid, "rel_type": "cites"}),
    );

    // idempotent: second identical relate returns the current head version without a new edge
    let rel2 = c.call_tool(
        "relate",
        json!({"subject": "alice", "from": a_koid, "to": b_koid, "rel_type": "references"}),
    );
    assert_eq!(rel2["version"], rel_c["version"]);

    let all = c.call_tool(
        "traverse",
        json!({"subject": "alice", "koid": a_koid, "depth": 1}),
    );
    let hits = all["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);

    let filtered = c.call_tool(
        "traverse",
        json!({"subject": "alice", "koid": a_koid, "depth": 1, "rel_type": "references"}),
    );
    let fhits = filtered["hits"].as_array().unwrap();
    assert_eq!(fhits.len(), 1);
    assert_eq!(fhits[0]["koid"], b_koid);
    assert_eq!(fhits[0]["direction"], "outbound");

    let _ = std::fs::remove_file(&db);
}

#[test]
fn m08_aikoql_query_over_mcp() {
    let db = tmp_db("aikoql");
    let mut c = McpClient::start(&db);
    c.request("initialize", json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "aikoql", "version": "0"}}));
    c.notify("notifications/initialized");

    c.call_tool("remember", json!({"subject": "alice", "type_name": "Person", "properties": {"name": "Alice", "city": "Amsterdam"}}));
    c.call_tool("remember", json!({"subject": "alice", "type_name": "Person", "properties": {"name": "Bob", "city": "London"}}));

    let all = c.call_tool("aikoql", json!({"subject": "alice", "query": "MATCH Person RETURN *"}));
    assert_eq!(all["results"].as_array().unwrap().len(), 2);

    let filtered = c.call_tool("aikoql", json!({"subject": "alice", "query": "MATCH Person WHERE name == \"Alice\" RETURN *"}));
    assert_eq!(filtered["results"].as_array().unwrap().len(), 1);

    let _ = std::fs::remove_file(&db);
}
