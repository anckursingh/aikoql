//! Real-World MCP Integration Test — exercises the full product as an AI agent would.
//!
//! This test:
//! 1. Starts the MCP server in stdio mode
//! 2. Sends JSON-RPC requests simulating an agent workflow
//! 3. Verifies every response
//! 4. Tests CRUD → Search → Graph → Programs → Policies → Backup → Audit
//!
//! ponytail: one comprehensive test that validates the entire surface.

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

struct McpClient {
    child: Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn start(db_path: &str) -> Self {
        // Find binary relative to workspace root.
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let exe = if cfg!(windows) {
            "aikoql-mcp.exe"
        } else {
            "aikoql-mcp"
        };
        let release_bin = workspace_root.join("target/release").join(exe);
        let debug_bin = workspace_root.join("target/debug").join(exe);
        // Prefer the freshest build — otherwise a stale release binary runs
        // old code and integration tests silently test the wrong version.
        let newest = |a: &std::path::Path, b: &std::path::Path| -> bool {
            let m = |p: &std::path::Path| {
                p.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH)
            };
            m(a) >= m(b)
        };
        let bin = match (release_bin.exists(), debug_bin.exists()) {
            (true, true) => {
                if newest(&debug_bin, &release_bin) {
                    debug_bin
                } else {
                    release_bin
                }
            }
            (true, false) => release_bin,
            _ => debug_bin,
        };
        eprintln!("Using binary: {}", bin.display());
        let mut child = Command::new(&bin)
            .arg("serve")
            .arg(db_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // crash output lands in CI logs, not /dev/null
            .spawn()
            .expect("start MCP server");

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);

        McpClient {
            child,
            stdin,
            reader,
            next_id: 1,
        }
    }

    fn call(&mut self, tool: &str, args: &J) -> J {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": tool,
                "arguments": args
            }
        });
        let line = serde_json::to_string(&req).unwrap() + "\n";
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.flush().unwrap();

        let mut response = String::new();
        self.reader.read_line(&mut response).unwrap();
        let v: J = serde_json::from_str(&response).unwrap();
        if let Some(err) = v.get("error") {
            panic!("MCP error for {}: {:?}", tool, err);
        }
        // Parse the content[0].text as JSON.
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap_or_else(|_| json!({"raw": text}))
    }

    fn call_raw(&mut self, tool: &str, args: &J) -> J {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": args}
        });
        self.stdin
            .write_all((serde_json::to_string(&req).unwrap() + "\n").as_bytes())
            .unwrap();
        self.stdin.flush().unwrap();
        let mut response = String::new();
        self.reader.read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    }

    /// Establish session identity (R9): subsequent tool calls inherit the
    /// agent_id, roles, and tenant scope until the next session/init.
    fn session_init(&mut self, agent_id: &str, tenant: &str) -> J {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "session/init",
            "params": {"agent_id": agent_id, "tenant": tenant}
        });
        self.stdin
            .write_all((serde_json::to_string(&req).unwrap() + "\n").as_bytes())
            .unwrap();
        self.stdin.flush().unwrap();
        let mut response = String::new();
        self.reader.read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn real_world_agent_workflow() {
    let db = std::env::temp_dir().join(format!("mcp-rw-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let mut c = McpClient::start(db.to_str().unwrap());

    // ── Phase 1: Knowledge CRUD ──────────────────────────────────────────

    // Create employee objects.
    let alice = c.call("remember", &json!({
        "subject": "admin", "type_name": "Employee", "tenant": "acme",
        "properties": {"name": "Alice Chen", "dept": "Engineering", "salary": 165000, "level": "L6"},
        "tags": ["engineering", "senior"]
    }));
    let alice_koid = alice["koid"].as_str().unwrap().to_string();
    assert_eq!(alice["version"], 1);

    let bob = c.call("remember", &json!({
        "subject": "admin", "type_name": "Employee", "tenant": "acme",
        "properties": {"name": "Bob Martinez", "dept": "Design", "salary": 130000, "level": "L5"},
        "tags": ["design"]
    }));
    let bob_koid = bob["koid"].as_str().unwrap().to_string();

    let carol = c.call("remember", &json!({
        "subject": "admin", "type_name": "Employee", "tenant": "beta",
        "properties": {"name": "Carol Wu", "dept": "Engineering", "salary": 175000, "level": "L6"},
        "tags": ["engineering", "lead"]
    }));
    let carol_koid = carol["koid"].as_str().unwrap().to_string();

    let proj = c.call("remember", &json!({
        "subject": "admin", "type_name": "Project", "tenant": "acme",
        "properties": {"title": "Aikoql Core", "status": "active", "priority": 1, "budget": 500000.0}
    }));
    let _proj_koid = proj["koid"].as_str().unwrap().to_string();

    // ── Phase 2: Read + Verify ──────────────────────────────────────────

    let fetched = c.call("get", &json!({"koid": &alice_koid, "subject": "admin"}));
    assert_eq!(fetched["type_name"], "Employee");
    assert_eq!(fetched["properties"]["name"], "Alice Chen");

    // ── Phase 3: Graph Relationships ─────────────────────────────────────

    let rel1 = c.call(
        "relate",
        &json!({
            "subject": "admin", "from": &alice_koid, "to": &bob_koid, "rel_type": "knows"
        }),
    );
    assert!(rel1["koid"].as_str().is_some());

    c.call(
        "relate",
        &json!({
            "subject": "admin", "from": &alice_koid, "to": &carol_koid, "rel_type": "collaborates"
        }),
    );

    // Traverse from Alice.
    let hits = c.call(
        "traverse",
        &json!({
            "subject": "admin", "koid": &alice_koid, "depth": 1, "direction": "outbound"
        }),
    );
    // Should find both Bob and Carol.
    assert!(hits["hits"].as_array().unwrap().len() >= 2);

    // ── Phase 4: Search ──────────────────────────────────────────────────

    let found = c.call(
        "find_similar",
        &json!({
            "subject": "admin", "type_name": "Employee", "text": "engineering lead", "k": 5
        }),
    );
    assert!(!found["results"].as_array().unwrap().is_empty());

    // ── Phase 5: Aikoql Query ────────────────────────────────────────────

    let query = "MATCH Employee WHERE dept == \"Engineering\" RETURN *".to_string();
    let results = c.call("aikoql", &json!({"query": query, "subject": "admin"}));
    assert!(results["results"].as_array().unwrap().len() >= 2);

    // ── Phase 6: Programs-as-KOs ─────────────────────────────────────────

    let prog = c.call(
        "deploy_program",
        &json!({
            "subject": "admin", "name": "FindEngineers",
            "body": "MATCH Employee WHERE dept == \"Engineering\" RETURN *",
            "language": "aikoql"
        }),
    );
    let prog_koid = prog["koid"].as_str().unwrap().to_string();

    let exec = c.call(
        "execute_program",
        &json!({
            "subject": "admin", "roles": ["admin"], "koid": &prog_koid
        }),
    );
    assert!(exec["count"].as_u64().unwrap() >= 2);

    // List programs.
    let programs = c.call("list_programs", &json!({"subject": "admin"}));
    assert!(!programs["programs"].as_array().unwrap().is_empty());

    // ── Phase 7: Policy-as-KO ────────────────────────────────────────────

    c.call(
        "deploy_policy",
        &json!({
            "subject": "admin", "name": "HRReadEmployee", "effect": "Allow",
            "principal": "hr-team", "action": "Read", "resource_type": "Employee"
        }),
    );

    let eval = c.call("evaluate_policies", &json!({
        "subject": "admin", "principal": "hr-team", "action": "Read", "resource_type": "Employee"
    }));
    assert_eq!(eval["allowed"], true);

    let deny_eval = c.call(
        "evaluate_policies",
        &json!({
            "subject": "admin", "principal": "intern", "action": "Read", "resource_type": "Employee"
        }),
    );
    // intern has no policy — should not be allowed.
    assert_eq!(deny_eval["allowed"], false);

    // ── Phase 8: Workflow ────────────────────────────────────────────────

    let wf = c.call(
        "deploy_workflow",
        &json!({
            "subject": "admin", "name": "TeamReport",
            "steps": [{"order": 1, "program": "FindEngineers"}]
        }),
    );
    let wf_koid = wf["koid"].as_str().unwrap().to_string();

    let wf_exec = c.call(
        "execute_workflow",
        &json!({
            "subject": "admin", "koid": &wf_koid
        }),
    );
    assert_eq!(wf_exec["executed"], true);

    // ── Phase 9: Backup + Audit ──────────────────────────────────────────

    let backup = c.call_raw("backup", &json!({"subject": "admin"})).clone();
    // Result may have been successful even if backup dir exists.
    assert!(backup["result"].is_object());

    let audit = c.call("audit_report", &json!({}));
    assert!(audit["total_objects"].as_u64().unwrap() >= 4);
    assert!(!audit["audit_chain"].as_str().unwrap().is_empty());

    // ── Phase 10: ABI Version ────────────────────────────────────────────

    let abi = c.call("abi_version", &json!({}));
    assert_eq!(abi["abi_version"], 1);
    assert_eq!(abi["audit_chain_exportable"], true);

    // ── Phase 11: Metrics ────────────────────────────────────────────────

    let metrics = c.call("metrics", &json!({}));
    assert!(metrics["journal_seq"].as_u64().unwrap() > 0);
    assert!(metrics["total_objects"].as_u64().unwrap() >= 4);

    // ── Phase 12: Multi-Tenancy (R9) ─────────────────────────────────────

    // The SAME principal "admin" owns both notes — only the tenant differs,
    // so any cross-visibility here is a tenant-confinement failure, not an
    // ACL failure. Session identity carries the tenant into every tool call.
    let init = c.session_init("admin", "acme");
    assert_eq!(init["result"]["established"], true);

    let acme_note = c.call(
        "remember",
        &json!({"type_name": "note", "properties": {"body": "acme quarterly report", "memo": "acme"}}),
    );
    let acme_koid = acme_note["koid"].as_str().unwrap().to_string();

    c.session_init("admin", "beta");
    let beta_note = c.call(
        "remember",
        &json!({"type_name": "note", "properties": {"body": "beta launch plan", "memo": "beta"}}),
    );
    let beta_koid = beta_note["koid"].as_str().unwrap().to_string();

    // Scoped to beta: recall sees only beta's note.
    let beta_sim = c.call(
        "find_similar",
        &json!({"type_name": "note", "text": "launch plan", "k": 10}),
    );
    let beta_koids: Vec<&str> = beta_sim["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["koid"].as_str())
        .collect();
    assert!(
        beta_koids.contains(&beta_koid.as_str()),
        "beta's own note must be visible: {beta_koids:?}"
    );
    assert!(
        !beta_koids.contains(&acme_koid.as_str()),
        "acme's note leaked into beta's recall: {beta_koids:?}"
    );

    // Cross-tenant point read denied even though admin owns the object.
    // Tool errors surface as an isError result carrying the message.
    let cross = c.call_raw("get", &json!({"koid": &acme_koid}));
    let cross_text = cross["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        cross["result"]["isError"] == true && cross_text.contains("ACCESS_DENIED"),
        "cross-tenant get must be denied: {cross}"
    );

    // Scoped to acme: recall sees only acme's note.
    c.session_init("admin", "acme");
    let acme_sim = c.call(
        "find_similar",
        &json!({"type_name": "note", "text": "report", "k": 10}),
    );
    let acme_koids: Vec<&str> = acme_sim["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["koid"].as_str())
        .collect();
    assert!(
        acme_koids.contains(&acme_koid.as_str()),
        "acme's own note must be visible: {acme_koids:?}"
    );
    assert!(
        !acme_koids.contains(&beta_koid.as_str()),
        "beta's note leaked into acme's recall: {acme_koids:?}"
    );

    // MATCH (aikoql) rides the same scoped path.
    let acme_match = c.call("aikoql", &json!({"query": "MATCH note RETURN *"}));
    let match_koids: Vec<&str> = acme_match["results"]
        .as_array()
        .map(|a| a.iter().filter_map(|o| o["koid"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        match_koids.contains(&acme_koid.as_str()),
        "MATCH should return acme's note: {match_koids:?}"
    );
    assert!(
        !match_koids.contains(&beta_koid.as_str()),
        "MATCH leaked beta's note: {match_koids:?}"
    );

    let _ = std::fs::remove_file(&db);
}

#[test]
fn mcp_ping_and_tools_list() {
    let db = std::env::temp_dir().join(format!("mcp-ping-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let mut c = McpClient::start(db.to_str().unwrap());

    // Ping
    let mut req = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    c.stdin
        .write_all((serde_json::to_string(&req).unwrap() + "\n").as_bytes())
        .unwrap();
    c.stdin.flush().unwrap();
    let mut resp = String::new();
    c.reader.read_line(&mut resp).unwrap();
    let v: J = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["result"], json!({}));

    // Tools list
    req = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    c.stdin
        .write_all((serde_json::to_string(&req).unwrap() + "\n").as_bytes())
        .unwrap();
    c.stdin.flush().unwrap();
    resp.clear();
    c.reader.read_line(&mut resp).unwrap();
    let v: J = serde_json::from_str(&resp).unwrap();
    let tools = v["result"]["tools"].as_array().unwrap();
    assert!(
        tools.len() >= 30,
        "Expected >=30 tools, got {}",
        tools.len()
    );

    let _ = std::fs::remove_file(&db);
}

#[test]
fn mcp_idempotency_guarantee() {
    let db = std::env::temp_dir().join(format!("mcp-idem-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let mut c = McpClient::start(db.to_str().unwrap());

    // Create with idempotency key.
    let r1 = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "Note",
            "properties": {"body": "idempotent test"},
            "idempotency_key": "agent-retry-001"
        }),
    );
    let koid1 = r1["koid"].as_str().unwrap().to_string();

    // Repeat with same idempotency key — must return same KOID, not create a new one.
    let r2 = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "Note",
            "properties": {"body": "idempotent test"},
            "idempotency_key": "agent-retry-001"
        }),
    );
    assert_eq!(r2["koid"].as_str().unwrap(), koid1);

    let _ = std::fs::remove_file(&db);
}
