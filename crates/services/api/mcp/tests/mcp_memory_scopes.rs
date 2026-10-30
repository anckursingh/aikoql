//! AG-11 — memory scopes, enforced where reads are enforced (the kernel):
//! `private` (default) — only the writer; `shared` — everyone in the
//! writer's tenant (an ACL entry for all principals, inside tenant
//! confinement); `session` — the writer, from the same session (run_id)
//! only. Promotion is `memory_update {scope}`: a new version, never a
//! silent ACL edit. Only the owner writes a memory.

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("memscope-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

// shortcut: another copy of the private stdio client (debt ledger:
// connectors/mod.rs:457), extract a shared one when a seventh appears
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
    fn serve(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
            .arg("serve")
            .arg(root.join("db"))
            .env("AIKOQL_MEMORY_DIR", root.join("memory"))
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

    /// A tool call as `who` (stdio trusts the caller's subject + tenant).
    fn tool(&mut self, who: &str, tenant: &str, name: &str, args: J) -> J {
        let mut args = args;
        args["subject"] = json!(who);
        args["tenant"] = json!(tenant);
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: {r}"));
        serde_json::from_str(text).unwrap_or_else(|_| json!(text))
    }

    fn store(
        &mut self,
        who: &str,
        tenant: &str,
        name: &str,
        content: &str,
        scope: Option<&str>,
    ) -> J {
        let mut args = json!({"name": name, "description": name, "content": content});
        if let Some(s) = scope {
            args["scope"] = json!(s);
        }
        self.tool(who, tenant, "memory_store", args)
    }

    fn search(&mut self, who: &str, tenant: &str, query: &str) -> Vec<String> {
        let r = self.tool(who, tenant, "memory_search", json!({"query": query}));
        r["results"]
            .as_array()
            .unwrap_or_else(|| panic!("memory_search: {r}"))
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn recalls(&mut self, who: &str, tenant: &str, query: &str) -> Vec<String> {
        let r = self.tool(
            who,
            tenant,
            "recall",
            json!({"query": query, "wait_for_freshness_ms": 5000}),
        );
        r["items"]
            .as_array()
            .unwrap_or_else(|| panic!("recall: {r}"))
            .iter()
            .map(|x| x["koid"].as_str().unwrap().to_string())
            .collect()
    }

    fn session(&mut self, run: &str) {
        let r = self.call(
            "tools/call",
            json!({"name": "session_init", "arguments": {"run_id": run}}),
        );
        assert!(r.get("error").is_none(), "session_init: {r}");
    }
}

#[test]
fn s01_a_private_memory_reaches_no_one_else_by_any_read() {
    let root = tmp("private");
    let mut m = Mcp::serve(&root);
    let koid = m.store(
        "alice",
        "acme",
        "alice-pin",
        "alice's locker code is 4417",
        None,
    )["koid"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        m.search("alice", "acme", "locker code"),
        vec!["alice-pin"],
        "the writer finds it"
    );
    assert!(
        m.search("bob", "acme", "locker code").is_empty(),
        "memory_search"
    );
    assert!(
        !m.recalls("bob", "acme", "locker code").contains(&koid),
        "recall"
    );
    let got = m.tool("bob", "acme", "get", json!({"koid": koid}));
    assert!(!got.to_string().contains("4417"), "get: {got}");
    let q = m.tool(
        "bob",
        "acme",
        "aikoql",
        json!({"query": "MATCH Memory RETURN content"}),
    );
    assert!(!q.to_string().contains("4417"), "aikoql: {q}");
}

#[test]
fn s02_a_shared_memory_is_readable_by_the_tenant_but_not_writable() {
    let root = tmp("shared");
    let mut m = Mcp::serve(&root);
    let stored = m.store(
        "alice",
        "acme",
        "wifi",
        "the office wifi password is tulip-42",
        Some("shared"),
    );
    assert_eq!(stored["scope"], json!("shared"), "{stored}");
    assert_eq!(
        m.search("bob", "acme", "wifi password"),
        vec!["wifi"],
        "bob reads a shared memory"
    );
    assert!(
        !m.recalls("bob", "acme", "wifi password").is_empty(),
        "recall too"
    );
    let u = m.tool(
        "bob",
        "acme",
        "memory_update",
        json!({"name": "wifi", "content": "hijacked"}),
    );
    assert!(
        u.to_string().contains("error") || u.to_string().contains("not"),
        "bob cannot edit it: {u}"
    );
    let r = m.tool(
        "alice",
        "acme",
        "memory_search",
        json!({"query": "wifi password"}),
    );
    assert!(
        r["results"][0]["snippet"]
            .as_str()
            .is_some_and(|s| s.contains("tulip-42")),
        "unchanged: {r}"
    );
}

#[test]
fn s03_sharing_stops_at_the_tenant() {
    let root = tmp("tenant");
    let mut m = Mcp::serve(&root);
    m.store(
        "alice",
        "acme",
        "roadmap",
        "acme ships the new dashboard in May",
        Some("shared"),
    );
    assert!(
        m.search("carol", "globex", "dashboard").is_empty(),
        "another tenant never sees it"
    );
    assert_eq!(m.search("bob", "acme", "dashboard"), vec!["roadmap"]);
}

#[test]
fn s04_a_session_memory_stays_in_its_session() {
    let root = tmp("session");
    let mut m = Mcp::serve(&root);
    m.session("run-1");
    m.store(
        "alice",
        "acme",
        "scratch",
        "draft numbers for the q3 forecast",
        Some("session"),
    );
    assert_eq!(
        m.search("alice", "acme", "q3 forecast"),
        vec!["scratch"],
        "its own session"
    );
    m.session("run-2");
    assert!(
        m.search("alice", "acme", "q3 forecast").is_empty(),
        "another session of the same writer"
    );
    m.session("run-1");
    assert_eq!(
        m.search("alice", "acme", "q3 forecast"),
        vec!["scratch"],
        "back in run-1"
    );
    assert!(
        m.search("bob", "acme", "q3 forecast").is_empty(),
        "never another principal"
    );
}

#[test]
fn s05_promotion_is_a_new_version() {
    let root = tmp("promote");
    let mut m = Mcp::serve(&root);
    let koid = m.store(
        "alice",
        "acme",
        "oncall",
        "the escalation number is 555-0100",
        None,
    )["koid"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(m.search("bob", "acme", "escalation number").is_empty());
    let u = m.tool(
        "alice",
        "acme",
        "memory_update",
        json!({"name": "oncall", "scope": "shared"}),
    );
    assert_eq!(u["koid"], json!(koid), "{u}");
    assert_eq!(u["version"], json!(2), "{u}");
    assert_eq!(u["scope"], json!("shared"));
    assert_eq!(
        m.search("bob", "acme", "escalation number"),
        vec!["oncall"],
        "promoted"
    );
}
