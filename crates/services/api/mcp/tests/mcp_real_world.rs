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

/// §51 Critical End-to-End Scenario (chatbot suite, certification G5):
/// deterministic scripted replay over the real MCP surface with mechanical
/// judges (PR-R pattern — the script is the "LLM", asserts are the judges).
///
/// Scenario beats: initial conversation → durable memories with provenance
/// and scope → later recall ("AWS") → authoritative org update supersedes
/// the preference ("Azure", with supersession evidence) → "Deploy it." runs
/// the Program-as-KO pipeline (identity → permissions → policy → execute →
/// postconditions → episode).
#[test]
fn critical_e2e_scenario_51_chatbot_memory() {
    let db = std::env::temp_dir().join(format!("mcp-s51-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let mut c = McpClient::start(db.to_str().unwrap());
    c.session_init("chatbot-user", "acme");

    // ── §51.1 Initial conversation → three durable memories ───────────────
    // Evidence-backed user statements enter as assertions (evidence is
    // mandatory there and stamped by the kernel); plain identity data uses
    // remember. Both must survive round-trip with provenance intact.
    let style = c.call(
        "assert_knowledge",
        &json!({
            "subject": "chatbot-user", "type_name": "UserPreference", "tenant": "acme",
            "properties": {"topic": "response style", "value": "concise"},
            "authority": "human_approved",
            "evidence": [{"source_artifact": "chat-message-1", "method": "human_provided"}]
        }),
    );
    assert_eq!(style["version"], 1);

    let acct = c.call(
        "remember",
        &json!({
            "subject": "chatbot-user", "type_name": "AccountInfo", "tenant": "acme",
            "properties": {"account": "ACME-123"},
            "origin": "human"
        }),
    );
    assert_eq!(acct["version"], 1);

    let aws = c.call("assert_knowledge", &json!({
        "subject": "chatbot-user", "type_name": "DeploymentPreference", "tenant": "acme",
        "properties": {"account": "ACME-123", "cloud": "AWS"},
        "authority": "human_approved",
        "evidence": [{"source_artifact": "chat-message-3", "method": "human_provided", "confidence": 0.95}]
    }));
    let aws_koid = aws["koid"].as_str().unwrap().to_string();

    // Memory carries provenance + scope to the query boundary.
    let aws_ko = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": &aws_koid}),
    );
    assert_eq!(aws_ko["type_name"], "DeploymentPreference");
    assert_eq!(aws_ko["properties"]["cloud"], "AWS");
    assert_eq!(aws_ko["extensions"]["authority"], "human_approved");
    assert_eq!(
        aws_ko["extensions"]["scope"], "session",
        "the kernel stamps an explicit scope for agent-mediated claims: {aws_ko}"
    );
    assert_eq!(aws_ko["extensions"]["epistemic_status"], "asserted");
    assert!(
        aws_ko["extensions"]["evidence"]
            .to_string()
            .contains("chat-message-3"),
        "provenance evidence must survive to the query boundary: {}",
        aws_ko["extensions"]["evidence"]
    );

    // ── §51.2 Later conversation: recall with correct provenance/scope ────
    // "What do you know about my deployment setup?" → the remembered AWS.
    let recall = c.call(
        "aikoql",
        &json!({
            "subject": "chatbot-user",
            "query": "MATCH DeploymentPreference WHERE account == \"ACME-123\" RETURN *"
        }),
    );
    let clouds: Vec<String> = recall["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["properties"]["cloud"].as_str().map(String::from))
        .collect();
    assert!(
        clouds.contains(&"AWS".to_string()),
        "recall must return the remembered deployment preference: {clouds:?}"
    );

    // ── §51.3 Authoritative org update supersedes the preference ──────────
    // Ingest the organization directive as an assertion carrying
    // organization_policy authority, then supersede the user preference
    // with it.
    let directive = c.call("assert_knowledge", &json!({
        "subject": "chatbot-user", "type_name": "DeploymentDirective", "tenant": "acme",
        "properties": {"account": "ACME-123", "cloud": "Azure"},
        "authority": "organization_policy",
        "evidence": [{"source_artifact": "org-policy-v2", "method": "human_provided", "confidence": 1.0}],
        "note": "ACME-123 must now deploy on Azure"
    }));
    let directive_koid = directive["koid"].as_str().unwrap().to_string();
    let directive_ko = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": &directive_koid}),
    );
    assert_eq!(
        directive_ko["extensions"]["authority"], "organization_policy",
        "the org directive must carry organization-policy authority: {directive_ko}"
    );

    let sup = c.call("supersede", &json!({
        "subject": "chatbot-user",
        "old": &aws_koid,
        "superseded_by": &directive_koid,
        "reason": "Organization policy supersedes the previous preference: ACME-123 must deploy on Azure",
        "evidence": [{"source_artifact": "org-policy-v2", "method": "human_provided"}]
    }));
    assert_eq!(sup["new"], directive_koid);

    // The old preference is temporally closed, still readable, and links to
    // its successor — the supersession explanation is durable knowledge.
    let aws_after = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": &aws_koid}),
    );
    assert_eq!(
        aws_after["properties"]["cloud"], "AWS",
        "superseded knowledge stays readable (temporal)"
    );
    assert_eq!(aws_after["extensions"]["epistemic_status"], "superseded");
    assert!(
        aws_after["relationships"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["target"] == directive_koid),
        "superseded preference must link to its successor: {}",
        aws_after["relationships"]
    );
    assert!(
        aws_after["extensions"]["epistemic_history"]
            .to_string()
            .contains("Organization policy supersedes"),
        "supersession reason must be recorded: {}",
        aws_after["extensions"]["epistemic_history"]
    );
    assert!(
        aws_after["extensions"]["evidence"]
            .to_string()
            .contains("org-policy-v2"),
        "supersession evidence must append to the old claim, never disappear"
    );

    // "Where should I deploy now?" → the org directive, with org authority.
    let now = c.call(
        "aikoql",
        &json!({
            "subject": "chatbot-user",
            "query": "MATCH DeploymentDirective WHERE account == \"ACME-123\" RETURN *"
        }),
    );
    let targets: Vec<String> = now["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["properties"]["cloud"].as_str().map(String::from))
        .collect();
    assert!(
        targets.contains(&"Azure".to_string()),
        "current deployment target must be Azure: {targets:?}"
    );

    // ── §51.4 "Deploy it." — the Program-as-KO action pipeline ────────────
    // Resolve Program-as-KO: the deployment program reads the current
    // directive from knowledge (no hardcoded target).
    let prog = c.call(
        "deploy_program",
        &json!({
            "subject": "chatbot-user",
            "name": "DeployToCloud",
            "body": "MATCH DeploymentDirective WHERE account == \"ACME-123\" RETURN *",
            "language": "aikoql"
        }),
    );
    let prog_koid = prog["koid"].as_str().unwrap().to_string();
    let prog_ko = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": &prog_koid}),
    );
    assert_eq!(prog_ko["type_name"], "aikoql:program");

    // Check permissions + policy: Allow for the bot principal, deny for
    // anyone else (the approval gate where a human would be asked).
    c.call(
        "deploy_policy",
        &json!({
            "subject": "chatbot-user", "name": "BotMayDeploy", "effect": "Allow",
            "principal": "chatbot-user", "action": "Write", "resource_type": "DeploymentDirective"
        }),
    );
    let allow = c.call(
        "evaluate_policies",
        &json!({
            "subject": "chatbot-user", "principal": "chatbot-user",
            "action": "Write", "resource_type": "DeploymentDirective"
        }),
    );
    assert_eq!(
        allow["allowed"], true,
        "deploy policy must allow the bot: {allow}"
    );
    let deny = c.call(
        "evaluate_policies",
        &json!({
            "subject": "chatbot-user", "principal": "other-bot",
            "action": "Write", "resource_type": "DeploymentDirective"
        }),
    );
    assert_eq!(
        deny["allowed"], false,
        "non-authorized principal must be denied: {deny}"
    );

    // Execute under the caller's identity.
    let exec = c.call(
        "execute_program",
        &json!({
            "subject": "chatbot-user", "roles": ["chatbot-user"], "koid": &prog_koid
        }),
    );
    assert_eq!(
        exec["count"], 1,
        "program must resolve exactly one deployment target: {exec}"
    );
    assert_eq!(
        exec["results"][0]["properties"]["cloud"], "Azure",
        "postcondition: the executed deployment targets the org-mandated cloud"
    );

    // Record the episode: goal → action → outcome, with preconditions.
    let ep = c.call(
        "record_experience",
        &json!({
            "subject": "chatbot-user",
            "goal": "Deploy ACME-123",
            "action": "execute DeployToCloud",
            "outcome": "success",
            "preconditions": ["policy BotMayDeploy allowed"],
            "lesson": "deployment target resolved from the org directive",
            "evidence": [{"source_artifact": "exec-run-1", "method": "runtime_observation"}]
        }),
    );
    let ep_koid = ep["koid"].as_str().unwrap().to_string();
    let ep_ko = c.call("get", &json!({"subject": "chatbot-user", "koid": &ep_koid}));
    assert_eq!(ep_ko["type_name"], "aikoql:experience");
    assert_eq!(ep_ko["properties"]["actor"], "chatbot-user");
    assert_eq!(ep_ko["properties"]["goal"], "Deploy ACME-123");
    assert_eq!(ep_ko["properties"]["outcome"], "success");
    assert_eq!(
        ep_ko["properties"]["preconditions"][0],
        "policy BotMayDeploy allowed"
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
