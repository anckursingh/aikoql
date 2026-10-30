//! AG-10 — one memory store. The memory_* tools used to keep Markdown files
//! outside the knowledge base: no versions, no lineage, no recall. Now each
//! memory is a `Memory` KO (the source of truth: versioned, recallable,
//! traceable); the files stay as a view, written as before, and an existing
//! directory is imported once at start (idempotently).

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("memstore-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

// shortcut: another copy of the private stdio client (debt ledger:
// connectors/mod.rs:457), extract a shared one when a sixth appears
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
    /// A server on `root/db` with its memory directory at `root/memory`.
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

    fn recall(&mut self, query: &str) -> Vec<J> {
        let r = self.tool(
            "recall",
            json!({"query": query, "wait_for_freshness_ms": 5000}),
        );
        r["items"]
            .as_array()
            .unwrap_or_else(|| panic!("recall: {r}"))
            .clone()
    }

    fn store(&mut self, name: &str, description: &str, content: &str) -> J {
        self.tool(
            "memory_store",
            json!({"name": name, "description": description, "content": content}),
        )
    }
}

fn memories(items: &[J]) -> Vec<&J> {
    items
        .iter()
        .filter(|i| i["type_name"] == "Memory")
        .collect()
}

#[test]
fn m01_a_stored_memory_is_a_recallable_knowledge_object_and_still_a_file() {
    let root = tmp("store");
    let mut m = Mcp::serve(&root);
    let r = m.store(
        "deploy-freeze",
        "deploys freeze in late December",
        "No deploys during the last week of December.",
    );
    let koid = r["koid"]
        .as_str()
        .unwrap_or_else(|| panic!("memory_store returns its KOID: {r}"))
        .to_string();
    let hits = m.recall("deploy freeze December");
    let hit = memories(&hits).into_iter().find(|i| i["koid"] == koid);
    assert!(hit.is_some(), "recall finds the memory: {hits:?}");
    let trace = m.tool("trace", json!({"koid": koid}));
    assert!(
        trace.to_string().contains(&koid),
        "the memory has a trace: {trace}"
    );
    // the file view is unchanged
    let file = std::fs::read_to_string(root.join("memory/deploy-freeze.md")).unwrap();
    assert!(file.contains("No deploys during the last week of December."));
    let index = std::fs::read_to_string(root.join("memory/MEMORY.md")).unwrap();
    assert!(index.contains("deploy-freeze.md"), "{index}");
}

#[test]
fn m02_an_update_is_a_new_version_of_the_same_object() {
    let root = tmp("update");
    let mut m = Mcp::serve(&root);
    let koid = m.store("office", "where the office is", "The office is in Berlin.")["koid"]
        .as_str()
        .unwrap()
        .to_string();
    let u = m.tool(
        "memory_update",
        json!({"name": "office", "content": "The office is in Lisbon."}),
    );
    assert_eq!(u["koid"], json!(koid), "same object: {u}");
    assert_eq!(u["version"], json!(2), "{u}");
    let got = m.tool("get", json!({"koid": koid}));
    assert!(got.to_string().contains("Lisbon"), "{got}");
}

#[test]
fn m03_a_deleted_memory_is_gone_everywhere() {
    let root = tmp("delete");
    let mut m = Mcp::serve(&root);
    let koid = m.store(
        "old-vpn",
        "the old VPN host",
        "The VPN host is vpn1.example.",
    )["koid"]
        .as_str()
        .unwrap()
        .to_string();
    m.tool("memory_delete", json!({"name": "old-vpn"}));
    assert!(!root.join("memory/old-vpn.md").exists());
    assert!(
        memories(&m.recall("VPN host"))
            .iter()
            .all(|i| i["koid"] != koid),
        "recall still returns it"
    );
    let s = m.tool("memory_search", json!({"query": "VPN host"}));
    assert_eq!(s["count"], json!(0), "{s}");
}

#[test]
fn m04_an_existing_directory_is_imported_once() {
    let root = tmp("import");
    std::fs::create_dir_all(root.join("memory")).unwrap();
    std::fs::write(
        root.join("memory/team-lead.md"),
        "---\nname: team-lead\ndescription: \"who leads the team\"\nmetadata:\n  type: user\n---\n\nPriya leads the platform team.\n",
    )
    .unwrap();
    let first = {
        let mut m = Mcp::serve(&root);
        let hits = m.recall("platform team lead");
        let mem = memories(&hits);
        assert_eq!(mem.len(), 1, "the file is imported: {hits:?}");
        mem[0].clone()
    };
    let mut m = Mcp::serve(&root);
    let again = m.recall("platform team lead");
    let mem = memories(&again);
    assert_eq!(mem.len(), 1, "a restart imports nothing twice: {again:?}");
    assert_eq!(mem[0]["koid"], first["koid"]);
    assert_eq!(
        mem[0]["version"],
        json!(1),
        "an unchanged file is not a new version"
    );
}

#[test]
fn m05_memory_search_answers_from_the_knowledge_base_in_its_old_shape() {
    let root = tmp("search");
    let mut m = Mcp::serve(&root);
    m.store(
        "billing-owner",
        "who owns billing",
        "Billing is owned by the payments team.",
    );
    // the file is only a view: the knowledge base still has the memory
    std::fs::remove_file(root.join("memory/billing-owner.md")).unwrap();
    let s = m.tool(
        "memory_search",
        json!({"query": "billing owner", "max_results": 5}),
    );
    let r = &s["results"][0];
    assert_eq!(r["name"], json!("billing-owner"), "{s}");
    assert_eq!(r["description"], json!("who owns billing"));
    assert_eq!(r["file"], json!("billing-owner.md"));
    assert!(
        r["snippet"]
            .as_str()
            .is_some_and(|t| t.contains("payments team")),
        "{s}"
    );
    assert!(r["score"].as_f64().is_some_and(|v| v > 0.0), "{s}");
}

/// The old memory_update looked for a second `---` after the closing
/// delimiter to find the body, found none, and wrote an empty body: an
/// update of only the description wiped the memory.
#[test]
fn m06_updating_only_the_description_keeps_the_body() {
    let root = tmp("desc-only");
    let mut m = Mcp::serve(&root);
    m.store(
        "standup",
        "when standup is",
        "Standup is at 9:30 every weekday.",
    );
    m.tool(
        "memory_update",
        json!({"name": "standup", "description": "the daily standup time"}),
    );
    let file = std::fs::read_to_string(root.join("memory/standup.md")).unwrap();
    assert!(
        file.contains("Standup is at 9:30 every weekday."),
        "the body survived: {file}"
    );
    assert!(file.contains("the daily standup time"), "{file}");
    let s = m.tool("memory_search", json!({"query": "standup"}));
    assert!(
        s["results"][0]["snippet"]
            .as_str()
            .is_some_and(|t| t.contains("9:30")),
        "{s}"
    );
}
