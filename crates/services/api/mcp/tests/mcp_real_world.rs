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

use aikoql_ingestion::{EntityCandidate, Evidence, FactCandidate, KnowledgeIr, RelationCandidate};

// Temp db paths written by THIS test thread, swept when the thread exits
// (the main thread's destructor runs at process exit — statics are NOT
// dropped on Windows MSVC, TLS is).
thread_local! {
    static TEMP_PATHS: std::cell::RefCell<TempSweeper> =
        const { std::cell::RefCell::new(TempSweeper { paths: Vec::new() }) };
}

struct TempSweeper {
    paths: Vec<std::path::PathBuf>,
}
impl Drop for TempSweeper {
    fn drop(&mut self) {
        for p in &self.paths {
            // v2 databases are directories (launch S-02).
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

fn tmp_db(suffix: &str) -> String {
    let p = std::env::temp_dir().join(format!("mcp-{suffix}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    TEMP_PATHS.with(|t| t.borrow_mut().paths.push(p.clone()));
    p.to_string_lossy().into_owned()
}

/// The local model store for F13's end-to-end pin: `AIKOQL_TEST_MODEL_DIR`
/// wins, else the platform default (~/.aikoql/models). Returns the models
/// ROOT (the serve joins the model slug itself) when all-MiniLM-L6-v2 is
/// installed there; `None` when the pin must skip.
fn installed_models_root() -> Option<String> {
    let root = if let Ok(dir) = std::env::var("AIKOQL_TEST_MODEL_DIR") {
        std::path::PathBuf::from(dir)
    } else {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from)?;
        home.join(".aikoql").join("models")
    };
    if root
        .join(aikoql_semantic::provider::model_slug(
            aikoql_semantic::provider::DEFAULT_MODEL_ID,
        ))
        .is_dir()
    {
        Some(root.to_string_lossy().into_owned())
    } else {
        None
    }
}

struct McpClient {
    child: Child,
    stdin: std::process::ChildStdin,
    // Option so `call_bounded` can move the reader onto its deadline thread.
    reader: Option<BufReader<std::process::ChildStdout>>,
    next_id: u64,
}

impl McpClient {
    fn start(db_path: &str) -> Self {
        // Pin the harness to the no-provider mode its assertions assume
        // (CTX-001 pins semantic:false): an installed local model would
        // start background enrichment, and its version bumps race
        // CTX-003's pinned update. An empty model dir is deterministically
        // unavailable on every machine.
        let model_dir = tmp_db("ctx-model");
        std::fs::create_dir_all(&model_dir).expect("create empty model dir");
        Self::start_inner(db_path, Some(&model_dir), &[])
    }

    /// Serve against a real model store so background enrichment runs
    /// (F13 pin: the restart's catch-up must enrich, not destroy).
    fn start_with_model_dir(db_path: &str, model_dir: &str) -> Self {
        Self::start_inner(db_path, Some(model_dir), &[])
    }

    /// Same, with extra env for the child (T-44 park pins).
    fn start_with_model_dir_env(db_path: &str, model_dir: &str, envs: &[(&str, &str)]) -> Self {
        Self::start_inner(db_path, Some(model_dir), envs)
    }

    fn start_inner(db_path: &str, model_dir: Option<&str>, envs: &[(&str, &str)]) -> Self {
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
        let mut cmd = Command::new(&bin);
        cmd.arg("serve").arg(db_path);
        if let Some(dir) = model_dir {
            cmd.arg("--model-dir").arg(dir);
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()); // crash output lands in CI logs, not /dev/null
        let mut child = cmd.spawn().expect("start MCP server");

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);

        McpClient {
            child,
            stdin,
            reader: Some(reader),
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
        self.reader
            .as_mut()
            .unwrap()
            .read_line(&mut response)
            .unwrap();
        let v: J = serde_json::from_str(&response).unwrap_or_else(|e| {
            panic!(
                "MCP parse failure for {tool}: {e:?} — response={response:?}, child_status={:?}",
                self.child.try_wait()
            )
        });
        if let Some(err) = v.get("error") {
            panic!("MCP error for {}: {:?}", tool, err);
        }
        // Parse the content[0].text as JSON.
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap_or_else(|_| json!({"raw": text}))
    }

    /// `call` with a wall-clock bound on the RESPONSE — the normal `call`
    /// reads one line unbounded, which would hang a pin whose server parks.
    /// On timeout the test aborts (the child is killed by Drop).
    fn call_bounded(&mut self, tool: &str, args: &J, timeout: std::time::Duration) -> J {
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
        let mut reader = self
            .reader
            .take()
            .expect("call_bounded: reader already taken");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut response = String::new();
            let r = reader.read_line(&mut response);
            let _ = tx.send((r, response, reader));
        });
        let (r, response, reader_back) = rx.recv_timeout(timeout).unwrap_or_else(|_| {
            panic!(
                "{tool} did not respond within {timeout:?} — the request queued behind \
                 the enrichment worker instead of failing fast"
            )
        });
        self.reader = Some(reader_back);
        r.unwrap();
        let v: J = serde_json::from_str(&response).unwrap_or_else(|e| {
            panic!(
                "MCP parse failure for {tool}: {e:?} — response={response:?}, child_status={:?}",
                self.child.try_wait()
            )
        });
        if let Some(err) = v.get("error") {
            panic!("MCP error for {}: {:?}", tool, err);
        }
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
        self.reader
            .as_mut()
            .unwrap()
            .read_line(&mut response)
            .unwrap();
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
        self.reader
            .as_mut()
            .unwrap()
            .read_line(&mut response)
            .unwrap();
        serde_json::from_str(&response).unwrap()
    }

    /// Session with NO tenant pin — the P3-009 fail-open shape.
    fn session_init_unscoped(&mut self, agent_id: &str) -> J {
        self.session_init_params(&json!({"agent_id": agent_id}))
    }

    /// Unscoped session with an explicit admin role — the global read channel.
    fn session_init_unscoped_admin(&mut self, agent_id: &str) -> J {
        self.session_init_params(&json!({"agent_id": agent_id, "roles": ["admin"]}))
    }

    /// Raw session/init passthrough (for boundary-rejection pins).
    fn session_init_with(&mut self, agent_id: &str, tenant: &str) -> J {
        self.session_init_params(&json!({"agent_id": agent_id, "tenant": tenant}))
    }

    fn session_init_params(&mut self, params: &J) -> J {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "session/init",
            "params": params
        });
        self.stdin
            .write_all((serde_json::to_string(&req).unwrap() + "\n").as_bytes())
            .unwrap();
        self.stdin.flush().unwrap();
        let mut response = String::new();
        self.reader
            .as_mut()
            .unwrap()
            .read_line(&mut response)
            .unwrap();
        serde_json::from_str(&response).unwrap()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        // Wait for the process to fully exit: the child holds the database
        // dir lock, and a respawn on the same db before the OS tears it
        // down fails to open and dies before responding (EOF flake under
        // parallel load).
        let _ = self.child.wait();
    }
}

#[test]
fn real_world_agent_workflow() {
    let db = tmp_db("rw");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

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

    let _ = std::fs::remove_dir_all(&db);
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
    let db = tmp_db("s51");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);
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

    let _ = std::fs::remove_dir_all(&db);
}

/// G6 — Chatbot Memory Certification Scenarios (TP-3b): scripted replay of
/// the chatbot suite's conversation-level scenarios — §8 CHAT-MEM-001..005
/// (same-session, cross-session, restart persistence, explicit remember,
/// ephemeral non-conversion), §9 CLASS-001..005 (fact/preference/episode/
/// procedure/program classification), §11 PERS-001..004 (behavior change,
/// explainability, conflict resolution, scope confinement) — over the real
/// MCP surface with mechanical judges (PR-R pattern).
#[test]
fn chatbot_memory_certification_scenarios() {
    let db = tmp_db("cmem");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);
    c.session_init("chatbot-user", "acme");

    // ── §8 CHAT-MEM-001: same-session preference recall ────────────────────
    // "I prefer responses in English." → an asserted UserPreference.
    let lang = c.call(
        "assert_knowledge",
        &json!({
            "subject": "chatbot-user", "type_name": "UserPreference", "tenant": "acme",
            "properties": {"topic": "preferred language", "value": "English"},
            "authority": "human_approved",
            "evidence": [{"source_artifact": "chat-msg-lang", "method": "human_provided"}]
        }),
    );
    assert_eq!(lang["version"], 1);
    let recall_lang = c.call(
        "aikoql",
        &json!({
            "subject": "chatbot-user",
            "query": "MATCH UserPreference WHERE topic == \"preferred language\" RETURN *"
        }),
    );
    let lang_values: Vec<String> = recall_lang["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["properties"]["value"].as_str().map(String::from))
        .collect();
    assert!(
        lang_values.contains(&"English".to_string()),
        "CHAT-MEM-001: same-session preference must be recallable: {lang_values:?}"
    );

    // ── CHAT-MEM-002: cross-session recall ─────────────────────────────────
    // "I prefer concise answers." → remembered in conversation 1 …
    c.call(
        "assert_knowledge",
        &json!({
            "subject": "chatbot-user", "type_name": "UserPreference", "tenant": "acme",
            "properties": {"topic": "response style", "value": "concise"},
            "authority": "human_approved",
            "evidence": [{"source_artifact": "chat-msg-style", "method": "human_provided", "confidence": 0.9}]
        }),
    );
    // … available again in conversation 2 (fresh session, same identity).
    c.session_init("chatbot-user", "acme");
    let recall_style = c.call(
        "aikoql",
        &json!({
            "subject": "chatbot-user",
            "query": "MATCH UserPreference WHERE topic == \"response style\" RETURN *"
        }),
    );
    let style_values: Vec<String> = recall_style["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["properties"]["value"].as_str().map(String::from))
        .collect();
    assert!(
        style_values.contains(&"concise".to_string()),
        "CHAT-MEM-002: cross-session recall must find the preference: {style_values:?}"
    );

    // ── CHAT-MEM-003: persistence across server restart ────────────────────
    // Kill the server, reopen the same database, ask again.
    drop(c);
    let mut c = McpClient::start(&db);
    c.session_init("chatbot-user", "acme");
    let after_restart = c.call(
        "aikoql",
        &json!({
            "subject": "chatbot-user",
            "query": "MATCH UserPreference RETURN *"
        }),
    );
    let after_restart_values: Vec<String> = after_restart["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["properties"]["value"].as_str().map(String::from))
        .collect();
    assert!(
        after_restart_values.contains(&"English".to_string())
            && after_restart_values.contains(&"concise".to_string()),
        "CHAT-MEM-003: preferences must survive a full restart: {after_restart_values:?}"
    );

    // ── CHAT-MEM-004: explicit "Remember that …" → durable candidate ───────
    // "Remember that my preferred deployment environment is AWS."
    let aws = c.call("assert_knowledge", &json!({
        "subject": "chatbot-user", "type_name": "DeploymentPreference", "tenant": "acme",
        "properties": {"account": "ACME-123", "cloud": "AWS"},
        "authority": "human_approved",
        "evidence": [{"source_artifact": "chat-msg-4", "method": "human_provided", "confidence": 0.95}]
    }));
    let aws_koid = aws["koid"].as_str().unwrap().to_string();
    let aws_ko = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": &aws_koid}),
    );
    assert_eq!(aws_ko["extensions"]["authority"], "human_approved");
    assert_eq!(aws_ko["extensions"]["epistemic_status"], "asserted");
    assert!(
        aws_ko["extensions"]["evidence"]
            .to_string()
            .contains("chat-msg-4"),
        "CHAT-MEM-004: explicit remember must keep its evidence: {}",
        aws_ko["extensions"]["evidence"]
    );

    // ── CHAT-MEM-005: ephemeral statements are NOT auto-converted ──────────
    // "I am currently testing this on AWS." → an observation (status
    // "observed", non-assertive channel) — classification is the chatbot's
    // job; the substrate must not silently promote it to a preference.
    let obs = c.call(
        "observe",
        &json!({
            "subject": "chatbot-user", "type_name": "UserStatement", "tenant": "acme",
            "properties": {"environment": "AWS", "stage": "testing"},
            "evidence": [{"source_artifact": "chat-msg-5", "method": "human_provided"}]
        }),
    );
    let obs_koid = obs["koid"].as_str().unwrap().to_string();
    let obs_ko = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": &obs_koid}),
    );
    assert_eq!(
        obs_ko["extensions"]["epistemic_status"], "observed",
        "ephemeral statement must be stamped observed, not asserted: {obs_ko}"
    );
    let prefs_after_ephemeral = c.call(
        "aikoql",
        &json!({"subject": "chatbot-user", "query": "MATCH UserPreference RETURN *"}),
    );
    let pref_blobs: Vec<String> = prefs_after_ephemeral["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["properties"].to_string())
        .collect();
    assert!(
        !pref_blobs.iter().any(|p| p.contains("testing")),
        "CHAT-MEM-005: the ephemeral statement must not become a preference: {pref_blobs:?}"
    );

    // ── §9 CLASS: classification into memory types ─────────────────────────
    // CLASS-001: "My company is ACME." → semantic fact.
    let fact = c.call(
        "assert_knowledge",
        &json!({
            "subject": "chatbot-user", "type_name": "SemanticFact", "tenant": "acme",
            "properties": {"subject": "user company", "predicate": "is", "object": "ACME"},
            "authority": "human_approved",
            "evidence": [{"source_artifact": "chat-msg-6", "method": "human_provided"}]
        }),
    );
    let fact_ko = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": fact["koid"].as_str().unwrap()}),
    );
    assert_eq!(fact_ko["type_name"], "SemanticFact");
    assert_eq!(fact_ko["properties"]["object"], "ACME");

    // CLASS-002: preference → UserPreference KO (the concise one, §8).
    let style_ko = c.call(
        "aikoql",
        &json!({"subject": "chatbot-user", "query": "MATCH UserPreference WHERE topic == \"response style\" RETURN *"}),
    );
    let style_koid = style_ko["results"][0]["koid"].as_str().unwrap().to_string();
    let style_ko = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": &style_koid}),
    );
    assert_eq!(style_ko["type_name"], "UserPreference");

    // CLASS-003: "Yesterday I deployed ACME-123." → episodic memory.
    let ep = c.call(
        "record_experience",
        &json!({
            "subject": "chatbot-user",
            "goal": "Deploy ACME-123 yesterday",
            "action": "ran the deployment pipeline",
            "outcome": "success",
            "preconditions": [],
            "evidence": [{"source_artifact": "chat-msg-7", "method": "human_provided"}]
        }),
    );
    let ep_ko = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": ep["koid"].as_str().unwrap()}),
    );
    assert_eq!(ep_ko["type_name"], "aikoql:experience");

    // CLASS-004: "To reset an account: …" → procedural memory. Procedural
    // knowledge is an experience KO carrying reuse_conditions (there is no
    // separate aikoql:procedure type).
    let proc = c.call(
        "record_experience",
        &json!({
            "subject": "chatbot-user",
            "goal": "Reset an account",
            "action": "verify identity, then reset password",
            "outcome": "account reset",
            "preconditions": ["user verified identity"],
            "lesson": "always verify identity before resetting",
            "reuse_conditions": ["account reset request"],
            "evidence": [{"source_artifact": "chat-msg-8", "method": "human_provided"}]
        }),
    );
    let proc_ko = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": proc["koid"].as_str().unwrap()}),
    );
    assert_eq!(proc_ko["type_name"], "aikoql:experience");
    assert!(
        proc_ko["properties"]["reuse_conditions"]
            .to_string()
            .contains("account reset request"),
        "CLASS-004: procedural memory must carry reuse_conditions: {}",
        proc_ko["properties"]["reuse_conditions"]
    );

    // CLASS-005: "Run ResetAccount." → Program-as-KO.
    let prog = c.call(
        "deploy_program",
        &json!({
            "subject": "chatbot-user", "name": "ResetAccount",
            "body": "MATCH AccountInfo WHERE account == \"ACME-123\" RETURN *",
            "language": "aikoql"
        }),
    );
    let prog_ko = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": prog["koid"].as_str().unwrap()}),
    );
    assert_eq!(prog_ko["type_name"], "aikoql:program");

    // ── §11 PERS-001/002: behavior + explainability ────────────────────────
    // PERS-001: the preference that changes behavior is durable knowledge.
    // PERS-002: "Why do you answer concisely?" → provenance names the user
    // statement, the confidence, and the evidence chain.
    let prov = c.call(
        "provenance",
        &json!({"subject": "chatbot-user", "koid": &style_koid}),
    );
    let prov_md = prov["provenance"].as_str().unwrap();
    assert!(
        prov_md.contains("chat-msg-style"),
        "PERS-002: provenance must name the source chat message: {prov_md}"
    );
    assert!(
        prov_md.contains("Confidence:"),
        "PERS-002: provenance must carry the confidence: {prov_md}"
    );

    // ── PERS-003: conflict resolution keeps history ────────────────────────
    // "Actually I prefer detailed answers now." → supersede, not overwrite.
    let detailed = c.call("assert_knowledge", &json!({
        "subject": "chatbot-user", "type_name": "UserPreference", "tenant": "acme",
        "properties": {"topic": "response style", "value": "detailed"},
        "authority": "human_approved",
        "evidence": [{"source_artifact": "chat-msg-9", "method": "human_provided", "confidence": 1.0}]
    }));
    let detailed_koid = detailed["koid"].as_str().unwrap().to_string();
    c.call(
        "supersede",
        &json!({
            "subject": "chatbot-user",
            "old": &style_koid,
            "superseded_by": &detailed_koid,
            "reason": "user now prefers detailed answers",
            "evidence": [{"source_artifact": "chat-msg-9", "method": "human_provided"}]
        }),
    );
    // The old preference is closed but readable — history is never lost.
    let old_style = c.call(
        "get",
        &json!({"subject": "chatbot-user", "koid": &style_koid}),
    );
    assert_eq!(old_style["properties"]["value"], "concise");
    assert_eq!(old_style["extensions"]["epistemic_status"], "superseded");
    // Current-truth recall returns only the new preference.
    let current = c.call(
        "aikoql",
        &json!({"subject": "chatbot-user", "query": "MATCH UserPreference WHERE topic == \"response style\" RETURN *"}),
    );
    let current_values: Vec<String> = current["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["properties"]["value"].as_str().map(String::from))
        .collect();
    assert!(
        current_values.contains(&"detailed".to_string())
            && !current_values.contains(&"concise".to_string()),
        "PERS-003: current-truth recall must return only the new preference: {current_values:?}"
    );

    // ── PERS-004: user scope confinement ───────────────────────────────────
    // Another user in the same tenant must see neither the preference nor
    // the point object; a user preference never widens to org scope.
    c.session_init("other-user", "acme");
    let leak = c.call("aikoql", &json!({"query": "MATCH UserPreference RETURN *"}));
    assert_eq!(
        leak["results"].as_array().unwrap().len(),
        0,
        "PERS-004: another user's recall must not leak this user's preferences: {leak}"
    );
    let foreign = c.call_raw("get", &json!({"koid": &style_koid}));
    let foreign_text = foreign["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        foreign["result"]["isError"] == true && foreign_text.contains("ACCESS_DENIED"),
        "PERS-004: another user's point read must be denied: {foreign}"
    );

    let _ = std::fs::remove_dir_all(&db);
}

/// G7 — CTX differential scenarios (TP-3c): the same context-compilation
/// question over the real MCP surface under different permissions (CTX-001),
/// different temporal states (CTX-002), and post-update knowledge (CTX-003).
/// CTX-MIN-001..003 (1000-KO minimization, no irrelevant forwarding, dedup)
/// are pure-compiler tests in aikoql-ingestion's context::tests.
#[test]
fn ctx_differential_scenarios() {
    let db = tmp_db("ctx");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);
    c.session_init("alice", "acme");

    // Knowledge snapshot v1 — the same ir_json shape ingest-dir produces.
    let v1 = KnowledgeIr {
        entities: vec![
            EntityCandidate {
                name: "PaymentService".into(),
                type_hint: Some("Struct".into()),
                mentions: vec!["processes payments".into()],
                confidence: 0.9,
                evidence: Evidence::default(),
            },
            EntityCandidate {
                name: "Ledger".into(),
                type_hint: Some("Struct".into()),
                mentions: vec!["payment ledger".into()],
                confidence: 0.8,
                evidence: Evidence::default(),
            },
        ],
        facts: vec![FactCandidate {
            snippet: None,
            statement: "payments flow through Stripe".into(),
            entities: vec![],
            confidence: 0.9,
            evidence: Evidence::default(),
        }],
        ..Default::default()
    };
    let doc = c.call(
        "remember",
        &json!({
            "subject": "alice", "type_name": "KnowledgeSnapshot", "tenant": "acme",
            "properties": {"ir_json": serde_json::to_string(&v1).unwrap()},
            "origin": "system"
        }),
    );
    let doc_koid = doc["koid"].as_str().unwrap().to_string();

    // ── CTX-001: same question, two users, different permissions ──────────
    // Alice (owner) compiles the payments context…
    let alice_ctx = c.call(
        "compile_context",
        &json!({"subject": "alice", "koid": &doc_koid, "task": "process payments"}),
    );
    let alice_names: Vec<&str> = alice_ctx["package"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(
        alice_names.contains(&"PaymentService"),
        "the owner must get the payment context: {alice_ctx}"
    );
    // §36: a disabled/absent semantic index must be detectable in the
    // response — this harness has no embedding provider wired, so the
    // compile must say so instead of silently degrading.
    assert_eq!(
        alice_ctx["semantic"],
        json!(false),
        "semantic availability must be reported: {alice_ctx}"
    );

    // …Bob (same tenant, no grant) gets no context at all — the context
    // compilation layer is permission-differential, not just content-differential.
    c.session_init("bob", "acme");
    let bob_ctx = c.call_raw(
        "compile_context",
        &json!({"koid": &doc_koid, "task": "process payments"}),
    );
    let bob_text = bob_ctx["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        bob_ctx["result"]["isError"] == true && bob_text.contains("ACCESS_DENIED"),
        "CTX-001: a user without permission must get no context: {bob_ctx}"
    );

    // ── CTX-002: same question at two times — temporal state ──────────────
    // A fresh run experience (1s TTL) enters the context for the refund task…
    c.session_init("alice", "acme");
    c.call(
        "record_experience",
        &json!({
            "subject": "alice",
            "goal": "process payments refund",
            "action": "refund via payment service",
            "outcome": "success",
            "preconditions": [],
            "ttl_seconds": 1,
            "evidence": [{"source_artifact": "exec-run-2", "method": "runtime_observation"}]
        }),
    );
    let ctx_t0 = c.call(
        "compile_context",
        &json!({"subject": "alice", "koid": &doc_koid, "task": "process payments refund"}),
    );
    assert!(
        !ctx_t0["experiences"].as_array().unwrap().is_empty(),
        "t0: the fresh experience must be in the context: {ctx_t0}"
    );
    // …and drops out once its temporal window closes. Same question, same
    // knowledge — only time has passed, so only the temporal state differs.
    std::thread::sleep(std::time::Duration::from_millis(2200));
    let ctx_t1 = c.call(
        "compile_context",
        &json!({"subject": "alice", "koid": &doc_koid, "task": "process payments refund"}),
    );
    assert!(
        ctx_t1["experiences"].as_array().unwrap().is_empty(),
        "CTX-002: the expired experience must drop out of the context: {ctx_t1}"
    );

    // ── CTX-003: same question after a knowledge update ───────────────────
    // The snapshot moves to v2: internal ledger replaces Stripe.
    let v2 = KnowledgeIr {
        entities: vec![
            EntityCandidate {
                name: "PaymentService".into(),
                type_hint: Some("Struct".into()),
                mentions: vec!["processes payments".into()],
                confidence: 0.9,
                evidence: Evidence::default(),
            },
            EntityCandidate {
                name: "InternalLedger".into(),
                type_hint: Some("Struct".into()),
                mentions: vec!["internal payment ledger".into()],
                confidence: 0.8,
                evidence: Evidence::default(),
            },
        ],
        facts: vec![FactCandidate {
            snippet: None,
            statement: "payments flow through the internal ledger".into(),
            entities: vec![],
            confidence: 0.9,
            evidence: Evidence::default(),
        }],
        relations: vec![RelationCandidate {
            subject: "PaymentService".into(),
            predicate: "depends_on".into(),
            object: "InternalLedger".into(),
            confidence: 0.8,
            evidence: Evidence::default(),
        }],
        ..Default::default()
    };
    let upd = c.call(
        "remember",
        &json!({
            "subject": "alice", "koid": &doc_koid, "expected_version": 1,
            "type_name": "KnowledgeSnapshot", "tenant": "acme",
            "properties": {"ir_json": serde_json::to_string(&v2).unwrap()},
            "origin": "system"
        }),
    );
    assert_eq!(upd["version"], 2);

    let after = c.call(
        "compile_context",
        &json!({"subject": "alice", "koid": &doc_koid, "task": "process payments"}),
    );
    let fact_strs: Vec<&str> = after["package"]["facts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["statement"].as_str())
        .collect();
    assert!(
        fact_strs.iter().any(|s| s.contains("internal ledger")),
        "CTX-003: the updated context must carry the new fact: {after}"
    );
    assert!(
        !fact_strs.iter().any(|s| s.contains("Stripe")),
        "CTX-003: the replaced fact must not linger in the context: {fact_strs:?}"
    );
    let after_names: Vec<&str> = after["package"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(
        after_names.contains(&"InternalLedger"),
        "CTX-003: the new entity must enter the context: {after_names:?}"
    );

    let _ = std::fs::remove_dir_all(&db);
}

#[test]
fn mcp_ping_and_tools_list() {
    let db = tmp_db("ping");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

    // Ping
    let mut req = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    c.stdin
        .write_all((serde_json::to_string(&req).unwrap() + "\n").as_bytes())
        .unwrap();
    c.stdin.flush().unwrap();
    let mut resp = String::new();
    c.reader.as_mut().unwrap().read_line(&mut resp).unwrap();
    let v: J = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["result"], json!({}));

    // Tools list
    req = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    c.stdin
        .write_all((serde_json::to_string(&req).unwrap() + "\n").as_bytes())
        .unwrap();
    c.stdin.flush().unwrap();
    resp.clear();
    c.reader.as_mut().unwrap().read_line(&mut resp).unwrap();
    let v: J = serde_json::from_str(&resp).unwrap();
    let tools = v["result"]["tools"].as_array().unwrap();
    assert!(
        tools.len() >= 30,
        "Expected >=30 tools, got {}",
        tools.len()
    );

    let _ = std::fs::remove_dir_all(&db);
}

#[test]
fn mcp_idempotency_guarantee() {
    let db = tmp_db("idem");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

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

    let _ = std::fs::remove_dir_all(&db);
}

#[test]
fn mvp_rec_002_backup_destroy_restore_round_trip() {
    // MVP-QA-001 MVP-REC-002: backup → destroy → restore yields equivalent
    // knowledge — same KOID resolvable with the same content, and the
    // backup is listable.
    let db = tmp_db("recv");
    let _ = std::fs::remove_dir_all(&db);

    // Phase 1: build knowledge.
    let mut c = McpClient::start(&db);
    let note = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "note", "tenant": "acme",
            "properties": {"body": "quarterly revenue reached 42M", "memo": "rec002"}
        }),
    );
    let koid = note["koid"].as_str().unwrap().to_string();

    // MVP-QA-001 REC-002 equivalence legs (2026-08-25): relations,
    // provenance (evidence + assertion instant) and temporal state
    // (supersession) must all survive backup → destroy → restore.
    let asserted = c.call(
        "assert_knowledge",
        &json!({
            "subject": "admin", "type_name": "Policy",
            "properties": {"text": "retention is 30 days"},
            "authority": "architecture_decision",
            "evidence": [{"source_artifact": "runbook.md", "method": "doc_extraction"}],
            "valid_from": 1000
        }),
    );
    let asserted_koid = asserted["koid"].as_str().unwrap().to_string();

    let rel = c.call(
        "relate",
        &json!({
            "subject": "admin", "from": &koid, "to": &asserted_koid,
            "rel_type": "derived_from"
        }),
    );
    assert!(rel["koid"].as_str().is_some());

    let successor = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "note", "tenant": "acme",
            "properties": {"body": "quarterly revenue reached 45M", "memo": "rec002-v2"}
        }),
    );
    let successor_koid = successor["koid"].as_str().unwrap().to_string();
    let sup = c.call(
        "supersede",
        &json!({
            "subject": "admin",
            "old": &koid,
            "superseded_by": &successor_koid,
            "reason": "correction: 45M",
            "evidence": [{"source_artifact": "finance.md", "method": "human_provided"}]
        }),
    );
    assert_eq!(sup["new"], successor_koid);

    // Phase 2: verified backup + it must be listable.
    let backup = c.call("backup", &json!({"subject": "admin"}));
    assert_eq!(backup["verified"], true, "backup must verify: {backup}");
    let backup_dir = backup["backup"].as_str().unwrap().to_string();

    let list = c.call("list_backups", &json!({"subject": "admin"}));
    let names: Vec<&str> = list["backups"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["name"].as_str())
        .collect();
    assert!(
        names.iter().any(|n| backup_dir.ends_with(n)),
        "backup must appear in list_backups, got {names:?}"
    );

    // Phase 3: destroy — kill the server, delete the v2 database dir (give
    // the killed process a moment to release the handle on Windows).
    drop(c);
    let mut removed = false;
    for _ in 0..20 {
        if std::fs::remove_file(&db).is_ok() || std::fs::remove_dir_all(&db).is_ok() {
            removed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    assert!(removed, "destroy: database file must be removable");

    // Phase 4: fresh server on the same path (empty DB), then restore.
    let mut c = McpClient::start(&db);
    let restored = c.call(
        "restore",
        &json!({"subject": "admin", "backup": &backup_dir}),
    );
    assert_eq!(
        restored["restored"], true,
        "restore must succeed: {restored}"
    );

    // Phase 5: the restored file lands on reopen — restart the server.
    drop(c);
    let mut c = McpClient::start(&db);

    // Phase 6: equivalent knowledge — same KOID, same content.
    let fetched = c.call("get", &json!({"koid": &koid, "subject": "admin"}));
    assert_eq!(fetched["type_name"], "note");
    assert_eq!(
        fetched["properties"]["body"],
        "quarterly revenue reached 42M"
    );

    // Relations survive: note → Policy derived_from.
    let rels = fetched["relationships"].as_array().unwrap();
    assert!(
        rels.iter().any(|r| r["target"] == asserted_koid),
        "relation to asserted policy must survive restore: {fetched}"
    );

    // Temporal state survives: the supersession mark + successor link.
    assert_eq!(fetched["extensions"]["epistemic_status"], "superseded");
    assert!(
        rels.iter().any(|r| r["target"] == successor_koid),
        "supersession link must survive restore"
    );

    // Provenance survives: evidence list + assertion instant on the asserted KO.
    let restored_asserted = c.call("get", &json!({"koid": &asserted_koid, "subject": "admin"}));
    let evidence = restored_asserted["extensions"]["evidence"]
        .as_array()
        .unwrap();
    assert!(!evidence.is_empty(), "evidence must survive restore");
    assert_eq!(restored_asserted["extensions"]["valid_from"], 1000);

    let _ = std::fs::remove_dir_all(&db);
}

// P3-M3 bkp005 — MCP backup/restore take the engine-native snapshot
// (§58–60): the backup dir holds the manifest + segments + logs +
// torn-safe WAL and exactly one SNAPSHOT-{gen} marker (the commit point),
// and restore verifies then swaps rows through the live kernel.
#[test]
fn p3m3_bkp005_backup_restore_route_by_backend() {
    // ── v2 leg (the production default): engine-native snapshot ──────────
    let db = tmp_db("bkp005v2");
    let _ = std::fs::remove_dir_all(&db);
    let _ = std::fs::remove_dir_all(&db);

    let mut c = McpClient::start(&db);
    let note = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "note",
            "properties": {"body": "bkp005 native snapshot", "memo": "bkp005"}
        }),
    );
    let koid = note["koid"].as_str().unwrap().to_string();

    let backup = c.call("backup", &json!({"subject": "admin"}));
    assert_eq!(backup["verified"], true, "v2 backup must verify: {backup}");
    assert_eq!(
        backup["engine"], "aikoql-v2",
        "v2 backup routes engine-native: {backup}"
    );
    assert!(
        backup["generation"].as_u64().unwrap() > 0,
        "v2 backup records its generation"
    );
    let backup_dir = std::path::PathBuf::from(backup["backup"].as_str().unwrap());
    let entries: Vec<String> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        entries.iter().any(|n| n.starts_with("SNAPSHOT-")),
        "v2 backup must hold a snapshot marker, got {entries:?}"
    );
    assert!(
        !entries.iter().any(|n| n.ends_with(".redb")),
        "v2 backup must hold no redb file, got {entries:?}"
    );

    // verify_backup verifies the snapshot marker.
    let v = c.call(
        "verify_backup",
        &json!({"subject": "admin", "backup": backup_dir.to_str().unwrap()}),
    );
    assert_eq!(
        v["verified"], true,
        "verify_backup must accept the marker: {v}"
    );

    // destroy → fresh v2 server → restore → restart → knowledge is back.
    drop(c);
    let mut removed = false;
    for _ in 0..20 {
        if std::fs::remove_dir_all(&db).is_ok() {
            removed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    assert!(removed, "destroy: v2 database dir must be removable");

    let mut c = McpClient::start(&db);
    let restored = c.call(
        "restore",
        &json!({"subject": "admin", "backup": backup_dir.to_str().unwrap()}),
    );
    assert_eq!(restored["restored"], true, "v2 native restore: {restored}");
    assert_eq!(
        restored["engine"], "aikoql-v2",
        "restore routed engine-native: {restored}"
    );
    assert!(
        restored["rows_restored"].as_u64().unwrap() >= 1,
        "restore must report rows: {restored}"
    );
    drop(c);
    let mut c = McpClient::start(&db);
    let fetched = c.call("get", &json!({"koid": &koid, "subject": "admin"}));
    assert_eq!(
        fetched["properties"]["body"], "bkp005 native snapshot",
        "restored knowledge must read back: {fetched}"
    );
    drop(c);
}

#[test]
fn batch_ops_inherit_session_identity() {
    // F2: batch ops without an explicit subject land as mcp-agent and the
    // submitting session then hits ACCESS_DENIED on its own KO.
    let db = tmp_db("batch-ident");
    let mut c = McpClient::start(&db);
    c.session_init("device-identity-eval", "acme");
    let batch = c.call(
        "batch",
        &json!({
            "operations": [{
                "op": "remember",
                "type_name": "device",
                "properties": {"device_id": "d1", "farm": "f07"},
                "idempotency_key": "batch-ident-d1"
            }]
        }),
    );
    let koid = batch["results"][0]["result"]["koid"]
        .as_str()
        .unwrap()
        .to_string();
    let got = c.call("get", &json!({"koid": &koid}));
    assert_eq!(
        got["koid"],
        json!(koid),
        "batch op must inherit the submitting session's identity: {got}"
    );

    // Fill-if-absent, not override: an op with its own subject keeps it.
    let batch2 = c.call(
        "batch",
        &json!({
            "operations": [{
                "op": "remember",
                "type_name": "device",
                "subject": "another-agent",
                "properties": {"device_id": "d2"}
            }]
        }),
    );
    let koid2 = batch2["results"][0]["result"]["koid"]
        .as_str()
        .unwrap()
        .to_string();
    let got2 = c.call_raw("get", &json!({"koid": &koid2}));
    let text2 = got2["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        got2["result"]["isError"] == true && text2.contains("ACCESS_DENIED"),
        "an explicit op subject must survive injection: {got2}"
    );
}

#[test]
fn replay_relate_through_batch_is_version_idempotent() {
    // F13: re-applying an identical relate on replay re-versions the source
    // (device-eval DI-002: 14 d2 edges re-versioned — koid stable, edge set
    // unchanged, version bumped).
    let db = tmp_db("relate-replay");
    let mut c = McpClient::start(&db);
    c.session_init("device-identity-eval", "acme");

    let mk = |idem: &str, dev: &str| {
        json!({
            "op": "remember",
            "type_name": "device",
            "properties": {"device_id": dev},
            "idempotency_key": idem
        })
    };
    let b1 = c.call(
        "batch",
        &json!({"operations": [mk("f13-d1", "d1"), mk("f13-d2", "d2")]}),
    );
    let d1 = b1["results"][0]["result"]["koid"]
        .as_str()
        .unwrap()
        .to_string();
    let d2 = b1["results"][1]["result"]["koid"]
        .as_str()
        .unwrap()
        .to_string();
    let rel = json!({"op": "relate", "from": &d1, "to": &d2, "rel_type": "linked_to"});

    let first = c.call("batch", &json!({"operations": [rel.clone()]}));
    let v_first = first["results"][0]["result"]["version"].as_u64().unwrap();

    // Replay: the same remember ops (idempotency keys) + the same relate.
    let replay = c.call(
        "batch",
        &json!({"operations": [mk("f13-d1", "d1"), mk("f13-d2", "d2"), rel]}),
    );
    let v_replay = replay["results"][2]["result"]["version"].as_u64().unwrap();
    assert_eq!(
        v_replay, v_first,
        "an identical relate replayed through batch must not re-version the source"
    );
    let head = c.call("get", &json!({"koid": &d1}));
    assert_eq!(
        head["version"],
        json!(v_first),
        "the source head must stay at the first relate's version"
    );
}

#[test]
fn t57_ordered_replay_through_batch_converges() {
    // P3-008 MEDIUM: the P3-007 corpus shape (ingest -> correct -> retract,
    // per-op idempotency keys, per-op tenant stamps, retracts targeting a
    // prior op's returned koid) must replay through the batch tool and
    // converge — a full re-send of the same batches is a no-op, and the
    // AS_OF slices re-check against the expected corpus timeline.
    let db = tmp_db("t57-replay");
    let mut c = McpClient::start(&db);
    c.session_init("device-identity-eval", "acme");
    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    };
    let t_pre = now_ms();
    let ev = json!([{"source_artifact": "t57-pin", "method": "human_provided"}]);

    // Batch A: two ingests (one tenant-stamped per-op) + an in-batch
    // correction targeting $1.koid — the handle of the first op. MVCC picks
    // versions by commit_ts, so the oracle slices are the markers BETWEEN
    // batches (nothing exists before batch A commits).
    let mk_batch_a = || {
        json!({
            "operations": [
                {"op": "remember", "type_name": "device",
                 "properties": {"device_id": "dev0", "value": "v0"},
                 "extensions": {"valid_from": t_pre - 60_000},
                 "idempotency_key": "t57-s1-0"},
                {"op": "remember", "type_name": "device",
                 "tenant": "other",
                 "properties": {"device_id": "dev1", "value": "v1"},
                 "extensions": {"valid_from": t_pre - 60_000},
                 "idempotency_key": "t57-s1-1"},
                {"op": "supersede", "old": "$1.koid", "type_name": "device",
                 "properties": {"device_id": "dev0", "value": "v0b"},
                 "evidence": ev, "idempotency_key": "t57-c1",
                 "reason": "corpus correction t57-c1"}
            ]
        })
    };
    let batch_a = c.call("batch", &mk_batch_a());
    for (i, r) in batch_a["results"].as_array().unwrap().iter().enumerate() {
        assert!(
            r["ok"] == true,
            "batch A op {i} must apply through the bulk path: {batch_a}"
        );
    }
    let koid0 = batch_a["results"][0]["result"]["koid"]
        .as_str()
        .unwrap()
        .to_string();
    let koid1 = batch_a["results"][1]["result"]["koid"]
        .as_str()
        .unwrap()
        .to_string();
    let new0 = batch_a["results"][2]["result"]["new"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        batch_a["results"][2]["result"]["old"],
        json!(koid0),
        "$1.koid must resolve to the first op's returned handle: {batch_a}"
    );

    let t_mid = now_ms();
    // Batch B must commit in a strictly later millisecond than t_mid: the
    // retraction stamps valid_to = its own commit ms, and F12's half-open
    // [valid_from, valid_to) interval hides the row at valid_to itself — a
    // same-ms retract would make AS_OF t_mid (correctly) empty. Wait out
    // the millisecond so the slice is unambiguously pre-retraction.
    while now_ms() <= t_mid {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // Batch B: the corpus retraction — targets a handle batch A returned.
    let mk_batch_b = || {
        json!({
            "operations": [
                {"op": "supersede", "old": koid1, "type_name": "device",
                 "retract": true, "tenant": "other", "evidence": ev,
                 "idempotency_key": "t57-r1", "reason": "corpus retraction t57-r1"}
            ]
        })
    };
    let batch_b = c.call("batch", &mk_batch_b());
    assert!(
        batch_b["results"][0]["ok"] == true,
        "retract must ride batch: {batch_b}"
    );
    assert!(
        batch_b["results"][0]["result"]["new"].is_null(),
        "a retraction has no successor: {batch_b}"
    );
    let t_post = now_ms();

    // Replay both batches: per-op idempotency keys make the re-send a no-op.
    let replay_a = c.call("batch", &mk_batch_a());
    for (i, r) in replay_a["results"].as_array().unwrap().iter().enumerate() {
        assert!(
            r["ok"] == true,
            "batch A replay op {i} must converge, not error: {replay_a}"
        );
    }
    assert_eq!(replay_a["results"][0]["result"]["koid"], json!(koid0));
    assert_eq!(replay_a["results"][2]["result"]["new"], json!(new0));
    let replay_b = c.call("batch", &mk_batch_b());
    assert!(
        replay_b["results"][0]["ok"] == true,
        "a replayed retraction must converge via its idempotency key: {replay_b}"
    );

    // Context oracle: the AS_OF slices + heads must match the corpus timeline.
    let proj = |resp: &J| -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = resp["results"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| {
                        (
                            r["properties"]["device_id"]
                                .as_str()
                                .unwrap_or("")
                                .to_string(),
                            r["properties"]["value"].as_str().unwrap_or("").to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    };
    let mid = c.call(
        "aikoql",
        &json!({"query": format!("MATCH device AS_OF {t_mid} RETURN *")}),
    );
    assert_eq!(
        proj(&mid),
        vec![("dev0".into(), "v0b".into())],
        "AS_OF t_mid must show the corrected value (batch A landed before t_mid): {mid}"
    );
    let mid_other = c.call(
        "aikoql",
        &json!({"query": format!("MATCH device AS_OF {t_mid} RETURN *"), "tenant": "other"}),
    );
    assert_eq!(
        proj(&mid_other),
        vec![("dev1".into(), "v1".into())],
        "AS_OF t_mid under the per-op tenant must show dev1 before its retraction: {mid_other}"
    );
    let post_other = c.call(
        "aikoql",
        &json!({"query": format!("MATCH device AS_OF {t_post} RETURN *"), "tenant": "other"}),
    );
    assert_eq!(
        proj(&post_other),
        vec![],
        "AS_OF t_post must hide the retracted row: {post_other}"
    );
    let head = c.call("aikoql", &json!({"query": "MATCH device RETURN *"}));
    assert_eq!(
        proj(&head),
        vec![("dev0".into(), "v0b".into())],
        "head must show the corrected value only: {head}"
    );
    let head_other = c.call(
        "aikoql",
        &json!({"query": "MATCH device RETURN *", "tenant": "other"}),
    );
    assert_eq!(
        proj(&head_other),
        vec![],
        "head under the other tenant must hide the retracted row: {head_other}"
    );
}

#[test]
fn serve_restart_catchup_preserves_edges_for_relate_replay() {
    // F13 end-to-end (the device-eval DI-002 pipeline): run1 remembers and
    // relates, run2 restarts the serve — the start-up catch-up enriches
    // every KO, and pre-T-34 enrichment rode the remember-update path,
    // wiping caller edges between the relate and its replay. The replay
    // relate then missed the no-op guard and re-versioned the source.
    // Needs the local embedding model; skips where none is installed.
    let Some(models_root) = installed_models_root() else {
        eprintln!("[SKIP] no local embedding model (run `aikoql model install`)");
        return;
    };
    let db = tmp_db("relate-restart");
    let mk = |idem: &str, dev: &str| {
        json!({
            "op": "remember",
            "type_name": "device",
            "properties": {"device_id": dev},
            "idempotency_key": idem
        })
    };
    // Serve A: no enrichment provider (empty model dir) — remember + relate.
    let (d1, rel) = {
        let mut a = McpClient::start(&db);
        a.session_init("device-identity-eval", "acme");
        let b1 = a.call(
            "batch",
            &json!({"operations": [mk("f13e-d1", "d1"), mk("f13e-d2", "d2")]}),
        );
        let d1 = b1["results"][0]["result"]["koid"]
            .as_str()
            .unwrap()
            .to_string();
        let d2 = b1["results"][1]["result"]["koid"]
            .as_str()
            .unwrap()
            .to_string();
        let rel = json!({"op": "relate", "from": &d1, "to": &d2, "rel_type": "linked_to"});
        a.call("batch", &json!({"operations": [rel.clone()]}));
        let head = a.call("get", &json!({"koid": &d1}));
        assert_eq!(
            head["relationships"].as_array().map(|r| r.len()),
            Some(1),
            "serve A must record the relate edge before the restart"
        );
        (d1, rel)
    }; // drop serve A: child killed and waited, the db dir survives

    // Serve B: real model store — start-up catch-up enriches both devices.
    let mut b = McpClient::start_with_model_dir(&db, &models_root);
    b.session_init("device-identity-eval", "acme");

    // PRR-3: the enrichment worker flips health to "ready" only after the
    // catch-up scan completes.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    loop {
        let h = b.call("health", &json!({}));
        let state = h["semantic"]["state"].as_str().unwrap_or("initializing");
        if state == "ready" {
            break;
        }
        if state == "unavailable" {
            panic!("enrichment unavailable: {}", h["semantic"]["detail"]);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "catch-up enrichment never reached ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    // Wipe tooth: enrichment must not have destroyed the caller edge.
    let head = b.call("get", &json!({"koid": &d1}));
    assert!(
        !head["relationships"].as_array().unwrap().is_empty(),
        "catch-up enrichment wiped the caller-created edge"
    );
    let v_head = head["version"].as_u64().unwrap();

    // Replay: the same remember ops (idempotency keys) + the same relate.
    let replay = b.call(
        "batch",
        &json!({"operations": [mk("f13e-d1", "d1"), mk("f13e-d2", "d2"), rel]}),
    );
    assert_eq!(
        replay["results"][2]["result"]["version"].as_u64().unwrap(),
        v_head,
        "the replayed relate must no-op after restart catch-up enrichment"
    );
    let after = b.call("get", &json!({"koid": &d1}));
    assert_eq!(after["version"].as_u64().unwrap(), v_head);
    assert_eq!(
        after["relationships"].as_array().map(|r| r.len()),
        Some(1),
        "the edge set must survive enrichment and replay unchanged"
    );
}

#[test]
fn query_group_by_count_aggregate_surfaces_through_tool() {
    // device-eval MINOR-1: "no GROUP BY count aggregate" — the eval's
    // binary predated T-32 and dropped Grouped rows at the tool layer, so
    // COUNT(*) looked absent. The compiler/runtime has executed the
    // aggregate since P5-M2 (count = every row, count(field) = non-null);
    // this pins the end-to-end tool path the eval drives.
    let db = tmp_db("cnt");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);
    c.session_init("admin", "acme");

    for (name, dept, salary) in [
        ("Alice Chen", "Engineering", 165_000),
        ("Bob Ortiz", "Engineering", 152_000),
        ("Carol Wu", "Sales", 131_000),
    ] {
        let _ = c.call(
            "remember",
            &json!({
                "subject": "admin", "type_name": "Employee", "tenant": "acme",
                "properties": {"name": name, "dept": dept, "salary": salary}
            }),
        );
    }

    // GROUP BY <key>, COUNT(*) — the mixed key/aggregate list.
    let res = c.call(
        "aikoql",
        &json!({"query": "MATCH Employee GROUP BY dept, COUNT(*) RETURN *"}),
    );
    let rows = res["results"]
        .as_array()
        .expect("grouped rows must surface through the tool");
    assert_eq!(rows.len(), 2, "two dept groups, got {rows:?}");
    let eng = rows
        .iter()
        .find(|r| r["properties"]["dept"] == "Engineering")
        .unwrap_or_else(|| panic!("Engineering group missing: {rows:?}"));
    let sales = rows
        .iter()
        .find(|r| r["properties"]["dept"] == "Sales")
        .unwrap_or_else(|| panic!("Sales group missing: {rows:?}"));
    assert_eq!(
        eng["properties"]["count"],
        json!(2),
        "COUNT(*) must count every row in the group"
    );
    assert_eq!(sales["properties"]["count"], json!(1));

    // Global aggregate (no keys): one row over the whole match set.
    let res = c.call(
        "aikoql",
        &json!({"query": "MATCH Employee GROUP BY COUNT(*) RETURN *"}),
    );
    let rows = res["results"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one global group, got {rows:?}");
    assert_eq!(rows[0]["properties"]["count"], json!(3));
}

#[test]
fn tool_boundary_emits_plain_epoch_millis_commit_ts() {
    // device-eval MINOR-2: the commit_ts hybrid encoding leaks through the
    // API — `epoch_ms << 16 | counter` forced clients to shift right 16
    // before AS_OF/validity math. The tool boundary emits plain epoch
    // millis; the counter stays kernel-internal.
    let db = tmp_db("cts");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);
    c.session_init("admin", "acme");
    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    };
    let before = now_ms();
    let res = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "Device", "tenant": "acme",
            "properties": {"mac": "aa:bb:cc:dd:ee:ff"}
        }),
    );
    let after = now_ms();
    let koid = res["koid"].as_str().unwrap().to_string();
    let ts = res["commit_ts"]
        .as_u64()
        .expect("remember must carry commit_ts");
    assert!(
        ts >= before && ts <= after + 60_000,
        "remember commit_ts must be plain epoch millis inside the commit \
         window (before={before}, after={after}, ts={ts})"
    );
    let got = c.call("get", &json!({"koid": koid}));
    let gts = got["commit_ts"].as_u64().expect("get must carry commit_ts");
    assert!(
        gts >= before && gts <= after + 60_000,
        "get commit_ts must be plain epoch millis inside the commit window \
         (ts={gts})"
    );
}

// ── T-44: compile_context must never queue behind the enrichment worker ──
// The device-eval corpus drove compile_context while the enrichment worker
// held the embedding model lock (one scalar-CPU forward ≈ 3s), and the
// training client's 5s socket timeout expired mid-catch-up. The pins below
// park the worker's embed holding the model lock (deterministic contention)
// and bound the compile response: queueing callers have no answer inside
// the bound, fail-fast callers answer instantly.

fn t44_snapshot() -> KnowledgeIr {
    KnowledgeIr {
        entities: vec![EntityCandidate {
            name: "PaymentService".into(),
            type_hint: Some("Struct".into()),
            mentions: vec!["processes payments".into()],
            confidence: 0.9,
            evidence: Evidence::default(),
        }],
        facts: vec![FactCandidate {
            snippet: None,
            statement: "payments flow through Stripe".into(),
            entities: vec![],
            confidence: 0.9,
            evidence: Evidence::default(),
        }],
        ..Default::default()
    }
}

/// Park the enrichment worker's Nth embed on the serve provider and wait
/// for the marker it writes while holding the model lock.
fn t44_parked_client(
    db: &str,
    models_root: &str,
    park_at: &str,
) -> (McpClient, std::path::PathBuf) {
    let marker = std::path::PathBuf::from(db).with_extension("park-marker");
    let _ = std::fs::remove_file(&marker);
    let client = McpClient::start_with_model_dir_env(
        db,
        models_root,
        &[
            ("AIKOQL_EMBED_PARK_AT", park_at),
            ("AIKOQL_EMBED_PARK_MARKER", marker.to_str().unwrap()),
        ],
    );
    (client, marker)
}

fn t44_wait_parked(marker: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "enrichment worker never entered the park"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn compile_context_stays_bounded_while_enrichment_holds_the_model() {
    // The corpus shape: a fresh snapshot is remembered, the enrichment
    // worker starts embedding it (parked here), and compile_context runs
    // mid-catch-up. It must answer inside the bound — lexically — instead
    // of queueing its semantic embed behind the worker.
    let Some(models_root) = installed_models_root() else {
        eprintln!("[SKIP] no local embedding model (run `aikoql model install`)");
        return;
    };
    let db = tmp_db("cc-park");
    let (mut c, marker) = t44_parked_client(&db, &models_root, "1");
    c.session_init("alice", "acme");
    let doc = c.call(
        "remember",
        &json!({
            "subject": "alice", "type_name": "KnowledgeSnapshot", "tenant": "acme",
            "properties": {"ir_json": serde_json::to_string(&t44_snapshot()).unwrap()},
            "origin": "system"
        }),
    );
    let doc_koid = doc["koid"].as_str().unwrap().to_string();
    t44_wait_parked(&marker);

    let ctx = c.call_bounded(
        "compile_context",
        &json!({"subject": "alice", "koid": &doc_koid, "task": "process payments"}),
        std::time::Duration::from_secs(2),
    );
    let names: Vec<&str> = ctx["package"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(
        names.contains(&"PaymentService"),
        "the lexical package must survive the busy model: {ctx}"
    );
    assert_eq!(
        ctx["semantic"],
        json!(false),
        "a queued semantic leg must degrade, not block: {ctx}"
    );
}

#[test]
fn compile_context_fails_fast_when_model_busy_with_stored_embeddings() {
    // The residual race: the snapshot ALREADY carries entity_embeddings
    // (ingest wrote them), so the semantic leg runs — but the worker's
    // next embed parks holding the model lock. The embed must fail fast
    // (Retryable) instead of queueing on the mutex.
    let Some(models_root) = installed_models_root() else {
        eprintln!("[SKIP] no local embedding model (run `aikoql model install`)");
        return;
    };
    let db = tmp_db("cc-busy");
    let (mut c, marker) = t44_parked_client(&db, &models_root, "2");
    c.session_init("alice", "acme");
    let k1 = c.call(
        "remember",
        &json!({
            "subject": "alice", "type_name": "KnowledgeSnapshot", "tenant": "acme",
            "properties": {
                "ir_json": serde_json::to_string(&t44_snapshot()).unwrap(),
                "entity_embeddings": "{\"d::PaymentService\": [0.1, 0.2]}"
            },
            "origin": "system"
        }),
    );
    let k1_koid = k1["koid"].as_str().unwrap().to_string();
    // The worker's second embed (this KO) parks holding the model lock.
    let _ = c.call(
        "remember",
        &json!({
            "subject": "alice", "type_name": "KnowledgeSnapshot", "tenant": "acme",
            "properties": {"ir_json": serde_json::to_string(&t44_snapshot()).unwrap()},
            "origin": "system"
        }),
    );
    t44_wait_parked(&marker);

    let ctx = c.call_bounded(
        "compile_context",
        &json!({"subject": "alice", "koid": &k1_koid, "task": "process payments"}),
        std::time::Duration::from_secs(2),
    );
    assert!(
        !ctx["package"]["entities"].as_array().unwrap().is_empty(),
        "the lexical package must survive the busy model: {ctx}"
    );
    assert_eq!(
        ctx["semantic"],
        json!(false),
        "a contended embed must fail fast, not queue: {ctx}"
    );
}

#[test]
fn compile_context_skips_semantic_embed_without_stored_embeddings() {
    // Guard-A tooth: with no entity_embeddings on the snapshot there is
    // nothing to score the task against, so the semantic leg must not burn
    // a full forward pass (~3s scalar) inside the caller's socket budget.
    let Some(models_root) = installed_models_root() else {
        eprintln!("[SKIP] no local embedding model (run `aikoql model install`)");
        return;
    };
    let db = tmp_db("cc-skip");
    let mut c = McpClient::start_with_model_dir(&db, &models_root);
    c.session_init("alice", "acme");
    let doc = c.call(
        "remember",
        &json!({
            "subject": "alice", "type_name": "KnowledgeSnapshot", "tenant": "acme",
            "properties": {"ir_json": serde_json::to_string(&t44_snapshot()).unwrap()},
            "origin": "system"
        }),
    );
    let doc_koid = doc["koid"].as_str().unwrap().to_string();

    let ctx = c.call_bounded(
        "compile_context",
        &json!({"subject": "alice", "koid": &doc_koid, "task": "process payments"}),
        std::time::Duration::from_secs(1),
    );
    assert_eq!(
        ctx["semantic"],
        json!(false),
        "no stored embeddings → no embed, no score: {ctx}"
    );
}

// ── T-45: the default rate limit serves a batch ingest phase ──
// The device eval throttles at 115 and takes ~130 batch calls per
// dataset phase against a default-configured server — the 120/min
// cap denied the tail of every phase. A legitimate batch phase from
// one principal must fit the default budget.

#[test]
fn default_rate_limit_serves_a_batch_ingest_phase() {
    let db = tmp_db("rl-batch");
    let mut c = McpClient::start(&db);
    c.session_init("alice", "acme");
    let started = std::time::Instant::now();
    for i in 0..130 {
        let doc = c.call(
            "remember",
            &json!({
                "subject": "alice", "type_name": "note", "tenant": "acme",
                "properties": {"body": format!("batch note {i}")},
                "origin": "system"
            }),
        );
        assert!(
            doc["koid"].is_string(),
            "remember #{i} must not trip the default rate limit: {doc}"
        );
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(50),
        "the batch phase straddled a window rollover — the pin proves nothing"
    );
}

// T-49 (POC-3 B3-1): supersede must read `extensions.valid_from` for the
// successor exactly as remember does — commit time is only the fallback.
// The device stream back-dates corrections by event_time; stamping the
// commit instant instead makes the successor assert validity in the future
// and breaks AS_OF reconstruction of the device timeline.
#[test]
fn supersede_honors_extensions_valid_from() {
    let db = tmp_db("t49vf");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

    let note = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "note",
            "properties": {"body": "v1"}
        }),
    );
    let old = note["koid"].as_str().unwrap().to_string();

    let sup = c.call(
        "supersede",
        &json!({
            "subject": "admin", "old": &old, "type_name": "note",
            "properties": {"body": "v2"},
            "extensions": {"valid_from": 1_700_000_000_000u64},
            "evidence": [{"source_artifact": "probe", "method": "runtime_observation"}]
        }),
    );
    let new = sup["new"].as_str().unwrap().to_string();

    let got = c.call("get", &json!({"koid": &new, "subject": "admin"}));
    assert_eq!(
        got["extensions"]["valid_from"], 1_700_000_000_000u64,
        "successor valid_from must honor extensions.valid_from, got: {got}"
    );

    let _ = std::fs::remove_dir_all(&db);
}

// T-50 (POC-3 B3-4): a supersede successor must inherit the replaced row's
// tenant. The successor generation replaces the claim — an untenanted
// successor is shared (ACL R9: untenanted objects stay visible), so the row
// escapes tenant_a's confinement and leaks into every other tenant's scans.
#[test]
fn supersede_successor_inherits_tenant() {
    let db = tmp_db("t50tn");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

    let note = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "device", "tenant": "tenant_a",
            "properties": {"key": "d1", "value": "v1"}
        }),
    );
    let old = note["koid"].as_str().unwrap().to_string();

    let sup = c.call(
        "supersede",
        &json!({
            "subject": "admin", "old": &old, "type_name": "device",
            "properties": {"key": "d1", "value": "v2"},
            "evidence": [{"source_artifact": "probe", "method": "runtime_observation"}]
        }),
    );
    assert!(sup["new"].is_string(), "successor must exist: {sup}");

    // A foreign tenant must not see the successor (pre-fix: untenanted
    // successors are shared, escaping tenant_a's confinement).
    let leak = c.call(
        "aikoql",
        &json!({
            "subject": "admin", "tenant": "tenant_b",
            "query": "MATCH device RETURN *"
        }),
    );
    assert_eq!(
        leak["results"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "tenant_b must not see tenant_a's successor: {leak}"
    );

    let _ = std::fs::remove_dir_all(&db);
}

// T-52 (POC-3 P3-009 HIGH): the idempotency key namespace was global, so a
// tenant_b remember with tenant_a's key replayed tenant_a's commit — tenant_b
// received the foreign koid and its own write silently vanished. Acceptance:
// same key, two tenants => two distinct KOs, both writes persisted.
#[test]
fn idempotency_key_is_tenant_scoped() {
    let db = tmp_db("t52id");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

    let a = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "device", "tenant": "tenant_a",
            "idempotency_key": "p9-twin",
            "properties": {"key": "d1", "value": "v1"}
        }),
    );
    let koid_a = a["koid"].as_str().unwrap().to_string();

    let b = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "device", "tenant": "tenant_b",
            "idempotency_key": "p9-twin",
            "properties": {"key": "d1", "value": "v2"}
        }),
    );
    assert_ne!(
        b["koid"].as_str().unwrap(),
        koid_a.as_str(),
        "tenant_b must not receive tenant_a's koid on replay: {b}"
    );

    // Both writes persisted — tenant_b's value is v2, not silently dropped.
    let match_b = c.call(
        "aikoql",
        &json!({
            "subject": "admin", "tenant": "tenant_b",
            "query": "MATCH device RETURN *"
        }),
    );
    let rows = match_b["results"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "tenant_b's write must persist: {match_b}");
    assert_eq!(rows[0]["properties"]["value"], json!("v2"));

    // Same-tenant retry stays exact-once.
    let a2 = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "device", "tenant": "tenant_a",
            "idempotency_key": "p9-twin",
            "properties": {"key": "d1", "value": "v1"}
        }),
    );
    assert_eq!(a2["koid"].as_str().unwrap(), koid_a.as_str());

    let _ = std::fs::remove_dir_all(&db);
}

// T-51 (POC-3 B3-3): `retract: true` ends validity without creating a shell
// successor. The G-002 workaround shape (supersede with no properties)
// created an empty v1 row — properties {} — visible in every MATCH head and
// AS_OF slice; a retracted device link must leave nothing behind.
#[test]
fn supersede_retract_leaves_no_shell_successor() {
    let db = tmp_db("t51rt");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

    let note = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "device",
            "properties": {"key": "d1", "value": "v1"}
        }),
    );
    let old = note["koid"].as_str().unwrap().to_string();

    let r = c.call(
        "supersede",
        &json!({
            "subject": "admin", "old": &old, "type_name": "device",
            "retract": true,
            "evidence": [{"source_artifact": "probe", "method": "runtime_observation"}]
        }),
    );
    assert_eq!(
        r["new"],
        json!(null),
        "retraction must not create a shell: {r}"
    );

    let m = c.call(
        "aikoql",
        &json!({
            "subject": "admin",
            "query": "MATCH device RETURN *"
        }),
    );
    assert_eq!(
        m["results"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "no shell row may remain in MATCH head: {m}"
    );

    let _ = std::fs::remove_dir_all(&db);
}

// T-53 (POC-3 Stage C HIGH): plain MATCH answers with current truth only —
// a supersede chain of a device key must leave exactly ONE row (the final
// generation) in the head; every closed generation stays out. History stays
// reachable through AS_OF (T-38), never through the default read.
#[test]
fn superseded_generations_stay_out_of_plain_match() {
    let db = tmp_db("t53sc");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

    let mut old = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "device", "tenant": "tenant_a",
            "properties": {"key": "dev_053", "value": "v1"}
        }),
    )["koid"]
        .as_str()
        .unwrap()
        .to_string();

    // Four supersede hops => five generations of the same key. gen3's commit
    // instant is captured for the AS_OF history assertion.
    let mut mid_ts: u64 = 0;
    for gen in 2..=5u32 {
        let sup = c.call(
            "supersede",
            &json!({
                "subject": "admin", "old": &old, "type_name": "device",
                "properties": {"key": "dev_053", "value": format!("v{gen}")},
                "evidence": [{"source_artifact": "probe", "method": "runtime_observation"}]
            }),
        );
        old = sup["new"].as_str().unwrap().to_string();
        if gen == 3 {
            let got = c.call("get", &json!({"subject": "admin", "koid": &old}));
            mid_ts = got["commit_ts"].as_u64().unwrap();
        }
    }
    assert!(mid_ts > 0, "gen3 commit instant must be captured");

    // The head: exactly the final generation, nothing else.
    let m = c.call(
        "aikoql",
        &json!({
            "subject": "admin", "tenant": "tenant_a",
            "query": "MATCH device WHERE key == \"dev_053\" RETURN *"
        }),
    );
    let rows = m["results"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "plain MATCH must show only the current generation: {m}"
    );
    assert_eq!(rows[0]["properties"]["value"], json!("v5"));

    // The AS_OF slice at the head agrees (T-38): one row, the successor.
    let head = c.call(
        "aikoql",
        &json!({
            "subject": "admin", "tenant": "tenant_a",
            "query": "MATCH device WHERE key == \"dev_053\" AS_OF 4102444800000 RETURN *"
        }),
    );
    let hrows = head["results"].as_array().unwrap();
    assert_eq!(
        hrows.len(),
        1,
        "AS_OF at the head must show one row, not the chain: {head}"
    );
    assert_eq!(hrows[0]["properties"]["value"], json!("v5"));

    // History preserved: at gen3's commit instant the row was v3 — and the
    // closed generations behind it stay out of that slice too.
    let hist = c.call(
        "aikoql",
        &json!({
            "subject": "admin", "tenant": "tenant_a",
            "query": format!("MATCH device WHERE key == \"dev_053\" AS_OF {mid_ts} RETURN *")
        }),
    );
    let hists = hist["results"].as_array().unwrap();
    assert_eq!(
        hists.len(),
        1,
        "AS_OF at gen3's instant must show exactly the v3 generation: {hist}"
    );
    assert_eq!(hists[0]["properties"]["value"], json!("v3"));

    let _ = std::fs::remove_dir_all(&db);
}

// T-54 (POC-3 Stage B3-2 HIGH): superseded predecessors stay visible in
// BETWEEN windows. The predecessor closes at the wall supersession instant
// (~1.79e12) while the eval's valid-time windows are event-time, so the
// stale row overlaps every later window. Acceptance: BETWEEN retires the
// superseded generation — the window after the correction returns exactly
// the corrected row — while AS_OF history stays reconstructable.
#[test]
fn between_windows_retire_superseded_generations() {
    let db = tmp_db("t54bt");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

    let first = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "device", "tenant": "tenant_a",
            "extensions": {"valid_from": 5000u64},
            "properties": {"key": "dev_b2", "value": "old"}
        }),
    );
    let old = first["koid"].as_str().unwrap().to_string();
    let gen1_ts = first["commit_ts"].as_u64().unwrap();

    let sup = c.call(
        "supersede",
        &json!({
            "subject": "admin", "old": &old, "type_name": "device",
            "extensions": {"valid_from": 5050u64},
            "properties": {"key": "dev_b2", "value": "new"},
            "evidence": [{"source_artifact": "probe", "method": "runtime_observation"}]
        }),
    );
    assert!(sup["new"].is_string(), "successor must exist: {sup}");

    // The window after the correction: exactly the corrected row. Pre-fix
    // the predecessor's wall valid_to overlaps every event-time window.
    let after = c.call(
        "aikoql",
        &json!({
            "subject": "admin", "tenant": "tenant_a",
            "query": "MATCH device WHERE key == \"dev_b2\" BETWEEN 6000 AND 9000 RETURN *"
        }),
    );
    let rows = after["results"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the window after the correction must hold only the corrected row: {after}"
    );
    assert_eq!(rows[0]["properties"]["value"], json!("new"));

    // The window before the correction event: nothing is valid there.
    let before = c.call(
        "aikoql",
        &json!({
            "subject": "admin", "tenant": "tenant_a",
            "query": "MATCH device WHERE key == \"dev_b2\" BETWEEN 0 AND 4000 RETURN *"
        }),
    );
    assert_eq!(
        before["results"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "nothing was valid before the first ingest: {before}"
    );

    // History preserved: AS_OF at the first commit instant still shows the
    // old generation (the filter is scan-level, storage is untouched).
    let past = c.call(
        "aikoql",
        &json!({
            "subject": "admin", "tenant": "tenant_a",
            "query": format!("MATCH device WHERE key == \"dev_b2\" AS_OF {gen1_ts} RETURN *")
        }),
    );
    let pasts = past["results"].as_array().unwrap();
    assert_eq!(
        pasts.len(),
        1,
        "AS_OF at the first commit must still show the old generation: {past}"
    );
    assert_eq!(pasts[0]["properties"]["value"], json!("old"));

    let _ = std::fs::remove_dir_all(&db);
}

// T-55 (POC-3 P3-009 MEDIUM): a session pinned with no tenant sees ALL
// tenants' rows — the read side fails OPEN when the client forgets the
// tenant pin. A tenant-less session must see nothing tenant-scoped; only an
// explicit admin role may read unscoped.
#[test]
fn tenantless_session_sees_nothing_scoped() {
    let db = tmp_db("t55ns");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

    // Two tenants, one row each, owned by different writers.
    let _a = c.call(
        "remember",
        &json!({
            "subject": "writer-a", "type_name": "device", "tenant": "tenant_a",
            "properties": {"key": "d1", "value": "v1"}
        }),
    );
    let _b = c.call(
        "remember",
        &json!({
            "subject": "writer-b", "type_name": "device", "tenant": "tenant_b",
            "properties": {"key": "d2", "value": "v2"}
        }),
    );

    // An unscoped session (no tenant pin) must fail closed: 0 rows, not
    // the cross-tenant head (POC F3 shape).
    let _init = c.session_init_unscoped("plain-agent");
    let m = c.call(
        "aikoql",
        &json!({
            "subject": "plain-agent",
            "query": "MATCH device RETURN *"
        }),
    );
    assert_eq!(
        m["results"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "tenant-less session must see nothing tenant-scoped: {m}"
    );

    // Even an unscoped OWNER stays confined — ownership does not bypass the
    // tenant pin; the pin is the only door for an unscoped principal.
    let _init = c.session_init_unscoped("writer-a");
    let m = c.call(
        "aikoql",
        &json!({
            "subject": "writer-a",
            "query": "MATCH device RETURN *"
        }),
    );
    assert_eq!(
        m["results"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "unscoped owner must not read own tenant-scoped rows: {m}"
    );

    // The explicit global channel still works: an admin-role unscoped
    // subject reads across tenants.
    let _init = c.session_init_unscoped_admin("global-admin");
    let m = c.call(
        "aikoql",
        &json!({
            "subject": "global-admin",
            "query": "MATCH device RETURN *"
        }),
    );
    assert_eq!(
        m["results"].as_array().map(|a| a.len()).unwrap_or(0),
        2,
        "admin-role unscoped subject is the explicit global read channel: {m}"
    );

    let _ = std::fs::remove_dir_all(&db);
}

// T-55 (POC-3 P3-009 P2c LOW): `tenant: ""` is accepted silently and the row
// lands in an invisible "" namespace. The tool boundary must reject a
// present-but-empty tenant instead of storing a black-hole row.
#[test]
fn remember_rejects_empty_tenant() {
    let db = tmp_db("t55et");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

    let r = c.call(
        "remember",
        &json!({
            "subject": "admin", "type_name": "device", "tenant": "",
            "properties": {"key": "d1", "value": "v1"}
        }),
    );
    assert_eq!(
        r["ok"],
        json!(false),
        "empty tenant must be rejected at the boundary: {r}"
    );
    assert_eq!(
        r["error"]["code"],
        json!("VALIDATION_ERROR"),
        "empty tenant rejection carries a validation code: {r}"
    );

    // And the same shape on the session pin.
    let i = c.session_init_with("admin", "");
    assert_eq!(i["error"]["code"], json!(-32602));

    let _ = std::fs::remove_dir_all(&db);
}

// T-55 (POC-3 P3-009 P5 LOW): ACL denials surface as INTERNAL with an
// "unexpected error" suggestion — clients cannot tell a permission denial
// from a server fault. A cross-tenant denial must carry ACCESS_DENIED,
// retryable=false, and an access-oriented suggestion.
#[test]
fn foreign_access_denied_is_not_internal() {
    let db = tmp_db("t55ad");
    let _ = std::fs::remove_dir_all(&db);
    let mut c = McpClient::start(&db);

    let note = c.call(
        "remember",
        &json!({
            "subject": "writer-a", "type_name": "device", "tenant": "tenant_a",
            "properties": {"key": "d1", "value": "v1"}
        }),
    );
    let koid = note["koid"].as_str().unwrap().to_string();

    // Foreign session: tenant_b cannot read tenant_a's object.
    let _init = c.session_init("reader-b", "tenant_b");
    for tool in ["get", "explain", "prove", "trace"] {
        let args = match tool {
            "explain" => json!({"subject": "reader-b", "koid": &koid}),
            "prove" => json!({"subject": "reader-b", "koid": &koid}),
            "trace" => json!({"subject": "reader-b", "koid": &koid}),
            _ => json!({"subject": "reader-b", "koid": &koid}),
        };
        let r = c.call(tool, &args);
        assert_eq!(r["ok"], json!(false), "foreign {tool} must be denied: {r}");
        assert_eq!(
            r["error"]["code"],
            json!("ACCESS_DENIED"),
            "denial must classify ACCESS_DENIED, not INTERNAL ({tool}): {r}"
        );
        assert_eq!(
            r["error"]["retryable"],
            json!(false),
            "a permission denial is not retryable ({tool}): {r}"
        );
    }

    let _ = std::fs::remove_dir_all(&db);
}
